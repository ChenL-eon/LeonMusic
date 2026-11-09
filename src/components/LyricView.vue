<script setup lang="ts">
/**
 * 歌词视图：普通 LRC 与逐字 LRC 通用。
 *
 * 逐字高亮实现：每个字一个 `<span>`，用 CSS 变量 `--p` 控制
 * `background-clip: text` 的填充比例。只在「当前行」上做，其余行静态渲染。
 *
 * `position` 由父组件传入（播放器 store 的插值位置），本组件不自己计时。
 *
 * 滚动（2026-11-09 重做）：不再用 `overflow:hidden` + translateY 硬跟随，
 * 改成真正的滚动容器 —— 自动追踪用 scrollTo 平滑滚动，用户可滚轮 / 拖拽 /
 * 触摸自由浏览；用户一动手就暂停跟随，停手 4s 或点「回到当前」后恢复。
 * 旧写法只要鼠标划进歌词区就永久停跟随，且 overflow:hidden 根本滚不动。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";

import type { LyricLine } from "@/types";

const props = withDefaults(
  defineProps<{
    lines: LyricLine[];
    currentIndex: number;
    position: number;
    showTranslation?: boolean;
    compact?: boolean;
    align?: "left" | "center";
    /** 沉浸模式：模糊封面背景上渲染，文字用纯白（当前行）+ 白色低透明（其余），
     *  行距加大。参考主流播放器「播放页歌词」样式。 */
    immersive?: boolean;
  }>(),
  { showTranslation: true, compact: false, align: "center", immersive: false },
);

const emit = defineEmits<{ (e: "seek", ms: number): void }>();

const viewport = ref<HTMLElement | null>(null);
const lineEls = ref<HTMLElement[]>([]);

/** 是否跟随当前行。用户主动滚动后置 false，停手一段时间或点按钮恢复 */
const follow = ref(true);
/** 鼠标拖拽滚动中（只用于 CSS 关掉文字选择） */
const dragging = ref(false);

const hasLyrics = computed(() => props.lines.length > 0);

/** 停手多久后自动恢复跟随 */
const RESUME_DELAY = 4000;

let resumeTimer: number | undefined;
let dragStartY = 0;
let dragStartTop = 0;
let suppressClick = false;

function wordProgress(start: number, end: number): number {
  if (end <= start) return props.position >= start ? 1 : 0;
  return Math.max(0, Math.min(1, (props.position - start) / (end - start)));
}

function setLineRef(el: Element | null, i: number) {
  if (el) lineEls.value[i] = el as HTMLElement;
}

/** 把当前行滚到视口 38% 处（主流播放器的视觉重心） */
function scrollToActive(behavior: ScrollBehavior = "smooth") {
  const vp = viewport.value;
  if (!vp) return;
  const el = lineEls.value[props.currentIndex];
  if (!el) {
    vp.scrollTo({ top: 0, behavior });
    return;
  }
  // 用 rect 差值算，避免 offsetParent 随布局变化算错
  const delta = el.getBoundingClientRect().top - vp.getBoundingClientRect().top;
  const top = vp.scrollTop + delta - vp.clientHeight * 0.38 + el.clientHeight / 2;
  vp.scrollTo({ top: Math.max(0, top), behavior });
}

function clearResume() {
  if (resumeTimer !== undefined) {
    window.clearTimeout(resumeTimer);
    resumeTimer = undefined;
  }
}

/** 用户开始自己滚：暂停跟随，停手 4s 后自动追上当前行 */
function pauseFollow() {
  follow.value = false;
  clearResume();
  resumeTimer = window.setTimeout(() => resumeFollow(), RESUME_DELAY);
}

function resumeFollow() {
  clearResume();
  follow.value = true;
  scrollToActive();
}

function pick(i: number) {
  // 拖拽结束浏览器会补一个 click，吞掉它
  if (suppressClick) {
    suppressClick = false;
    return;
  }
  const line = props.lines[i];
  if (!line) return;
  // 点某行 = 想听这句，顺手恢复跟随（currentIndex 变化后 watch 会滚过去）
  follow.value = true;
  clearResume();
  emit("seek", line.startMs);
}

/* ---------------- 鼠标拖拽滚动（滚轮/触摸已原生支持） ---------------- */

function onPointerDown(e: PointerEvent) {
  if (e.pointerType !== "mouse" || e.button !== 0) return;
  const vp = viewport.value;
  if (!vp) return;
  suppressClick = false;
  dragStartY = e.clientY;
  dragStartTop = vp.scrollTop;
  try {
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  } catch {
    /* 指针已失效，忽略 */
  }
}

