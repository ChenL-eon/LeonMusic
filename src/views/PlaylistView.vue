<script setup lang="ts">
/** 歌单详情 / 歌单管理。 */
import { computed, nextTick, onActivated, onMounted, onUnmounted, ref, watch } from "vue";

import CoverArt from "@/components/CoverArt.vue";
import Icon from "@/components/Icon.vue";
import TrackList from "@/components/TrackList.vue";
import { useLibraryStore } from "@/stores/library";
import { usePlayerStore } from "@/stores/player";
import { useUiStore } from "@/stores/ui";
import { useSettingsStore } from "@/stores/settings";
import type { Track } from "@/types";

const library = useLibraryStore();
const player = usePlayerStore();
const ui = useUiStore();
const settings = useSettingsStore();

const creating = ref(false);
const newName = ref("");

onMounted(() => void library.init());

const active = computed(() => library.activePlaylist);
const tracks = computed(() => library.playlistTracks);
/** 头图：歌单自己记的封面（云端导入写入）优先，否则回退到第一首有封面的曲目 */
const cover = computed(() => {
  const t = tracks.value.find((x) => x.coverPath || x.coverUrl);
  return {
    path: active.value?.coverPath ?? t?.coverPath ?? null,
    url: active.value?.coverUrl ?? t?.coverUrl ?? null,
  };
});

async function create() {
  const name = newName.value.trim();
  if (!name) return;
  const p = await library.createPlaylist(name);
  newName.value = "";
  creating.value = false;
  await library.openPlaylist(p.id);
  ui.toast(`已创建「${p.name}」`, "success");
}

function play(_t: Track, i: number) {
  void player.playList(tracks.value, i);
}

async function remove(t: Track) {
  if (!active.value) return;
  await import("@/api/ipc").then((m) => m.playlist.removeTracks(active.value!.id, [t.id]));
  await library.openPlaylist(active.value.id);
  ui.toast("已从歌单移除");
}

async function del() {
  if (!active.value) return;
  await library.deletePlaylist(active.value.id);
  ui.toast("歌单已删除", "success");
}

async function addQueue(t: Track) {
  await player.addToQueue([t], false);
  ui.toast("已加入队列");
}

/* ---------------- 右下角悬浮按钮：回到顶部 / 定位当前播放 ---------------- */

/** 详情列表与歌单网格是两个互斥的滚动容器，取当前挂载的那个。 */
const detailEl = ref<HTMLElement | null>(null);
const gridEl = ref<HTMLElement | null>(null);
const scrollEl = computed(() => detailEl.value ?? gridEl.value);

/** 回到顶部：滚过一屏才出现，避免在顶部晃着碍眼。
 *  阈值不能写死 200px —— 底部留白让短歌单的可滚动距离也可能不足 200，
 *  那样滚到底按钮都出不来。改成「最多 200，且不超过可滚动距离的一半」。 */
const NEAR_TOP = 200;
const scrolled = ref(false);
function syncScrolled() {
  const el = scrollEl.value;
  if (!el) {
    scrolled.value = false;
    return;
  }
  const max = el.scrollHeight - el.clientHeight;
  const showAt = Math.min(NEAR_TOP, Math.max(24, max * 0.5));
  scrolled.value = el.scrollTop > showAt;
}
function onScroll() {
  syncScrolled();
}

/** 当前播放曲目在本歌单中的位置（-1 = 不在本歌单 / 没在播放） */
const currentIndex = computed(() => {
  const id = player.snap.track?.id;
  return id ? tracks.value.findIndex((t) => t.id === id) : -1;
});
const canLocate = computed(() => !!active.value && currentIndex.value >= 0);

/** 脉冲高亮：滚过去之后给这一行一个短促的光圈，视线能立刻咬住 */
let pulseTimer: ReturnType<typeof setTimeout> | undefined;
function pulseRow(row: HTMLElement) {
  row.classList.remove("locate-pulse");
  // 强制回流，连点两次也能重放动画
  void row.offsetWidth;
  row.classList.add("locate-pulse");
  clearTimeout(pulseTimer);
  pulseTimer = setTimeout(() => row.classList.remove("locate-pulse"), 900);
}

