<script setup lang="ts">
/**
 * 桌面悬浮歌词（独立 WebView 窗口）。
 *
 * 与主界面共享同一份 Rust 状态推送：这里也调 `player.init()`，
 * 订阅同一组事件。窗口只是「另一个渲染器」，不存在第二套播放逻辑。
 *
 * 交互（对照酷狗式桌面歌词）：
 * - 整窗拖动（`data-tauri-drag-region`）
 * - 鼠标悬停浮出工具条：置顶 / 锁定 / A- 字号 A+ / 上一首 / 播放暂停 / 下一首 / 关闭
 * - 锁定后整窗鼠标穿透（点击落到达 underlying 应用），只能到 设置 → 桌面歌词 解除
 * - 双击切换「双行 / 单行」；右键关闭
 * - 排版方向 / 对齐 / 颜色 全部来自设置页，跨窗口经 settings 事件实时同步
 */
import { computed, onMounted, ref, watch } from "vue";
import { storeToRefs } from "pinia";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { emit as emitGlobal } from "@tauri-apps/api/event";
import { LogicalSize } from "@tauri-apps/api/dpi";

import Icon from "@/components/Icon.vue";
import { LYRIC_VIS_EVENT } from "@/api/ipc";
import { usePlayerStore } from "@/stores/player";
import { useSettingsStore } from "@/stores/settings";
import type { Settings } from "@/types";
import "@/styles/main.css";

const player = usePlayerStore();
const settingsStore = useSettingsStore();
const { snap, lyrics, currentLineIndex, displayPosition } = storeToRefs(player);
const s = computed(() => settingsStore.value);
const win = getCurrentWindow();

const hover = ref(false);

const current = computed(() => lyrics.value.lines[currentLineIndex.value] ?? null);
const prevLine = computed(() => lyrics.value.lines[currentLineIndex.value - 1] ?? null);
const nextLine = computed(() => lyrics.value.lines[currentLineIndex.value + 1] ?? null);
/** 翻译行只在单行模式（且主界面开了显示翻译）出现——双行模式被它占位 */
const showTrans = computed(
  () => !s.value.dlDualLine && !!current.value?.translation && settingsStore.value.lyricShowTranslation,
);

function wordProgress(start: number, end: number): number {
  if (end <= start) return displayPosition.value >= start ? 1 : 0;
  return Math.max(0, Math.min(1, (displayPosition.value - start) / (end - start)));
}

