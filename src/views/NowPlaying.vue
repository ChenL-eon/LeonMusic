<script setup lang="ts">
/** 正在播放页：参考主流播放器沉浸式布局——
 *  左列封面 + 歌名 + 控制 + 进度条，右侧大字逐行歌词；
 *  开启「播放页背景」后整页铺模糊封面，文字切换纯白沉浸风。 */
import { computed, onUnmounted, ref, watch } from "vue";
import { storeToRefs } from "pinia";
import { convertFileSrc } from "@tauri-apps/api/core";

import CoverArt from "@/components/CoverArt.vue";
import Icon from "@/components/Icon.vue";
import LyricView from "@/components/LyricView.vue";
import Slider from "@/components/Slider.vue";
import { useLibraryStore } from "@/stores/library";
import { usePlayerStore } from "@/stores/player";
import { useSettingsStore } from "@/stores/settings";
import { useUiStore } from "@/stores/ui";
import { qualityTag } from "@/types";

withDefaults(defineProps<{ embedded?: boolean }>(), { embedded: false });
const emit = defineEmits<{ (e: "close"): void }>();

const player = usePlayerStore();
const library = useLibraryStore();
const settingsStore = useSettingsStore();
const ui = useUiStore();
const { snap, track, isPlaying, lyrics, currentLineIndex, displayPosition } = storeToRefs(player);

const showFx = ref(false);
const showMenu = ref(false);

/** 播放页封面背景：按设置的来源与模糊半径渲染，无封面/关闭时为 null。
 *  blur 用内联 filter（值跟随滑块实时变），遮罩统一走 CSS。 */
const bgStyle = computed(() => {
  const st = settingsStore.value;
  if (st.nowBgSource !== "cover" || !track.value) return null;
  const t = track.value;
  let url = "";
  if (t.coverPath) {
    try {
      url = convertFileSrc(t.coverPath);
    } catch {
      /* asset 协议不可用时退回 coverUrl */
    }
  }
  if (!url) url = t.coverUrl ?? "";
  if (!url) return null;
  return {
    backgroundImage: `url("${url}")`,
    filter: `blur(${st.nowBgBlur}px)`,
  };
});
/** 背景开启 = 沉浸配色（纯白文字 + 深色遮罩），歌词同步切 immersive。 */
const immersive = computed(() => !!bgStyle.value);

/** 沉浸状态同步到根元素：TitleBar 据此隐藏浅色底衬，
 *  否则深色专辑背景顶部会出现一条浅色带（无框标题栏融合用）。
 *  immediate 必须带：打开播放页时背景往往早已就绪，无变化也需初始同步。 */
watch(
  immersive,
  (on) => {
    document.documentElement.toggleAttribute("data-immersive", on);
  },
  { immediate: true },
);
onUnmounted(() => document.documentElement.removeAttribute("data-immersive"));

const fav = computed(() => (track.value ? library.favoriteIds.has(track.value.id) : false));
const speedLabel = computed(() => `${snap.value.speed.toFixed(2)}×`);
const pitchLabel = computed(() => {
  const v = snap.value.pitchSemitones;
  return v === 0 ? "原调" : `${v > 0 ? "+" : ""}${v} 半音`;
});
/** 左上角音质胶囊：显示实际拿到的音质（Hi-Res / 320k…），同播放条。 */
const quality = computed(() => qualityTag(snap.value.quality));

const repeatIcon = computed(() =>
  snap.value.repeat === "one" ? "repeat-one" : "repeat",
);

/** 播放顺序（更多菜单用，与播放条同一套四态循环） */
const orderStates = [
  { shuffle: false, repeat: "off", label: "顺序播放", icon: "sequence" },
  { shuffle: true, repeat: "off", label: "随机播放", icon: "shuffle" },
  { shuffle: false, repeat: "one", label: "单曲循环", icon: "repeat-one" },
  { shuffle: false, repeat: "all", label: "列表循环", icon: "repeat" },
] as const;
const orderIndex = computed(() =>
  orderStates.findIndex(
    (s) => s.shuffle === snap.value.shuffle && s.repeat === snap.value.repeat,
  ),
);
const order = computed(() => orderStates[Math.max(0, orderIndex.value)]);
async function cycleOrder() {
  const next = orderStates[(orderIndex.value + 1) % orderStates.length];
  await player.setShuffle(next.shuffle);
  await player.setRepeat(next.repeat);
}