function scrollToTop() {
  scrollEl.value?.scrollTo({ top: 0, behavior: "smooth" });
}

/** 滚动结束后补一次脉冲：0.9s 的动画在长距离平滑滚动面前撑不到终点 */
function pulseOnArrive(el: HTMLElement, row: HTMLElement) {
  let done = false;
  const fire = () => {
    if (done) return;
    done = true;
    pulseRow(row);
  };
  // scrollend 只在真正停住时派发；老 WebView 不认识它，退回定时器
  if ("onscrollend" in el) el.addEventListener("scrollend", fire, { once: true });
  else setTimeout(fire, 800);
}

function locateCurrent() {
  if (!player.snap.track) {
    ui.toast("还没有正在播放的歌曲");
    return;
  }
  if (!canLocate.value) {
    ui.toast("正在播放的歌曲不在这个歌单里");
    return;
  }
  const el = detailEl.value;
  const row = el?.querySelector<HTMLElement>(".row.playing");
  if (!el || !row) return;
  // 用 rect 差值换算成容器内的滚动坐标，不依赖 offsetParent 是谁
  const offset = row.getBoundingClientRect().top - el.getBoundingClientRect().top;
  const top = el.scrollTop + offset - el.clientHeight * 0.42;
  const max = el.scrollHeight - el.clientHeight;
  const target = Math.max(0, Math.min(top, max));
  const far = Math.abs(target - el.scrollTop) > el.clientHeight;
  el.scrollTo({ top: target, behavior: "smooth" });
  pulseRow(row);
  // 一屏以上的距离就等落地再闪一次（近处不等，免得同一行连闪两下）
  if (far) pulseOnArrive(el, row);
}

/** 切换歌单 / 从别的页面回来时，按真实 scrollTop 校正按钮显隐（v-if 分支会换容器） */
function refresh() {
  void nextTick(syncScrolled);
}
watch(() => active.value?.id, refresh);
/** 删歌 / 刷新会让内容变短，scrollTop 被浏览器静默夹紧（不派发 scroll 事件），
 *  按钮得自己重新对一次状态 */
watch(() => tracks.value.length, refresh);
onActivated(refresh);
onMounted(refresh);
onUnmounted(() => clearTimeout(pulseTimer));
</script>

