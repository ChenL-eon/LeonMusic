<script setup lang="ts">
/** 底部播放条（参考主流播放器三段式）：
 *  左：封面 + 歌名/歌手；中：❤ ⏮ ⏯ ⏭ 播放顺序 + 进度；右：音量 / 倍速 / 音质 / 队列 / 歌词。
 *  所有操作都发命令，状态从 store（Rust 镜像）读。 */
import { computed, onMounted, ref, watch } from "vue";
import { storeToRefs } from "pinia";
import { listen } from "@tauri-apps/api/event";

import CoverArt from "@/components/CoverArt.vue";
import Icon from "@/components/Icon.vue";
import Slider from "@/components/Slider.vue";
import { LYRIC_VIS_EVENT, player as playerApi } from "@/api/ipc";
import { useLibraryStore } from "@/stores/library";
import { usePlayerStore } from "@/stores/player";
import { useSettingsStore } from "@/stores/settings";
import { useUiStore } from "@/stores/ui";
import { qualityTag } from "@/types";

const player = usePlayerStore();
const library = useLibraryStore();
const settings = useSettingsStore();
const ui = useUiStore();
const { snap, track, isPlaying, progress, displayPosition } = storeToRefs(player);

const fav = computed(() => library.isFav(track.value));
/** 实际拿到的音质，不是设置里选的档位。 */
const quality = computed(() => qualityTag(snap.value.quality));
/** 音量竖条弹出（hover 音量图标时显示） */
const volPop = ref(false);

// 切歌时刷新云端红心状态（网易云插件曲目；失败静默）
watch(track, (t) => void library.refreshCloudLiked(t), { immediate: true });

function fmt(ms: number): string {
  if (!ms || ms < 0) return "0:00";
  const total = Math.floor(ms / 1000);
  return `${Math.floor(total / 60)}:${(total % 60).toString().padStart(2, "0")}`;
}

async function onSeek(v: number) {
  await player.seek(v);
}

async function onVolume(v: number) {
  await player.setVolume(v);
  // Rust 已经把音量写回数据库；这里同步本地 store，
  // 免得设置页仍显示拖动前的旧百分比。
  settings.value.volume = v;
}

/** 悬停音量区（图标或弹出竖条）时滚轮微调音量：上滚 +5%，下滚 -5%。 */
async function onVolumeWheel(e: WheelEvent) {
  const step = e.deltaY < 0 ? 0.05 : -0.05;
  const v = Math.min(1, Math.max(0, Math.round((snap.value.volume + step) * 100) / 100));
  if (v !== snap.value.volume) await onVolume(v);
}

async function toggleFav() {
  if (!track.value) return;
  const on = await library.toggleFavoriteTrack(track.value);
  if (on) ui.toast("已红心（同步到网易云）");
}

/** 倍速菜单：0.5x~2x 弹出选择（参考图 3），当前档位高亮。 */
const speedOpen = ref(false);
const speedSteps = [0.5, 0.75, 1, 1.25, 1.5, 2];
async function pickSpeed(s: number) {
  speedOpen.value = false;
  await player.setSpeed(s);
}

/** 播放顺序：单按钮循环切换 顺序 → 随机 → 单曲循环 → 列表循环（参考图 1 交互）。
 *  数据模型是 shuffle + repeat 两个独立开关，这里组合成四种互斥模式。 */
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

const speedTag = computed(() => {
  const s = snap.value.speed;
  return Math.abs(s - 1) < 0.001 ? "1x" : `${s.toFixed(2).replace(/0+$/, "").replace(/\.$/, "")}x`;
});

/** 桌面歌词开关：点击开/关悬浮歌词窗（不再跳播放页），按钮随真实窗口状态高亮。
 *  状态三个来源：①启动/获得焦点时查询窗口 ②toggle 返回值 ③歌词窗自行关闭时广播。
 *  锁定态跟随 settings.dlLocked（歌词窗锁定时写设置，主窗口经 settings_changed 实时收到）：
 *  锁定后歌词窗鼠标穿透无法自救，本按钮变锁形、点击解锁 —— 主界面最顺手的唯一入口。 */
const dlLockedActive = computed(() => ui.lyricWindowOn && settings.value.dlLocked);
const lyricBtnTitle = computed(() =>
  dlLockedActive.value
    ? "解锁桌面歌词"
    : ui.lyricWindowOn
      ? "关闭桌面歌词"
      : "开启桌面歌词",
);

/** 开/关歌词窗本体（toggle 返回值同步按钮状态）。 */
async function toggleLyric() {
  try {
    ui.lyricWindowOn = await playerApi.toggleLyricWindow();
  } catch {
    ui.toast("桌面歌词仅 Windows 支持", "error");
  }
}