function onPointerMove(e: PointerEvent) {
  if (!viewport.value || e.pointerType !== "mouse" || e.buttons !== 1) return;
  const dy = e.clientY - dragStartY;
  if (!dragging.value) {
    if (Math.abs(dy) < 5) return; // 小抖动仍算点击
    dragging.value = true;
    pauseFollow();
  }
  e.preventDefault();
  viewport.value.scrollTop = dragStartTop - dy;
}

function endDrag(e: PointerEvent) {
  try {
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
  } catch {
    /* 已经释放过 */
  }
  if (!dragging.value) return;
  dragging.value = false;
  // 松手后重新计时，避免「拖得久一点，刚松手就被自动拉走」
  pauseFollow();
  suppressClick = true;
}

/* ---------------- 跟随逻辑 ---------------- */

watch(
  () => props.currentIndex,
  async () => {
    if (!follow.value) return;
    await nextTick();
    scrollToActive();
  },
);

// 换歌 / 歌词重新加载：重置跟随，并从新位置直接跳过去（不要从上一首滚过去）
watch(
  () => props.lines,
  async () => {
    clearResume();
    follow.value = true;
    await nextTick();
    scrollToActive("auto");
  },
);

function onResize() {
  if (follow.value) scrollToActive("auto");
}

onMounted(async () => {
  // 打开播放页时歌词可能已经加载好（watch immediate 时还拿不到 DOM）
  await nextTick();
  scrollToActive("auto");
  window.addEventListener("resize", onResize);
});

onBeforeUnmount(() => {
  clearResume();
  window.removeEventListener("resize", onResize);
});
</script>

<template>
  <div class="lyrics-shell" :class="{ immersive, compact }">
    <div
      ref="viewport"
      class="lyrics scroll"
      :class="[compact ? 'compact' : '', immersive ? 'immersive' : '', `align-${align}`, { dragging }]"
      @wheel.passive="pauseFollow"
      @touchstart.passive="pauseFollow"
      @pointerdown="onPointerDown"
      @pointermove="onPointerMove"
      @pointerup="endDrag"
      @pointercancel="endDrag"
    >
      <div v-if="!hasLyrics" class="empty">
        <div class="big">♪</div>
        <div>暂无歌词</div>
      </div>

      <div v-else class="track">
        <div
          v-for="(line, i) in lines"
          :key="i"
          :ref="(el) => setLineRef(el as Element | null, i)"
          class="line"
          :class="{ active: i === currentIndex, near: Math.abs(i - currentIndex) === 1 }"
          @click="pick(i)"
        >
          <div v-if="line.words.length" class="text words">
            <span
              v-for="(w, wi) in line.words"
              :key="wi"
              class="word"
              :class="{ on: i === currentIndex }"
              :style="{
                '--p': i === currentIndex ? wordProgress(w.startMs, w.endMs) : i < currentIndex ? 1 : 0,
              }"
              >{{ w.text }}</span
            >
          </div>
          <div v-else class="text">{{ line.text }}</div>

          <div v-if="showTranslation && line.translation" class="translation">
            {{ line.translation }}
          </div>
        </div>
      </div>
    </div>

    <!-- 用户滚走之后给一个「拉回来」的出口，不用等 4 秒自动归位 -->
    <transition name="fade">
      <button
        v-if="hasLyrics && !follow"
        class="back-now"
        aria-label="回到当前播放的歌词"
        @click="resumeFollow"
      >
        <svg viewBox="0 0 12 12" width="10" height="10" aria-hidden="true">
          <path
            d="M2 4.2 6 8l4-3.8"
            fill="none"
            stroke="currentColor"
            stroke-width="1.6"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
        回到当前
      </button>
    </transition>
  </div>
</template>

<style scoped>
.lyrics-shell {
  position: relative;
  height: 100%;
  min-height: 0;
}

/* 真正的滚动容器：滚动条隐藏，靠遮罩做上下淡出 */
.lyrics {
  position: relative;
  height: 100%;
  overflow-x: hidden;
  overflow-y: auto;
  overscroll-behavior: contain;
  scrollbar-width: none;
  -ms-overflow-style: none;
  touch-action: pan-y;
  mask-image: linear-gradient(
    to bottom,
    transparent 0%,
    #000 12%,
    #000 82%,
    transparent 100%
  );
}
.lyrics::-webkit-scrollbar {
  width: 0;
  height: 0;
}
.lyrics.dragging {
  user-select: none;
}

.track {
  padding: 42% 0;
}