<template>
  <div class="page">
    <template v-if="active">
      <header class="head">
        <CoverArt :cover-path="cover.path" :cover-url="cover.url" size="120px" radius="18px" />
        <div class="meta">
          <div class="kind">歌单</div>
          <h1 class="page-title">{{ active.name }}</h1>
          <p class="page-sub">{{ active.trackCount }} 首 · 创建于 {{ new Date(active.createdAt).toLocaleDateString() }}</p>
          <div class="ops">
            <button class="btn primary" :disabled="!tracks.length" @click="player.playList(tracks, 0)">
              <Icon name="play" :size="15" filled /> 播放全部
            </button>
            <button class="btn ghost" @click="player.addToQueue(tracks, false)">
              <Icon name="queue" :size="15" /> 加入队列
            </button>
            <button v-if="!active.system" class="btn ghost danger" @click="del">
              <Icon name="trash" :size="15" /> 删除歌单
            </button>
          </div>
        </div>
      </header>

      <div ref="detailEl" class="body scroll" @scroll.passive="onScroll">
        <TrackList
          :tracks="tracks"
          :current-id="player.snap.track?.id"
          :favorite-ids="library.favoriteIds"
        :click-play="settings.isAndroid"
          empty-text="歌单还是空的，去本地音乐右键添加"
          @play="play"
          @favorite="(t) => library.toggleFavorite(t.id)"
          @add-queue="addQueue"
          @add-playlist="addQueue"
          @remove="remove"
        />
      </div>
    </template>

    <template v-else>
      <header class="head">
        <div>
          <h1 class="page-title">我的歌单</h1>
          <p class="page-sub">{{ library.playlists.length }} 个歌单</p>
        </div>
        <button class="btn primary" @click="creating = true">
          <Icon name="plus" :size="16" /> 新建歌单
        </button>
      </header>

      <div v-if="creating" class="create card">
        <input v-model="newName" placeholder="歌单名称" @keyup.enter="create" autofocus />
        <button class="btn primary" @click="create">创建</button>
        <button class="btn ghost" @click="creating = false">取消</button>
      </div>

      <div ref="gridEl" class="grid scroll" @scroll.passive="onScroll">
        <button
          v-for="p in library.playlists"
          :key="p.id"
          class="pl-card"
          @click="library.openPlaylist(p.id)"
        >
          <CoverArt
            class="pl-cover"
            :cover-path="p.coverPath"
            :cover-url="p.coverUrl"
            size="100%"
            radius="10px"
          />
          <div class="pl-name">{{ p.name }}</div>
          <div class="pl-count">{{ p.trackCount }} 首</div>
        </button>
      </div>
    </template>

    <!-- 右下角悬浮操作：定位到正在播放（上）、回到顶部（下）。
         回到顶部用 CSS 隐藏而不是 v-if，保住占位，否则定位按钮会随之下跳。 -->
    <div class="fab">
      <button
        v-if="active && tracks.length"
        class="fab-btn locate"
        :class="{ idle: !canLocate }"
        title="定位到正在播放"
        aria-label="定位到正在播放"
        @click="locateCurrent"
      >
        <Icon name="locate" :size="19" />
      </button>
      <button
        class="fab-btn top"
        :class="{ show: scrolled }"
        title="回到顶部"
        aria-label="回到顶部"
        :aria-hidden="!scrolled"
        :tabindex="scrolled ? 0 : -1"
        @click="scrollToTop"
      >
        <Icon name="arrow-up" :size="19" />
      </button>
    </div>
  </div>
</template>

<style scoped>
.page {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  /* 悬浮按钮的定位基准；列表自己滚，按钮跟着页面固定 */
  position: relative;
}