/** 锁定瞬间提示一次解锁入口（歌词窗本体已穿透，用户容易找不到出路）。 */
watch(dlLockedActive, (locked, was) => {
  if (locked && !was) ui.toast("桌面歌词已锁定：点播放条右侧的锁形按钮解锁", "info");
});

async function onLyricBtn() {
  if (dlLockedActive.value) {
    await settings.save({ dlLocked: false });
    ui.toast("桌面歌词已解锁", "success");
    return;
  }
  await toggleLyric();
}

async function refreshLyricVisible() {
  try {
    ui.lyricWindowOn = await playerApi.lyricWindowVisible();
  } catch {
    /* 平台不支持 */
  }
}

onMounted(() => {
  void refreshLyricVisible();
  void listen<boolean>(LYRIC_VIS_EVENT, (e) => {
    ui.lyricWindowOn = e.payload === true;
  });
});
</script>

<template>
  <div class="bar card" :style="{ '--p': (progress * 100).toFixed(2) + '%' }">
    <!-- 左：当前曲目（点击进入「正在播放」；空状态不可点，
         2026-11-06 用户反馈：重启后误点空播放条会落进全屏空播放页，看着像出 bug 了） -->
    <div class="left" :class="{ clickable: !!track }" @click="track && ui.go('now')">
      <CoverArt
        :cover-path="track?.coverPath"
        :cover-url="track?.coverUrl"
        size="54px"
        radius="12px"
        class="mini-cover"
      />
      <div class="info" v-if="track">
        <div class="t">{{ track.title }}</div>
        <div class="a">
          {{ track.artist || "未知艺术家" }}
          <span
            v-if="quality"
            class="quality-mini"
            :class="[quality.tone, { warn: quality.warn }]"
          >{{ quality.text }}</span>
        </div>
      </div>
      <div class="info" v-else>
        <div class="t dim">未在播放</div>
        <div class="a">从本地音乐挑一首开始</div>
      </div>
    </div>

    <!-- 中：❤ ⏮ ⏯ ⏭ 🔀 + 进度（时间在两端） -->
    <div class="center">
      <div class="controls">
        <button
          class="icon-btn fav"
          :class="{ on: fav }"
          :disabled="!track"
          title="喜欢"
          @click="toggleFav"
        >
          <Icon name="heart" :size="18" :filled="fav" />
        </button>
        <button class="icon-btn" title="上一首" @click="player.prev()">
          <Icon name="prev" :size="19" filled />
        </button>
        <button class="play-btn" :disabled="!player.canPlay" @click="player.toggle()">
          <Icon :name="isPlaying ? 'pause' : 'play'" :size="20" :filled="!isPlaying" />
        </button>
        <button class="icon-btn" title="下一首" @click="player.next()">
          <Icon name="next" :size="19" filled />
        </button>
        <button
          class="icon-btn"
          :class="{ active: orderIndex !== 0 }"
          :title="`播放顺序：${order.label}（点击切换）`"
          @click="cycleOrder"
        >
          <Icon :name="order.icon" :size="17" />
        </button>
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
    </div>

    <!-- 右：音量（hover 上弹竖条）/ 倍速菜单 / 音质 / 队列 / 歌词 -->
    <div class="right">
      <div class="volume" @mouseenter="volPop = true" @mouseleave="volPop = false" @wheel.prevent="onVolumeWheel">
        <transition name="fade">
          <div v-if="volPop" class="vol-pop card">
            <span class="mono vol-pct">{{ Math.round(snap.volume * 100) }}%</span>
            <Slider
              class="vol-vertical"
              vertical
              :model-value="snap.volume"
              :max="1"
              :step="0.01"
              @change="onVolume"
            />
          </div>
        </transition>
        <button
          class="icon-btn"
          :title="snap.volume === 0 ? '已静音，点此恢复' : '静音'"
          @click="player.setVolume(snap.volume > 0 ? 0 : 0.85)"
        >
          <Icon
            :name="snap.volume === 0 ? 'volume-mute' : snap.volume < 0.4 ? 'volume-low' : 'volume'"
            :size="18"
          />
        </button>
      </div>

      <div class="speed-wrap">
        <button
          class="pill"
          :class="{ on: snap.speed !== 1 }"
          title="播放倍速"
          @click="speedOpen = !speedOpen"
        >
          <Icon name="speed" :size="14" />
          {{ speedTag }}
        </button>
        <transition name="fade">
          <div v-if="speedOpen" class="speed-menu card">
            <button
              v-for="s in speedSteps"
              :key="s"
              class="speed-item"
              :class="{ on: Math.abs(snap.speed - s) < 0.001 }"
              @click="pickSpeed(s)"
            >
              {{ s }}x
            </button>
          </div>
        </transition>
      </div>

      <span
        v-if="quality"
        class="pill"
        :class="[quality.tone, { warn: quality.warn }]"
        :title="quality.title"
      >{{ quality.text }}</span>

      <span class="pill" v-if="snap.pitchSemitones !== 0"
        :title="'音调偏移 ' + snap.pitchSemitones + ' 半音'"
      >{{ snap.pitchSemitones > 0 ? "+" : "" }}{{ snap.pitchSemitones }}st</span>
      <span class="pill" v-if="snap.gainDb !== 0"
        :title="'增益 ' + snap.gainDb + ' dB'"
      >{{ snap.gainDb > 0 ? "+" : "" }}{{ snap.gainDb }}dB</span>

      <button class="icon-btn" :class="{ active: ui.queueOpen }" title="播放队列" @click="ui.toggleQueue()">
        <Icon name="queue" :size="19" />
      </button>
      <!-- 桌面歌词开关（2026-11-07：锁定后歌词窗鼠标穿透，按钮变锁形、点击解锁） -->
      <button
        class="icon-btn"
        :class="{ active: ui.lyricWindowOn }"
        :title="lyricBtnTitle"
        @click="onLyricBtn"
      >
        <Icon :name="dlLockedActive ? 'lock' : 'lyrics'" :size="18" />
      </button>
    </div>
  </div>