/* 没有歌词时 .empty 只有内容高度，会贴在区域顶部，看着像「上面空了一块」 */
.lyrics .empty {
  height: 100%;
  padding: 0 16px;
}

.line {
  padding: 10px 18px;
  cursor: pointer;
  transition: opacity 0.3s, filter 0.3s;
  opacity: 0.36;
  filter: blur(0.2px);
}
.line.near {
  opacity: 0.55;
  filter: none;
}
.line.active {
  opacity: 1;
  filter: none;
}
.align-center .line {
  text-align: center;
}
.align-left .line {
  text-align: left;
}

.text {
  font-size: var(--lyric-size);
  font-weight: 700;
  letter-spacing: -0.01em;
  line-height: 1.35;
  color: var(--text);
}
.line.active .text {
  color: var(--accent);
  text-shadow: 0 0 32px color-mix(in srgb, var(--accent) 40%, transparent);
}

.words {
  display: inline;
}
.word {
  display: inline-block;
  white-space: pre;
}

/* 逐字：当前行用渐变裁切做「已唱/未唱」的分界 */
.line.active .word.on {
  background-image: linear-gradient(
    to right,
    var(--accent) calc(var(--p) * 100%),
    var(--text-3) calc(var(--p) * 100%)
  );
  -webkit-background-clip: text;
  background-clip: text;
  color: transparent;
  text-shadow: none;
  transition: none;
}

.translation {
  font-size: calc(var(--lyric-size) * 0.46);
  font-weight: 400;
  color: var(--text-3);
  margin-top: 6px;
}

.compact .text {
  font-size: 15px;
  font-weight: 600;
}
.compact .translation {
  font-size: 12px;
}
.compact .line {
  padding: 6px 12px;
}
.compact .track {
  padding: 40% 0;
}

/* 「回到当前」：浮在右下角，不占歌词行宽 */
.back-now {
  position: absolute;
  right: 12px;
  bottom: 14px;
  z-index: 2;
  display: inline-flex;
  align-items: center;
  gap: 4px;
  height: 28px;
  padding: 0 11px 0 9px;
  border-radius: 999px;
  font-size: 11.5px;
  font-weight: 600;
  color: var(--text-2);
  background: color-mix(in srgb, var(--panel) 88%, transparent);
  border: 1px solid var(--border);
  backdrop-filter: blur(12px);
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.18);
}
.back-now:hover {
  color: var(--text);
  background: var(--hover);
}

/*
 * 沉浸模式（播放页 + 封面模糊背景）：参考主流播放器样式——
 * 当前行纯白加大，其余行白色低透明，行距拉开；
 * 高亮不用 accent 色（彩色字在照片背景上发脏），靠大小和透明度区分。
 */
.lyrics.immersive .line {
  padding: 20px 18px;
  opacity: 0.32;
  filter: none;
}
.lyrics.immersive .line.near {
  opacity: 0.42;
}
.lyrics.immersive .line.active {
  opacity: 1;
}
.lyrics.immersive .text {
  color: #fff;
  text-shadow: 0 1px 14px rgba(0, 0, 0, 0.28);
}
.lyrics.immersive .line.active .text {
  color: #fff;
  text-shadow: 0 2px 22px rgba(0, 0, 0, 0.35);
}
/* 逐字行已在 active 上做了渐变裁切，别让白色 text-shadow 叠在字缝里 */
.lyrics.immersive .line.active .word.on {
  text-shadow: none;
}
.lyrics.immersive .translation {
  color: rgba(255, 255, 255, 0.55);
}
/* 沉浸模式下空白歌词提示也要可读 */
.lyrics.immersive .empty {
  color: rgba(255, 255, 255, 0.7);
}
.lyrics-shell.immersive .back-now {
  color: rgba(255, 255, 255, 0.92);
  background: rgba(0, 0, 0, 0.32);
  border-color: rgba(255, 255, 255, 0.18);
}
.lyrics-shell.immersive .back-now:hover {
  background: rgba(0, 0, 0, 0.48);
}

@media (max-width: 860px) {
  /* 字号由 main.css 的 --lyric-size 统一给（手机上封顶 24px），
     这里只管呼吸空间 —— 行距太大会导致可见行数不足 4 行。 */
  .line {
    padding: 9px 16px;
  }
  .track {
    padding: 36% 0;
  }
  .translation {
    font-size: calc(var(--lyric-size) * 0.58);
    margin-top: 4px;
  }
  /* 触摸下没有 hover，长按拖动的手感靠更宽松的点击区 */
  .lyrics {
    mask-image: linear-gradient(to bottom, transparent 0%, #000 10%, #000 78%, transparent 100%);
  }
}
</style>