/** 只在移动端出现的控制行（桌面把这些放在播放条右侧） */
function cycleRepeat() {
  const order = ["all", "one", "off"] as const;
  const i = order.indexOf(snap.value.repeat);
  void player.setRepeat(order[(i + 1) % order.length]);
}

async function toggleFav() {
  if (!track.value) return;
  const on = await library.toggleFavorite(track.value.id);
  ui.toast(on ? "已加入我喜欢的音乐" : "已取消喜欢", "success");
}

async function loadOnlineLyrics() {
  await player.fetchRemoteLyrics();
  ui.toast(
    player.lyrics.lines.length ? "已获取在线歌词" : "没有找到歌词",
    player.lyrics.lines.length ? "success" : "info",
  );
}

async function toggleLyricWindow() {
  try {
    // toggle 返回操作后的真实可见状态（而不是本地翻转），
    // 与 PlayerBar 开关、歌词窗自行关闭广播共用同一状态源 ui.lyricWindowOn。
    ui.lyricWindowOn = await import("@/api/ipc").then((m) => m.player.toggleLyricWindow());
  } catch {
    ui.toast("桌面歌词仅 Windows 支持", "error");
  }
}

async function onSeek(v: number) {
  await player.seek(v);
}

function fmt(ms: number): string {
  if (!ms || ms < 0) return "0:00";
  const total = Math.floor(ms / 1000);
  return `${Math.floor(total / 60)}:${(total % 60).toString().padStart(2, "0")}`;
}

function menuAction(fn: () => void) {
  showMenu.value = false;
  fn();
}
</script>