</template>

<style scoped>
.bar {
  height: 100%;
  display: grid;
  grid-template-columns: minmax(180px, 1fr) minmax(320px, 1.4fr) minmax(180px, 1fr);
  align-items: center;
  gap: 18px;
  padding: 12px 20px;
  border-radius: var(--radius-l);
  backdrop-filter: blur(20px);
  background: color-mix(in srgb, var(--panel) 88%, transparent);
}

.left {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
  /* 点信息区进入「正在播放」（移动端是主要入口，桌面也保留这个习惯）；
     空状态没有 clickable 就不显示手型、也不响应点击 */
  cursor: default;
}
.left.clickable {
  cursor: pointer;
  border-radius: 14px;
}
.mini-cover {
  box-shadow: 0 6px 20px rgba(0, 0, 0, 0.35);
}
.info {
  min-width: 0;
}
.t {
  font-size: 14px;
  font-weight: 600;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.a {
  font-size: 12px;
  color: var(--text-3);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  margin-top: 2px;
}

.center {
  display: flex;
  flex-direction: column;
  gap: 2px;
  align-items: center;
}
.controls {
  display: flex;
  align-items: center;
  gap: 12px;
}
.fav.on {
  color: #ff5c8a;
}
.play-btn {
  width: 40px;
  height: 40px;
  border-radius: 50%;
  display: grid;
  place-items: center;
  /* 参考图：实心圆主按钮，自动随主题反色（浅=黑圆白标，深=白圆黑标） */
  background: var(--text);
  color: var(--bg);
  transition: transform 0.12s var(--ease), filter 0.18s;
}
.play-btn:hover {
  filter: brightness(1.12);
}
.play-btn:active {
  transform: scale(0.94);
}
.play-btn:disabled {
  opacity: 0.4;
}

.progress-row {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
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

.right {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 6px;
}
.volume {
  position: relative;
  display: flex;
  align-items: center;
  gap: 4px;
}
/* hover 音量图标：上弹竖直音量条（参考图 2）。
   顶部的隐形桥 + padding 让鼠标从图标移入弹层时不算 leave。 */
.vol-pop {
  position: absolute;
  bottom: calc(100% + 14px);
  left: 50%;
  transform: translateX(-50%);
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  padding: 12px 8px 10px;
  border-radius: 14px;
  z-index: 30;
  cursor: default;
}
.vol-pop::after {
  /* 图标与弹层之间的悬停桥，避免鼠标斜移时闪断 */
  content: "";
  position: absolute;
  top: 100%;
  left: 0;
  right: 0;
  height: 14px;
}
.vol-pct {
  font-size: 11px;
  color: var(--text-2);
}
.vol-vertical {
  height: 104px;
}
.speed-wrap {
  position: relative;
}
/* 倍速弹出菜单（参考图 3）：当前档位高亮 */
.speed-menu {
  position: absolute;
  bottom: calc(100% + 12px);
  right: 0;
  display: flex;
  flex-direction: column;
  padding: 5px;
  min-width: 74px;
  border-radius: 12px;
  z-index: 30;
}
.speed-menu::after {
  content: "";
  position: absolute;
  top: 100%;
  right: 0;
  width: 100%;
  height: 12px;
}
.speed-item {
  padding: 7px 14px;
  border-radius: 8px;
  font-size: 12.5px;
  color: var(--text-2);
  text-align: left;
  white-space: nowrap;
}
.speed-item:hover {
  background: var(--hover);
  color: var(--text);
}
.speed-item.on {
  background: color-mix(in srgb, var(--accent) 12%, transparent);
  color: var(--accent);
  font-weight: 600;
}
/* 功能胶囊：倍速 / 音质 / 变调 / 增益 */
.pill {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  height: 28px;
  padding: 0 10px;
  border-radius: 999px;
  border: 1px solid var(--border-strong);
  background: transparent;
  color: var(--text-2);
  font-size: 11.5px;
  font-weight: 600;
  white-space: nowrap;
  cursor: default;
}
button.pill {
  cursor: pointer;
}
button.pill:hover {
  background: var(--hover);
  color: var(--text);
}
.pill.on {
  border-color: color-mix(in srgb, var(--accent) 55%, transparent);
  color: var(--accent);
}
/* 音质徽标色调：LQ 灰 / SQ 绿 / HQ 蓝 / FLAC 金 / Hi-Res 橙红（亮暗主题通用的中亮色） */
.pill.lq {
  color: #98a2b3;
  border-color: color-mix(in srgb, #98a2b3 50%, transparent);
}
.pill.sq {
  color: #3ecf8e;
  border-color: color-mix(in srgb, #3ecf8e 55%, transparent);
}
.pill.hq {
  color: #4da3ff;
  border-color: color-mix(in srgb, #4da3ff 55%, transparent);
}
.pill.flac {
  color: #f0b429;
  border-color: color-mix(in srgb, #f0b429 60%, transparent);
}
.pill.hires {
  color: #ff7a59;
  border-color: color-mix(in srgb, #ff7a59 60%, transparent);
}
/* 降级/试听：保持音质色、边框虚线提示规格不稳，详细原因看悬浮说明 */
.pill.warn {
  border-style: dashed;
}

/* 窄屏：右标签收起，把真实音质挪到歌名下方那行 */
.quality-mini {
  display: none;
  margin-left: 6px;
  font-size: 9px;
  font-weight: 600;
  color: var(--accent);
  white-space: nowrap;
}
.quality-mini.lq {
  color: #98a2b3;
}
.quality-mini.sq {
  color: #3ecf8e;
}
.quality-mini.hq {
  color: #4da3ff;
}
.quality-mini.flac {
  color: #f0b429;
}
.quality-mini.hires {
  color: #ff7a59;
}
.quality-mini.warn {
  text-decoration: underline dashed;
  text-underline-offset: 2px;
}

@media (max-width: 980px) {
  .bar {
    grid-template-columns: minmax(140px, 1fr) minmax(260px, 1.6fr) auto;
    gap: 10px;
    padding: 10px 14px;
  }
  .volume,
  .pill {
    display: none;
  }
  .quality-mini {
    display: inline;
  }
}

/*
 * 移动端：单行「封面 + 标题 …… 上一首 / 播放 / 下一首」。
 * 进度信息用卡片顶部的 3px 细条（::before），不占高度。
 */
@media (max-width: 860px) {
  .bar {
    position: relative;
    grid-template-columns: minmax(0, 1fr) auto;
    grid-template-rows: 1fr;
    align-items: center;
    gap: 2px;
    padding: 8px 8px 8px 10px;
    overflow: hidden;
  }
  .bar::before {
    content: "";
    position: absolute;
    top: 0;
    left: 0;
    height: 3px;
    width: var(--p, 0%);
    background: var(--accent);
    border-radius: 0 3px 3px 0;
    transition: width 0.2s linear;
    pointer-events: none;
  }

  .left {
    grid-column: 1;
    grid-row: 1;
    gap: 10px;
    min-width: 0;
    cursor: pointer;
  }
  .left:active {
    opacity: 0.7;
  }
  .t {
    font-size: 13.5px;
  }
  .a {
    font-size: 11.5px;
  }

  .center {
    grid-column: 2;
    grid-row: 1;
    gap: 0;
  }
  .progress-row {
    display: none;
  }
  .controls {
    gap: 0;
  }
  /* 喜欢 / 随机收进「正在播放」页 */
  .controls .fav,
  .controls .icon-btn:last-child {
    display: none;
  }
  .controls .icon-btn {
    width: 40px;
    height: 40px;
  }
  .play-btn {
    width: 44px;
    height: 44px;
  }

  .right {
    display: none;
  }
}
</style>
