<script setup lang="ts">
/**
 * 歌词视图：普通 LRC 与逐字 LRC 通用。
 *
 * 逐字高亮实现：每个字一个 `<span>`，用 CSS 变量 `--p` 控制
 * `background-clip: text` 的填充比例。只在「当前行」上做，其余行静态渲染。
 *
 * `position` 由父组件传入（播放器 store 的插值位置），本组件不自己计时。
 */
import { computed, nextTick, ref, watch } from "vue";

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
const offset = ref(0);
const dragging = ref(false);

const hasLyrics = computed(() => props.lines.length > 0);

function wordProgress(start: number, end: number): number {
  if (end <= start) return props.position >= start ? 1 : 0;
  return Math.max(0, Math.min(1, (props.position - start) / (end - start)));
}

function setLineRef(el: Element | null, i: number) {
  if (el) lineEls.value[i] = el as HTMLElement;
}

watch(
  () => props.currentIndex,
  async () => {
    if (dragging.value) return;
    await nextTick();
    const el = lineEls.value[props.currentIndex];
    const vp = viewport.value;
    if (!el || !vp) return;
    const target = el.offsetTop - vp.clientHeight * 0.38 + el.clientHeight / 2;
    offset.value = Math.max(0, target);
  },
  { immediate: true },
);

function pick(i: number) {
  const line = props.lines[i];
  if (line) emit("seek", line.startMs);
}
</script>

<template>
  <div
    ref="viewport"
    class="lyrics scroll"
    :class="{ compact, immersive, [`align-${align}`]: true }"
    @mouseenter="dragging = true"
    @mouseleave="dragging = false"
  >
    <div v-if="!hasLyrics" class="empty">
      <div class="big">♪</div>
      <div>暂无歌词</div>
    </div>

    <div v-else class="track" :style="{ transform: `translateY(${-offset}px)` }">
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
</template>

<style scoped>
.lyrics {
  position: relative;
  height: 100%;
  overflow: hidden;
  mask-image: linear-gradient(
    to bottom,
    transparent 0%,
    #000 12%,
    #000 82%,
    transparent 100%
  );
}
.track {
  transition: transform 0.55s var(--ease);
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