<template>
  <div class="now" :class="{ embedded, 'has-bg': immersive }">
    <!-- 封面背景层：设置里「播放页背景 = 歌曲封面」时铺满全页，
         blur 内联（跟滑块实时变），深色遮罩保证白字可读 -->
    <div v-if="bgStyle" class="cover-bg" :style="bgStyle" aria-hidden="true" />
    <div v-if="bgStyle" class="cover-bg-shade" aria-hidden="true" />

    <!-- 左上角：收起 + 音质胶囊 -->
    <div class="corner">
      <button v-if="embedded" class="corner-btn" aria-label="收起播放页" @click="emit('close')">
        <Icon name="chevron-down" :size="20" />
      </button>
      <span
        v-if="quality"
        class="corner-pill mono"
        :class="[quality.tone, { warn: quality.warn }]"
        :title="quality.title"
      >
        <Icon name="waveform" :size="13" />
        {{ quality.text }}
      </span>
    </div>

    <div class="stage">
      <div class="cover-wrap">
        <CoverArt
          :cover-path="track?.coverPath"
          :cover-url="track?.coverUrl"
          size="min(36vh, 300px)"
          radius="16px"
        />
      </div>

      <div class="meta">
        <div class="title">{{ track?.title ?? "还没有播放" }}</div>
        <div class="artist">
          {{ track?.artist || "选择一首歌开始" }}
          <template v-if="track?.album"> · {{ track.album }}</template>
        </div>
        <div v-if="snap.status === 'buffering'" class="chips">
          <span class="chip buffering">缓存中…</span>
        </div>
      </div>

      <!-- 控制行：❤ ⏮ ⏯ ⏭ ⋮（更多菜单收随机/循环/音效/桌面歌词/歌词） -->
      <div class="actions">
        <button class="icon-btn" :class="{ on: fav }" :disabled="!track" title="喜欢" @click="toggleFav">
          <Icon name="heart" :size="20" :filled="fav" />
        </button>
        <button class="icon-btn" :disabled="!track" title="上一首" @click="player.prev()">
          <Icon name="prev" :size="22" filled />
        </button>
        <button class="play-btn" :disabled="!player.canPlay" :title="isPlaying ? '暂停' : '播放'" @click="player.toggle()">
          <Icon :name="isPlaying ? 'pause' : 'play'" :size="30" :filled="!isPlaying" />
        </button>
        <button class="icon-btn" :disabled="!track" title="下一首" @click="player.next()">
          <Icon name="next" :size="22" filled />
        </button>
        <button class="icon-btn" title="更多" @click="showMenu = !showMenu">
          <Icon name="more" :size="20" />
        </button>

        <transition name="fade">
          <div v-if="showMenu" class="menu card">
            <button class="menu-item" @click="menuAction(cycleOrder)">
              <Icon :name="order.icon" :size="16" />
              播放顺序 · {{ order.label }}
            </button>
            <button class="menu-item" @click="menuAction(() => (showFx = !showFx))">
              <Icon name="speed" :size="16" />
              音效 · {{ speedLabel }} · {{ pitchLabel }}
            </button>
            <button
              v-if="player.platform?.supportsLyricWindow"
              class="menu-item"
              @click="menuAction(toggleLyricWindow)"
            >
              <Icon name="caption" :size="16" />
              桌面悬浮歌词
            </button>
            <button class="menu-item" @click="menuAction(loadOnlineLyrics)">
              <Icon name="refresh" :size="16" />
              重新获取歌词
            </button>
          </div>
        </transition>
      </div>

      <div class="progress-row">
        <span class="time mono">{{ fmt(displayPosition) }}</span>
        <Slider
          class="progress"
          :model-value="displayPosition"
          :max="Math.max(snap.durationMs, 1)"
          :step="100"
          thin
          @change="onSeek"
        />
        <span class="time mono">{{ fmt(snap.durationMs) }}</span>
      </div>

      <!-- 移动端专用：播放条上没地方放的循环 / 队列 / 音效 -->
      <div class="m-actions">
        <button
          class="icon-btn"
          :class="{ active: snap.shuffle }"
          title="随机播放"
          @click="player.setShuffle(!snap.shuffle)"
        >
          <Icon name="shuffle" :size="19" />
        </button>
        <button
          class="icon-btn"
          :class="{ active: snap.repeat !== 'off' }"
          :title="snap.repeat === 'one' ? '单曲循环' : snap.repeat === 'all' ? '列表循环' : '不循环'"
          @click="cycleRepeat"
        >
          <Icon :name="repeatIcon" :size="19" />
        </button>
        <button
          class="icon-btn"
          :class="{ active: ui.queueOpen }"
          title="播放队列"
          @click="ui.toggleQueue()"
        >
          <Icon name="queue" :size="20" />
        </button>
        <button class="icon-btn" title="音效" @click="showFx = !showFx">
          <Icon name="speed" :size="19" />
        </button>
        <button
          class="icon-btn"
          :class="{ on: fav }"
          :disabled="!track"
          title="喜欢"
          @click="toggleFav"
        >
          <Icon name="heart" :size="19" :filled="fav" />
        </button>
      </div>

      <!-- 音效面板 -->
      <transition name="fade">
        <div v-if="showFx" class="fx card">
          <div class="fx-row">
            <label>速度</label>
            <input
              type="range"
              min="0.5"
              max="2"
              step="0.05"
              :value="snap.speed"
              @input="player.setSpeed(Number(($event.target as HTMLInputElement).value))"
            />
            <span class="mono">{{ speedLabel }}</span>
          </div>
          <div class="fx-row">
            <label>音调</label>
            <input
              type="range"
              min="-12"
              max="12"
              step="1"
              :value="snap.pitchSemitones"
              @input="player.setPitch(Number(($event.target as HTMLInputElement).value))"
            />
            <span class="mono">{{ pitchLabel }}</span>
          </div>
          <div class="fx-row">
            <label>增益</label>
            <input
              type="range"
              min="-12"
              max="12"
              step="1"
              :value="snap.gainDb"
              @input="player.setGain(Number(($event.target as HTMLInputElement).value))"
            />
            <span class="mono">{{ snap.gainDb > 0 ? "+" : "" }}{{ snap.gainDb }} dB</span>
          </div>
          <div class="fx-note">
            变速不变调 / 变调不变速由 Rust 侧 WSOLA 完成，前端只发参数。
          </div>
        </div>
      </transition>
    </div>

    <div class="lyric-pane">
      <LyricView
        :lines="lyrics.lines"
        :current-index="currentLineIndex"
        :position="displayPosition"
        :show-translation="settingsStore.value.lyricShowTranslation"
        :align="settingsStore.value.lyricAlign"
        :immersive="immersive"
        @seek="player.seek"
      />
      <!-- 在线曲目没有本地 .lrc，标签里也没有歌词，必须手动拉一次。
           桌面有这个按钮（更多菜单），手机那排按钮被隐藏了，补一个。 -->
      <div v-if="track && !lyrics.lines.length" class="lyric-cta">
        <button class="btn ghost" @click="loadOnlineLyrics">
          <Icon name="refresh" :size="15" />
          获取在线歌词
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.now {
  display: grid;
  grid-template-columns: minmax(280px, 0.85fr) minmax(0, 1.15fr);
  gap: 30px;
  height: 100%;
  min-height: 0;
  /* 背景铺满全高（沉浸时深色盖满含标题带的窗口顶），
     内容用 padding 让出自绘标题栏；非沉浸时浅色渐变与标题带融合 */
  padding: calc(var(--titlebar-h) + 8px) 6px 0;
  /* 封面背景层的定位基准；背景只在播放页内显示，不外溢 */
  position: relative;
  overflow: hidden;
}