/** #RGB/#RGBA/#RRGGBB/#RRGGBBAA → rgba() 字符串；解析失败回退白色（WebView2 无 color-mix） */
function rgba(hex: string, alpha: number): string {
  let h = (hex || "").trim().replace(/^#/, "");
  if (h.length === 3 || h.length === 4) h = [...h].map((c) => c + c).join("");
  if (h.length !== 6 && h.length !== 8) return `rgba(255, 255, 255, ${alpha})`;
  const r = parseInt(h.slice(0, 2), 16);
  const g = parseInt(h.slice(2, 4), 16);
  const b = parseInt(h.slice(4, 6), 16);
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

/** 卡拉OK渐变双色 + 副行颜色，全部从设置推导，主题模式回落 accent */
const stackVars = computed(() => {
  const custom = s.value.dlColorMode === "custom";
  return {
    "--dl-size": `${s.value.dlFontSize}px`,
    "--dl-side": `${Math.max(12, Math.round(s.value.dlFontSize * 0.42))}px`,
    "--dl-trans-size": `${Math.max(11, Math.round(s.value.dlFontSize * 0.38))}px`,
    "--dl-sung": custom ? s.value.dlColorLyric || "#ffffff" : "var(--accent-2)",
    "--dl-unsung": custom
      ? rgba(s.value.dlColorLyric || "#ffffff", 0.38)
      : "rgba(255, 255, 255, 0.92)",
    "--dl-plain": custom ? s.value.dlColorLyric || "#ffffff" : "#ffffff",
    "--dl-side-color": custom
      ? rgba(s.value.dlColorTrans || "#ffffff", 0.55)
      : "rgba(255, 255, 255, 0.45)",
    "--dl-trans-color": custom
      ? rgba(s.value.dlColorTrans || "#ffffff", 0.78)
      : "rgba(255, 255, 255, 0.7)",
  };
});

const rootClass = computed(() => [
  s.value.dlDirection === "vertical" ? "vertical" : "horizontal",
  `align-${s.value.dlAlign}`,
]);

// ---- 窗口行为随设置走（锁定 = 鼠标穿透；置顶跟随设置）----

watch(
  () => s.value.dlLocked,
  (locked) => {
    if (locked) hover.value = false;
    void win.setIgnoreCursorEvents(locked).catch(() => {
      /* 窗口可能正在销毁，忽略 */
    });
  },
  { immediate: true },
);

watch(
  () => s.value.dlAlwaysOnTop,
  (on) => {
    void win.setAlwaysOnTop(on).catch(() => {});
  },
  { immediate: true },
);

// 排版方向决定窗口形状：横向矮宽、竖向高窄。
// 竖排歌词在 860x170 的默认窗里一列只能放三四个字，必须跟着换形状。
async function applyShape(dir: string) {
  try {
    await win.setSize(dir === "vertical" ? new LogicalSize(320, 620) : new LogicalSize(860, 170));
  } catch {
    /* 窗口可能正在销毁 */
  }
}

watch(
  () => s.value.dlDirection,
  (dir) => {
    void applyShape(dir);
  },
);

// ---- 工具条动作 ----

async function save(patch: Partial<Settings>) {
  await settingsStore.save(patch);
}

// 歌词窗强制深色：settingsStore 内部 applyTheme 会按主题重写 data-theme
// （主窗口语义），无论是本窗保存还是主窗保存经事件同步过来，都要拨回深色。
watch(s, () => {
  document.documentElement.dataset.theme = "dark";
});

function togglePin() {
  void save({ dlAlwaysOnTop: !s.value.dlAlwaysOnTop });
}

/** 立即锁定：穿透后本窗口收不到任何鼠标事件，解除只能去设置页 */
function lockNow() {
  hover.value = false;
  void save({ dlLocked: true });
}

function stepSize(delta: number) {
  const v = Math.max(16, Math.min(72, s.value.dlFontSize + delta));
  if (v !== s.value.dlFontSize) void save({ dlFontSize: v });
}

function closeWin() {
  // 广播给主窗：PlayerBar / 设置页的「桌面歌词」开关状态要跟着熄灭
  void emitGlobal(LYRIC_VIS_EVENT, false).catch(() => {});
  void win.hide();
}

onMounted(async () => {
  await settingsStore.init();
  // 顺序很重要：settings.init() 会把主题写回 <html data-theme>，
  // 所以「强制深色」必须放在它之后，否则歌词窗会被刷成浅色。
  document.documentElement.dataset.theme = "dark";
  await player.init();
  // 竖向模式下按持久化的方向恢复窗口形状（横向是默认形状，不用动）
  if (s.value.dlDirection === "vertical") {
    void applyShape("vertical");
  }

  window.addEventListener("dblclick", () => {
    void save({ dlDualLine: !s.value.dlDualLine });
  });
  window.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    closeWin();
  });
});
</script>

