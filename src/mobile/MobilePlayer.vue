<script setup lang="ts">
/** 沉浸式播放页（全屏覆盖层）：封面模糊背景 + 封面/歌词分页 + 5 键控制栏。
 *  参考 KA Music 的 PlayerPage：从右侧滑入、下拉或点箭头关闭。 */
import { computed, ref, watch } from "vue";
import { storeToRefs } from "pinia";

import CoverArt from "@/components/CoverArt.vue";
import Icon from "@/components/Icon.vue";
import { useLibraryStore } from "@/stores/library";
import { usePlayerStore } from "@/stores/player";
import { useUiStore } from "@/stores/ui";
import type { LyricLine } from "@/types";

const emit = defineEmits<{ (e: "close"): void }>();

const player = usePlayerStore();
const library = useLibraryStore();
const ui = useUiStore();
const { snap, track, isPlaying, lyrics, currentLineIndex, displayPosition } = storeToRefs(player);

const pane = ref<0 | 1>(0);
const fav = computed(() => library.isFav(track.value));
/** 封面页歌词预览的第二行：当前句之后那句，纯展示不参与高亮 */
const nextLineText = computed(() => lyrics.value.lines[currentLineIndex.value + 1]?.text ?? "");
/** 切歌时刷新云端红心状态（网易云插件曲目；失败静默） */
watch(track, (t) => void library.refreshCloudLiked(t), { immediate: true });
/** 随机是独立开关：开时显示 shuffle 图标，其余按 repeat 显示 */
const repeatIcon = computed(() =>
  snap.value.shuffle ? "shuffle" : snap.value.repeat === "one" ? "repeat-one" : "repeat",
);

function fmt(ms: number): string {
  if (!ms || ms < 0) return "0:00";
  const total = Math.floor(ms / 1000);
  return `${Math.floor(total / 60)}:${(total % 60).toString().padStart(2, "0")}`;
}

/** 四态循环：全部循环 → 单曲循环 → 随机 → 顺序（随机是独立开关，与桌面共用引擎状态） */
function cycleMode() {
  const s = snap.value;
  if (s.shuffle) {
    void player.setShuffle(false);
    void player.setRepeat("off");
  } else if (s.repeat === "all") {
    void player.setRepeat("one");
  } else if (s.repeat === "one") {
    void player.setShuffle(true);
  } else {
    void player.setRepeat("all");
  }
}

async function toggleFav() {
  if (!track.value) return;
  const on = await library.toggleFavoriteTrack(track.value);
  ui.toast(on ? "已加入我喜欢的音乐" : "已取消喜欢", "success");
}

async function fetchLyrics() {
  await player.fetchRemoteLyrics();
  ui.toast(player.lyrics.lines.length ? "已获取在线歌词" : "没有找到歌词", "info");
}

function seekLine(l: LyricLine) {
  void player.seek(l.startMs);
}

function onSeek(e: Event) {
  void player.seek(Number((e.target as HTMLInputElement).value));
}

let sx = 0;
let sy = 0;
function onTouchStart(e: TouchEvent) {
  sx = e.changedTouches[0]?.clientX ?? 0;
  sy = e.changedTouches[0]?.clientY ?? 0;
}
function onTouchEnd(e: TouchEvent) {
  const dx = (e.changedTouches[0]?.clientX ?? 0) - sx;
  const dy = (e.changedTouches[0]?.clientY ?? 0) - sy;
  if (dy > 110 && dy > Math.abs(dx) * 1.4) {
    emit("close");
  } else if (Math.abs(dx) > 60 && Math.abs(dx) > Math.abs(dy)) {
    pane.value = dx < 0 ? 1 : 0;
  }
}
</script>