/* 封面背景：inset 外扩让 blur 边缘不露白，blur 值内联 */
.cover-bg {
  position: absolute;
  inset: -80px;
  background-size: cover;
  background-position: center;
  z-index: 0;
  pointer-events: none;
}
/* 沉浸遮罩：参考图是深色透底（能看到封面本色），白字靠 text-shadow 兜底 */
.cover-bg-shade {
  position: absolute;
  inset: 0;
  background: linear-gradient(
    to bottom,
    rgba(0, 0, 0, 0.34),
    rgba(0, 0, 0, 0.26) 40%,
    rgba(0, 0, 0, 0.42)
  );
  z-index: 0;
  pointer-events: none;
}
.stage,
.lyric-pane {
  position: relative;
  z-index: 1;
}

/* 沉浸配色：背景开启后整页白字 */
.now.has-bg {
  color: #fff;
}
.now.has-bg .title,
.now.has-bg .artist {
  color: #fff;
}
.now.has-bg .artist {
  opacity: 0.72;
}
.now.has-bg .time {
  color: rgba(255, 255, 255, 0.62);
}
.now.has-bg .icon-btn {
  color: rgba(255, 255, 255, 0.88);
}
.now.has-bg .icon-btn:hover {
  background: rgba(255, 255, 255, 0.14);
  color: #fff;
}
.now.has-bg .icon-btn.on {
  color: #ff7aa2;
}
.now.has-bg .chip.buffering {
  color: #fff;
  background: rgba(255, 255, 255, 0.18);
}

/* 左上角浮层：收起 + 音质胶囊 + 封面查看 */
.corner {
  position: absolute;
  top: 14px;
  left: 16px;
  z-index: 6;
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  max-width: 46%;
}
.corner-btn {
  width: 36px;
  height: 36px;
  border-radius: 50%;
  display: grid;
  place-items: center;
  color: var(--text-2);
  background: color-mix(in srgb, var(--panel) 82%, transparent);
  border: 1px solid var(--border);
  backdrop-filter: blur(14px);
}
.corner-btn:hover {
  color: var(--text);
  background: var(--hover);
}
.now.has-bg .corner-btn {
  color: rgba(255, 255, 255, 0.9);
  background: rgba(0, 0, 0, 0.28);
  border-color: rgba(255, 255, 255, 0.16);
}
.now.has-bg .corner-btn:hover {
  background: rgba(0, 0, 0, 0.45);
  color: #fff;
}
.corner-pill {
  height: 36px;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 0 12px;
  border-radius: 999px;
  font-size: 12px;
  font-weight: 600;
  color: var(--text-2);
  background: color-mix(in srgb, var(--panel) 82%, transparent);
  border: 1px solid var(--border);
  backdrop-filter: blur(14px);
  white-space: nowrap;
}
.now.has-bg .corner-pill {
  background: rgba(0, 0, 0, 0.28);
  border-color: rgba(255, 255, 255, 0.16);
}
/* 音质徽标色调（同播放条）：LQ 灰 / SQ 绿 / HQ 蓝 / FLAC 金 / Hi-Res 橙红 */
.corner-pill.lq {
  color: #98a2b3;
}
.corner-pill.sq {
  color: #3ecf8e;
}
.corner-pill.hq {
  color: #4da3ff;
}
.corner-pill.flac {
  color: #f0b429;
}
.corner-pill.hires {
  color: #ff7a59;
}
/* 降级/试听：保持音质色、边框虚线提示规格不稳 */
.corner-pill.warn {
  border-style: dashed;
}