<template>
  <div
    class="lyric-root"
    :class="[rootClass, { hovered: hover }]"
    :style="stackVars"
    data-tauri-drag-region
    @mouseenter="hover = true"
    @mouseleave="hover = false"
  >
    <!-- 悬停工具条：锁定状态下整窗穿透，自然不会再出现 -->
    <transition name="bar">
      <div v-if="hover && !s.dlLocked" class="bar" data-tauri-drag-region>
        <button
          class="bar-btn text"
          :class="{ on: s.dlAlwaysOnTop }"
          :title="s.dlAlwaysOnTop ? '取消置顶' : '窗口置顶'"
          @click="togglePin"
        >
          置顶
        </button>
        <button class="bar-btn" title="锁定：鼠标穿透，到 设置 → 桌面歌词 解除" @click="lockNow">
          <Icon name="lock" :size="14" />
        </button>
        <span class="bar-sep" />
        <button class="bar-btn text" title="减小字号" @click="stepSize(-2)">A-</button>
        <span class="bar-size mono">{{ s.dlFontSize }}</span>
        <button class="bar-btn text" title="增大字号" @click="stepSize(2)">A+</button>
        <span class="bar-flex" data-tauri-drag-region />
        <button class="bar-btn" title="上一首" @click="player.prev()">
          <Icon name="prev" :size="14" filled />
        </button>
        <button class="bar-btn" :title="player.isPlaying ? '暂停' : '播放'" @click="player.toggle()">
          <Icon :name="player.isPlaying ? 'pause' : 'play'" :size="14" :filled="!player.isPlaying" />
        </button>
        <button class="bar-btn" title="下一首" @click="player.next()">
          <Icon name="next" :size="14" filled />
        </button>
        <span class="bar-sep" />
        <button class="bar-btn" title="关闭桌面歌词" @click="closeWin">
          <Icon name="close" :size="14" />
        </button>
      </div>
    </transition>

    <div class="stack" data-tauri-drag-region>
      <div v-if="s.dlDualLine && prevLine" class="line side" data-tauri-drag-region>
        {{ prevLine.text }}
      </div>

      <div class="line current" data-tauri-drag-region>
        <template v-if="current">
          <span
            v-for="(w, i) in current.words"
            :key="i"
            class="word"
            :style="{ '--p': wordProgress(w.startMs, w.endMs) }"
            >{{ w.text }}</span
          >
          <template v-if="!current.words.length">{{ current.text }}</template>
        </template>
        <span v-else class="idle">{{ snap.track?.title ?? "LeonMusic" }}</span>
      </div>

      <div v-if="showTrans && current" class="line trans" data-tauri-drag-region>
        {{ current.translation }}
      </div>
      <div v-else-if="s.dlDualLine && nextLine" class="line side" data-tauri-drag-region>
        {{ nextLine.text }}
      </div>
    </div>
  </div>
</template>

<style scoped>
/* 悬浮歌词必须是「贴在桌面上的文字」，不能带一块不透明底板。
   main.css 里 body 有实色背景（主界面需要），这里必须显式清掉。 */
:global(html),
:global(body) {
  background: transparent !important;
  background-image: none !important;
}
.lyric-root {
  height: 100vh;
  width: 100vw;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  /* 默认完全透明；鼠标移上来才浮出底板，提示「这里可以拖」 */
  background: transparent;
  border: 1px solid transparent;
  border-radius: 18px;
  overflow: hidden;
  cursor: move;
  transition: background 160ms ease, border-color 160ms ease;
  position: relative;
  /* backdrop-filter 在独立透明窗里取不到桌面像素，只会得到一块灰底，
     所以这里不用它，改用 rgba 半透明。 */
}
/* 底板必须由 JS 的 hover 状态驱动（class hovered），不能用 :hover 伪类：
   锁定（鼠标穿透）瞬间 WebView2 收不到 mouseleave，:hover 会冻结在 true，
   黑色底板就会一直挂在桌面上（2026-11-07 用户反馈）。锁定时 watch 里
   强制 hover=false，底板和工具条一起消失。 */
.lyric-root.hovered {
  background: rgba(10, 10, 16, 0.46);
  border-color: rgba(255, 255, 255, 0.08);
}