/* ---------- 右下角悬浮按钮 ---------- */
.fab {
  position: absolute;
  right: 18px;
  bottom: 14px;
  z-index: 6;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
}
.fab-btn {
  width: 40px;
  height: 40px;
  border-radius: 50%;
  display: grid;
  place-items: center;
  color: var(--text-2);
  background: color-mix(in srgb, var(--panel) 88%, transparent);
  border: 1px solid var(--border);
  backdrop-filter: blur(14px);
  -webkit-backdrop-filter: blur(14px);
  box-shadow: 0 6px 18px rgba(0, 0, 0, 0.16);
  transition: color 0.16s var(--ease), background 0.16s var(--ease),
    transform 0.12s var(--ease), opacity 0.2s var(--ease), visibility 0.2s;
}
/* 触摸设备上 tap 完 :hover 会粘住，整颗按钮一直亮着 —— 只在真有指针时给 hover */
@media (hover: hover) {
  .fab-btn:hover {
    color: var(--accent);
    background: var(--panel-2);
  }
}
.fab-btn:active {
  transform: scale(0.93);
}
/* 当前播放不在本歌单时仍然可点（点了给提示），只是视觉上退到背景里 */
.fab-btn.locate.idle {
  color: var(--text-3);
  opacity: 0.55;
}
/* 未滚动到一定距离时收起，但保留占位 */
.fab-btn.top {
  opacity: 0;
  visibility: hidden;
  pointer-events: none;
  transform: translateY(8px) scale(0.9);
}
.fab-btn.top.show {
  opacity: 1;
  visibility: visible;
  pointer-events: auto;
  transform: none;
}
/* .top.show 的 0,3,0 会压掉 .fab-btn:active 的 0,2,0，按压反馈得单独提权 */
.fab-btn.top.show:active {
  transform: scale(0.93);
}
/* 定位落点的一次性光圈 */
.page :deep(.row.locate-pulse) {
  animation: locate-pulse 0.9s var(--ease);
}
@keyframes locate-pulse {
  0% {
    box-shadow: 0 0 0 0 color-mix(in srgb, var(--accent) 60%, transparent);
  }
  100% {
    box-shadow: 0 0 0 12px transparent;
  }
}
.head {
  display: flex;
  gap: 20px;
  align-items: flex-end;
  padding-bottom: 18px;
}
.meta {
  min-width: 0;
}
.kind {
  font-size: 10.5px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: var(--text-3);
  margin-bottom: 4px;
}
.ops {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
}
.danger {
  color: #ff6b6b;
}
.body {
  flex: 1;
  min-height: 0;
  /* 右下角悬浮按钮压在这里：按钮组 14 + 40 + 8 + 40 = 102px，
     再加列表自身的 20px 下内边距，滚到底时最后一行不会有一角被盖住 */
  padding-bottom: 88px;
}
.create {
  display: flex;
  gap: 8px;
  padding: 12px;
  margin-bottom: 14px;
  align-items: center;
}
.create input {
  flex: 1;
  height: 38px;
  border-radius: 10px;
  border: 1px solid var(--border);
  background: var(--panel-2);
  color: var(--text);
  padding: 0 12px;
  outline: none;
  font-size: 13px;
}
.grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
  gap: 14px;
  /* 同样给悬浮按钮让位：按钮组顶边距容器底 102px，这里没有列表的内边距可借 */
  padding-bottom: 108px;
  min-height: 0;
}
.pl-card {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 12px;
  border-radius: var(--radius-m);
  background: var(--panel);
  border: 1px solid var(--border);
  text-align: left;
  transition: transform 0.16s var(--ease), background 0.16s;
}
.pl-card:hover {
  transform: translateY(-2px);
  background: var(--panel-2);
}
.pl-cover {
  aspect-ratio: 1;
  border-radius: 10px;
  width: 100%;
}
.pl-name {
  font-size: 13px;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.pl-count {
  font-size: 11px;
  color: var(--text-3);
}

@media (max-width: 860px) {
  /* 封面在左、信息在右：竖排会把 120px 封面 + 三个按钮叠成高高一列，
     列表被推到屏幕外 */
  .head {
    flex-direction: row;
    align-items: flex-start;
    gap: 13px;
    padding-bottom: 12px;
  }
  /* CoverArt 的宽高是内联 style */
  .head :deep(.cover) {
    width: 82px !important;
    height: 82px !important;
    border-radius: 14px !important;
  }
  .kind {
    margin-bottom: 2px;
    font-size: 9.5px;
  }
  .page-title {
    font-size: 19px;
  }
  .ops {
    gap: 6px;
    margin-top: 8px;
  }
  .ops .btn {
    height: 38px;
    padding: 0 14px;
    font-size: 12.5px;
  }
  .create {
    flex-wrap: wrap;
  }
  .create input {
    flex: 1 1 100%;
    height: 44px;
  }
  .grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 10px;
    padding-bottom: 108px;
  }
  .body {
    padding-bottom: 92px;
  }
  .pl-card {
    padding: 10px;
  }
  /* 触摸端放到 44px，右下角避开滚动条与底部导航 */
  .fab {
    right: 8px;
    bottom: 4px;
    gap: 10px;
  }
  .fab-btn {
    width: 44px;
    height: 44px;
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.18);
  }
}

@media (max-width: 900px) and (max-height: 520px) {
  .head {
    padding-bottom: 6px;
    gap: 10px;
  }
  .head :deep(.cover) {
    width: 58px !important;
    height: 58px !important;
    border-radius: 12px !important;
  }
  .ops {
    margin-top: 4px;
  }
  .ops .btn {
    height: 34px;
    font-size: 12px;
  }
  .grid {
    grid-template-columns: repeat(auto-fill, minmax(120px, 1fr));
    gap: 8px;
  }
  /* 横屏高度只有 390：按钮回到 38px，别压住列表最后一行 */
  .fab {
    gap: 6px;
    bottom: 2px;
  }
  .fab-btn {
    width: 38px;
    height: 38px;
  }
}
</style>