/* 左列：封面 → 信息 → 控制 → 进度，全部左对齐 */
.stage {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  justify-content: center;
  gap: 18px;
  position: relative;
  min-height: 0;
  padding-left: clamp(4px, 3vw, 40px);
}

.meta {
  max-width: 420px;
  width: 100%;
}
.title {
  font-size: 26px;
  font-weight: 700;
  letter-spacing: -0.02em;
  line-height: 1.25;
  overflow: hidden;
  text-overflow: ellipsis;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
}
.artist {
  margin-top: 5px;
  font-size: 14px;
  color: var(--text-2);
}
.chips {
  display: flex;
  gap: 6px;
  margin-top: 8px;
  flex-wrap: wrap;
}
.chip {
  font-size: 10.5px;
  padding: 3px 8px;
  border-radius: 999px;
  background: var(--panel-3);
  color: var(--text-2);
  letter-spacing: 0.02em;
}
.chip.buffering {
  color: var(--accent);
  background: color-mix(in srgb, var(--accent) 15%, transparent);
}

.actions {
  display: flex;
  align-items: center;
  gap: 6px;
  position: relative;
}
/* 沉浸风控制键：无底色图标，播放键大一号 */
.actions .icon-btn {
  width: 44px;
  height: 44px;
  color: var(--text-2);
}
.actions .play-btn {
  width: 52px;
  height: 52px;
  margin: 0 6px;
  color: var(--text);
}
.now.has-bg .actions .play-btn {
  color: #fff;
}
.actions .icon-btn.on {
  color: #ff5c8a;
}
.icon-btn.big {
  width: 42px;
  height: 42px;
}

/* 更多菜单：贴着控制行上沿弹出 */
.menu {
  position: absolute;
  bottom: calc(100% + 10px);
  left: 0;
  min-width: 218px;
  padding: 6px;
  z-index: 8;
  display: flex;
  flex-direction: column;
  background: color-mix(in srgb, var(--panel) 94%, transparent);
  backdrop-filter: blur(20px);
}
.menu-item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 9px 12px;
  border-radius: 10px;
  font-size: 13px;
  color: var(--text);
  text-align: left;
}
.menu-item:hover {
  background: var(--hover);
}

/* 进度条：细滑块 + 淡色两端时间 */
.progress-row {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  max-width: 460px;
}
.progress {
  flex: 1;
}
.time {
  font-size: 11px;
  color: var(--text-3);
  min-width: 38px;
  text-align: center;
}

/* 移动端专用控制行，桌面隐藏 */
.m-actions {
  display: none;
}

.fx {
  position: absolute;
  bottom: 4px;
  width: min(360px, 92%);
  padding: 14px 16px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  background: color-mix(in srgb, var(--panel) 92%, transparent);
  backdrop-filter: blur(18px);
  z-index: 5;
}
.fx-row {
  display: grid;
  grid-template-columns: 34px 1fr 62px;
  align-items: center;
  gap: 10px;
  font-size: 12px;
}
.fx-row label {
  color: var(--text-2);
}
.fx-row span {
  text-align: right;
  color: var(--text-2);
  font-size: 11px;
}
.fx-row input[type="range"] {
  -webkit-appearance: none;
  appearance: none;
  height: 3px;
  border-radius: 999px;
  background: var(--border-strong);
  outline: none;
}
.fx-row input[type="range"]::-webkit-slider-thumb {
  -webkit-appearance: none;
  width: 12px;
  height: 12px;
  border-radius: 50%;
  background: var(--accent);
  cursor: pointer;
}
.fx-note {
  font-size: 10.5px;
  color: var(--text-3);
  line-height: 1.5;
  border-top: 1px solid var(--border);
  padding-top: 8px;
}