/* ---- 悬停工具条 ---- */
.bar {
  position: absolute;
  top: 8px;
  left: 50%;
  transform: translateX(-50%);
  z-index: 5;
  display: flex;
  align-items: center;
  gap: 2px;
  padding: 4px 8px;
  border-radius: 999px;
  background: rgba(18, 18, 26, 0.92);
  border: 1px solid rgba(255, 255, 255, 0.1);
  box-shadow: 0 6px 24px rgba(0, 0, 0, 0.45);
  white-space: nowrap;
  max-width: calc(100vw - 24px);
}
.bar-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 26px;
  height: 26px;
  padding: 0 6px;
  border: none;
  border-radius: 8px;
  background: transparent;
  color: rgba(255, 255, 255, 0.85);
  cursor: pointer;
  font-size: 12px;
  line-height: 1;
  font-family: inherit;
}
.bar-btn:hover {
  background: rgba(255, 255, 255, 0.12);
  color: #fff;
}
.bar-btn.on {
  color: var(--accent-2, #8f7bff);
}
.bar-btn.text {
  font-weight: 600;
  letter-spacing: 0.02em;
}
.bar-size {
  min-width: 30px;
  text-align: center;
  font-size: 11px;
  color: rgba(255, 255, 255, 0.55);
  user-select: none;
}
.bar-sep {
  width: 1px;
  height: 14px;
  margin: 0 4px;
  background: rgba(255, 255, 255, 0.14);
}
.bar-flex {
  flex: 1 1 auto;
  min-width: 8px;
}
.bar-enter-active,
.bar-leave-active {
  transition: opacity 140ms ease, transform 140ms ease;
}
.bar-enter-from,
.bar-leave-to {
  opacity: 0;
  transform: translateX(-50%) translateY(-6px);
}

/* ---- 歌词正文 ---- */
.stack {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
  padding: 34px 26px 10px;
  width: 100%;
  transform-origin: center;
}
.line {
  text-align: center;
  line-height: 1.35;
  width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: #fff;
  text-shadow:
    0 2px 12px rgba(0, 0, 0, 0.85),
    0 0 2px rgba(0, 0, 0, 0.9);
}
.current {
  font-size: var(--dl-size, 34px);
  font-weight: 800;
  letter-spacing: 0.01em;
  color: var(--dl-plain);
}
.side {
  font-size: var(--dl-side, 15px);
  font-weight: 500;
  color: var(--dl-side-color);
}
.trans {
  font-size: var(--dl-trans-size, 14px);
  font-weight: 500;
  color: var(--dl-trans-color);
}
.idle {
  opacity: 0.6;
  font-size: calc(var(--dl-size, 34px) * 0.7);
}
.word {
  display: inline-block;
  white-space: pre;
  background-image: linear-gradient(
    to right,
    var(--dl-sung) calc(var(--p) * 100%),
    var(--dl-unsung) calc(var(--p) * 100%)
  );
  -webkit-background-clip: text;
  background-clip: text;
  color: transparent;
  text-shadow: none;
  filter: drop-shadow(0 2px 8px rgba(0, 0, 0, 0.85));
}

/* ---- 排版：对齐（横向时控制文本，竖向时控制列高方向） ---- */
.align-left .line {
  text-align: left;
}
.align-right .line {
  text-align: right;
}
.align-left.stack,
.lyric-root.align-left .stack {
  align-items: flex-start;
}
.lyric-root.align-right .stack {
  align-items: flex-end;
}

/* ---- 排版：竖向（writing-mode 让每行歌词变竖列，自右向左排列） ---- */
.lyric-root.vertical {
  flex-direction: row;
}
.lyric-root.vertical .stack {
  writing-mode: vertical-rl;
  text-orientation: mixed;
  flex-direction: row;
  align-items: center;
  padding: 10px 26px 10px 34px;
  max-height: 100%;
}
.lyric-root.vertical .line {
  width: auto;
  max-height: calc(100vh - 60px);
  text-overflow: ellipsis;
}
.lyric-root.vertical.align-left .stack {
  justify-content: flex-start;
}
.lyric-root.vertical.align-right .stack {
  justify-content: flex-end;
}
</style>