<template>
  <div class="player" @touchstart.passive="onTouchStart" @touchend.passive="onTouchEnd">
    <div class="bg">
      <CoverArt
        :cover-path="track?.coverPath"
        :cover-url="track?.coverUrl"
        size="100%"
        radius="0"
      />
    </div>
    <div class="veil" />

    <div class="content">
      <header class="top">
        <button class="arrow" aria-label="收起播放页" @click="emit('close')">
          <Icon name="chevron-down" :size="24" />
        </button>
        <div class="titles">
          <b>{{ track?.title ?? "还没有播放" }}</b>
          <small>{{ track?.artist || "从本地音乐挑一首开始" }}</small>
        </div>
        <div class="acts">
          <button class="rnd" :class="{ on: fav }" aria-label="喜欢" @click="toggleFav">
            <Icon name="heart" :size="19" :filled="fav" />
          </button>
          <button class="rnd" aria-label="播放队列" @click="ui.toggleQueue()">
            <Icon name="more" :size="17" filled />
          </button>
        </div>
      </header>

      <div class="pane">
        <div v-if="pane === 0" class="cover-pane">
          <CoverArt
            :cover-path="track?.coverPath"
            :cover-url="track?.coverUrl"
            size="var(--cover-w)"
            radius="20px"
            class="big-cover"
          />
          <div class="lyric-preview">
            <p class="cur">{{ lyrics.lines[currentLineIndex]?.text || "暂无歌词" }}</p>
            <p v-if="nextLineText" class="next">{{ nextLineText }}</p>
          </div>
        </div>

        <div v-else class="lyric-pane">
          <div v-if="lyrics.lines.length" class="lines">
            <button
              v-for="(l, i) in lyrics.lines"
              :key="i"
              class="line"
              :class="{ cur: i === currentLineIndex }"
              @click="seekLine(l)"
            >
              <span>{{ l.text }}</span>
              <small v-if="l.translation">{{ l.translation }}</small>
            </button>
          </div>
          <div v-else class="no-lyric">
            <div>暂无歌词</div>
            <button class="rnd wide" @click="fetchLyrics">获取在线歌词</button>
          </div>
        </div>
      </div>

      <div class="dots">
        <button :class="{ on: pane === 0 }" aria-label="封面" @click="pane = 0" />
        <button :class="{ on: pane === 1 }" aria-label="歌词" @click="pane = 1" />
      </div>

      <div class="bottom">
        <div class="progress">
          <input
            type="range"
            min="0"
            :max="Math.max(snap.durationMs, 1)"
            step="100"
            :value="displayPosition"
            :style="{ '--p': ((displayPosition / Math.max(snap.durationMs, 1)) * 100).toFixed(2) + '%' }"
            @touchstart.stop
            @change="onSeek"
          />
          <div class="times"><span>{{ fmt(displayPosition) }}</span><span>{{ fmt(snap.durationMs) }}</span></div>
        </div>

        <div class="controls">
          <button
            class="ctrl"
            :class="{ on: snap.repeat !== 'off' || snap.shuffle }"
            aria-label="播放模式"
            @click="cycleMode"
          >
            <Icon :name="repeatIcon" :size="21" />
          </button>
          <button class="ctrl" aria-label="上一首" @click="player.prev()">
            <Icon name="prev" :size="26" filled />
          </button>
          <button class="ctrl main" :disabled="!player.canPlay" aria-label="播放或暂停" @click="player.toggle()">
            <Icon :name="isPlaying ? 'pause-f' : 'play'" :size="30" filled />
          </button>
          <button class="ctrl" aria-label="下一首" @click="player.next()">
            <Icon name="next" :size="26" filled />
          </button>
          <button class="ctrl" aria-label="播放队列" @click="ui.toggleQueue()">
            <Icon name="queue" :size="22" />
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.player {
  position: absolute;
  inset: 0;
  z-index: 60;
  overflow: hidden;
  color: #fff;
  background: #1b1b24;
}
.bg {
  position: absolute;
  inset: -20%;
  filter: blur(60px) saturate(1.35);
  transform: scale(1.35);
  opacity: 0.75;
}
.bg :deep(.cover) {
  box-shadow: none;
}
.veil {
  position: absolute;
  inset: 0;
  background: linear-gradient(180deg, rgba(20, 20, 30, 0.42), rgba(18, 18, 26, 0.62) 55%, rgba(12, 12, 18, 0.82));
}
.content {
  /* 封面宽度只用一处定义：大封面与顶部标题的左对齐都靠它，改一处两边同步。
     70vw 与参考图的封面占比一致；上限 320px 防止平板上封面过大。 */
  --cover-w: min(70vw, 320px);
  --cover-x: calc((100% - var(--cover-w)) / 2);
  position: relative;
  height: 100%;
  display: flex;
  flex-direction: column;
  padding: calc(var(--sat) + 10px) 20px calc(20px + var(--sab));
  box-sizing: border-box;
}
.top {
  position: relative;
  display: flex;
  align-items: center;
  gap: 10px;
  flex: none;
  min-height: 46px;
}
/* 收起箭头不带圆形底：参考图里它只是一个贴左的图标 */
.arrow {
  position: absolute;
  left: -8px;
  top: 50%;
  transform: translateY(-50%);
  display: grid;
  place-items: center;
  width: 38px;
  height: 38px;
  border-radius: 50%;
  color: #fff;
  background: transparent;
}
.arrow:active {
  background: rgba(255, 255, 255, 0.14);
}
.acts {
  display: flex;
  align-items: center;
  gap: 10px;
  flex: none;
}
.rnd {
  display: grid;
  place-items: center;
  width: 42px;
  height: 42px;
  border-radius: 50%;
  border: 1px solid rgba(255, 255, 255, 0.3);
  background: rgba(255, 255, 255, 0.05);
  color: #fff;
  flex: none;
}
.rnd.on {
  color: #ff8fa3;
  border-color: rgba(255, 143, 163, 0.55);
}
.rnd.wide {
  width: auto;
  padding: 0 18px;
  font-size: 13px;
  margin-top: 14px;
  border-color: rgba(255, 255, 255, 0.22);
}
.titles {
  /* 左边缘对齐大封面：封面居中留出的空白 = (内容宽 - 封面宽) / 2。
     百分比在 padding-left 上按 .top 的宽度解析，正好等于内容宽。 */
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 3px;
  padding-left: var(--cover-x);
  text-align: left;
}
.titles b {
  font-size: 15px;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.titles small {
  font-size: 11.5px;
  opacity: 0.72;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.pane {
  flex: 1;
  min-height: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 16px 0;
}
.cover-pane {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 18px;
  width: 100%;
}
.big-cover {
  box-shadow: 0 24px 60px rgba(0, 0, 0, 0.45);
}
.lyric-preview {
  /* 与封面同宽、左对齐：整页文本共用一条左基线，参考图就是这种排法 */
  width: var(--cover-w);
  display: flex;
  flex-direction: column;
  gap: 7px;
  text-align: left;
}
.lyric-preview p {
  margin: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.lyric-preview .cur {
  font-size: 16px;
  font-weight: 600;
  color: rgba(255, 255, 255, 0.94);
}
.lyric-preview .next {
  font-size: 13.5px;
  color: rgba(255, 255, 255, 0.55);
}
.lyric-pane {
  width: 100%;
  height: 100%;
  overflow-y: auto;
  overscroll-behavior: contain;
  -webkit-overflow-scrolling: touch;
  /* 左边距与封面页共用同一条基线，切到歌词页时标题不会「跳一下」 */
  padding: 8px 4px 8px var(--cover-x);
  display: flex;
  flex-direction: column;
  align-items: stretch;
}
.lines {
  display: flex;
  flex-direction: column;
  gap: 22px;
  margin: auto 0;
}
.line {
  display: flex;
  flex-direction: column;
  gap: 6px;
  text-align: left;
  color: rgba(255, 255, 255, 0.55);
  font-size: 20px;
  font-weight: 500;
  line-height: 1.5;
}
.line small {
  font-size: 12.5px;
  font-weight: 400;
  opacity: 0.8;
}
.line.cur {
  color: #fff;
  font-size: 27px;
  font-weight: 700;
}
.no-lyric {
  margin: auto;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 4px;
  opacity: 0.7;
  font-size: 14px;
}
.dots {
  display: flex;
  justify-content: center;
  gap: 8px;
  flex: none;
  padding: 6px 0 10px;
}
.dots button {
  width: 7px;
  height: 7px;
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.35);
  transition: width 0.2s;
}
.dots button.on {
  width: 18px;
  background: #fff;
}
.bottom {
  flex: none;
}
.progress input {
  width: 100%;
  height: 18px;
  margin: 0;
  appearance: none;
  -webkit-appearance: none;
  background: transparent;
  outline: none;
}
.progress input::-webkit-slider-runnable-track {
  height: 3px;
  border-radius: 999px;
  background: linear-gradient(to right, #fff var(--p), rgba(255, 255, 255, 0.3) var(--p));
}
.progress input::-webkit-slider-thumb {
  appearance: none;
  -webkit-appearance: none;
  width: 12px;
  height: 12px;
  margin-top: -4.5px;
  border-radius: 50%;
  background: #fff;
}
.times {
  display: flex;
  justify-content: space-between;
  font-size: 11px;
  opacity: 0.7;
  font-variant-numeric: tabular-nums;
  margin-top: 2px;
}
.controls {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  padding-top: 12px;
}
.ctrl {
  display: grid;
  place-items: center;
  width: 48px;
  height: 48px;
  color: #fff;
  flex: none;
}
.ctrl.on {
  color: #a9c4ff;
}
.ctrl.main {
  width: 62px;
  height: 62px;
  border-radius: 50%;
  background: rgba(255, 255, 255, 0.16);
}
.ctrl:active {
  transform: scale(0.92);
}
</style>