.lyric-pane {
  min-height: 0;
  height: 100%;
  position: relative;
  padding-right: clamp(6px, 2vw, 30px);
}

/* 空歌词时的「获取在线歌词」。桌面在更多菜单里，这里只在手机出现 */
.lyric-cta {
  position: absolute;
  left: 0;
  right: 0;
  top: 58%;
  display: none;
  justify-content: center;
}
.lyric-cta .btn {
  height: 34px;
  padding: 0 14px;
  font-size: 12.5px;
}

/*
 * 竖屏手机：单列「封面 → 信息 → 控制 → 歌词」。
 * 封面跟着视口宽度收，让歌词区至少能显示 5 行。
 */
@media (max-width: 860px) {
  .now {
    grid-template-columns: 1fr;
    grid-template-rows: auto minmax(0, 1fr);
    gap: 10px;
    padding: 2px 0 0;
  }
  .stage {
    gap: 12px;
    justify-content: flex-start;
    align-items: center;
    padding-left: 0;
  }
  /* CoverArt 的宽高是内联 style，只能 !important 盖 */
  .cover-wrap :deep(.cover) {
    width: min(52vw, 208px) !important;
    height: min(52vw, 208px) !important;
    border-radius: 18px !important;
  }
  .meta {
    text-align: center;
  }
  .title {
    font-size: 19px;
  }
  .artist {
    font-size: 12.5px;
    margin-top: 4px;
  }
  .chips {
    margin-top: 8px;
    justify-content: center;
  }

  .actions {
    display: none;
  }
  .progress-row {
    display: none;
  }
  .m-actions {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 4px;
  }
  .m-actions .icon-btn {
    width: 44px;
    height: 44px;
  }
  .m-actions .icon-btn.active {
    color: var(--accent);
  }

  /* 音效面板从「贴在封面下方」改成底部浮层，否则会盖住歌词 */
  .fx {
    position: fixed;
    left: 12px;
    right: 12px;
    /* 播放条 76 + tab 59 + 呼吸 8，再加手势条 */
    bottom: calc(143px + var(--sab));
    width: auto;
    z-index: 60;
    padding: 16px;
  }
  .fx-row {
    grid-template-columns: 32px 1fr 64px;
    gap: 12px;
  }
  /* 输入控件在触摸下要更粗，否则拖不动 */
  .fx-row input[type="range"] {
    height: 4px;
  }
  .fx-row input[type="range"]::-webkit-slider-thumb {
    width: 18px;
    height: 18px;
  }

  .lyric-pane {
    min-height: 0;
    overflow: hidden;
    padding-right: 0;
  }
  .lyric-cta {
    display: flex;
  }
  /* 左上角浮层在手机上只留音质胶囊（收起按钮由覆盖层提供） */
  .corner {
    left: auto;
    right: 12px;
    max-width: none;
  }
  .corner-btn {
    display: none;
  }
}

/*
 * 横屏：高度只有 393，单列排会把歌词压没。
 * 改回两栏（封面在左、歌词在右），封面缩到 104px 保证歌词至少有 4 行。
 */
@media (max-width: 900px) and (max-height: 520px) {
  .now {
    grid-template-columns: minmax(0, 0.72fr) minmax(0, 1.28fr);
    grid-template-rows: 1fr;
    gap: 14px;
  }
  .stage {
    justify-content: center;
    gap: 7px;
    align-items: center;
  }
  .cover-wrap :deep(.cover) {
    width: min(24vw, 104px) !important;
    height: min(24vw, 104px) !important;
    border-radius: 14px !important;
  }
  .title {
    font-size: 15px;
    -webkit-line-clamp: 1;
  }
  .artist {
    font-size: 11px;
    margin-top: 2px;
  }
  .chips {
    display: none;
  }
  .m-actions {
    gap: 0;
  }
  .m-actions .icon-btn {
    width: 34px;
    height: 34px;
  }
  .m-actions .icon-btn :deep(svg) {
    width: 17px;
    height: 17px;
  }
}
</style>
