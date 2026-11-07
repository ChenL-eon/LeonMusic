<script setup lang="ts">
/** 移动端聚合首页：顶部 chip + 大搜索框 + 双封面卡 + 推荐歌曲流。
 *  数据全部来自现有 store / IPC，不新增接口；「电台」是本地曲库的另一种切片。
 *  「推荐」tab 在网易云登录后取每日推荐（与桌面首页同源），未登录回落本地最近播放。 */
import { computed, onActivated, onMounted, ref } from "vue";

import CoverArt from "@/components/CoverArt.vue";
import Icon from "@/components/Icon.vue";
import MobileTrackRow from "@/mobile/MobileTrackRow.vue";
import { plugins as pluginApi } from "@/api/ipc";
import { useLibraryStore } from "@/stores/library";
import { usePlayerStore } from "@/stores/player";
import { useUiStore } from "@/stores/ui";
import type { RecommendFeed, Track } from "@/types";

const NETEASE = "netease";

const emit = defineEmits<{
  (e: "open-search", keyword?: string): void;
  (e: "open-settings"): void;
}>();

const library = useLibraryStore();
const player = usePlayerStore();
const ui = useUiStore();
const tab = ref<"recommend" | "radio">("recommend");

/** 云端每日推荐：null = 还没拉到（未登录 / 失败），此时回落本地切片。 */
const remote = ref<RecommendFeed | null>(null);
let loadedAt = 0;
const FEED_STALE_MS = 10 * 60 * 1000;

async function loadFeed(silent = false) {
  try {
    remote.value = await pluginApi.recommendFeed(NETEASE, 12);
    loadedAt = Date.now();
  } catch {
    if (!silent) remote.value = null; // 未登录 / 网络不通都按「用本地数据」处理
  }
}

onMounted(() => void loadFeed());
/* 页面被 KeepAlive 缓存：没拿到云端流（未登录→刚登录 / 首拉失败）或
 * 数据超过 10 分钟时，切回首页静默补拉——登录完成回到首页必须能刷出推荐 */
onActivated(() => {
  const fresh = loadedAt && Date.now() - loadedAt < FEED_STALE_MS;
  if (!fresh || !remote.value?.loggedIn) void loadFeed(true);
});

/** 登录且推荐有货时才启用云端流 */
const remoteTracks = computed<Track[]>(() =>
  remote.value?.loggedIn && remote.value.tracks.length
    ? remote.value.tracks.slice(0, 10)
    : [],
);

/** 首页列表：优先每日推荐，没有再用最近播放 / 收藏 / 本地库补齐。 */
const feed = computed<Track[]>(() => {
  const pick = (...groups: Track[][]) => {
    const seen = new Set<string>();
    return groups.flat().filter((t) => !seen.has(t.id) && seen.add(t.id)).slice(0, 10);
  };
  if (tab.value === "recommend" && remoteTracks.value.length) return remoteTracks.value;
  return tab.value === "recommend"
    ? pick(library.recent, library.favorites, library.tracks)
    : pick(library.tracks, library.recent);
});

const sectionTitle = computed(() => {
  if (tab.value === "radio") return "本地曲库";
  return remoteTracks.value.length ? "每日推荐" : "为你推荐";
});

const tileA = computed(() => library.favorites[0] ?? library.recent[0] ?? library.tracks[0] ?? null);
const tileB = computed(
  () => library.tracks[library.tracks.length - 1] ?? library.recent[1] ?? tileA.value,
);

function playAll(list: Track[]) {
  if (!list.length) {
    ui.toast("还没有可播放的歌曲，先去在线搜索吧", "info");
    return;
  }
  void player.playTracks(list, 0);
}

function playRow(t: Track) {
  const i = feed.value.findIndex((x) => x.id === t.id);
  void player.playTracks(feed.value, Math.max(0, i));
  ui.toast("已开始播放");
}

async function fav(t: Track) {
  const on = await library.toggleFavorite(t.id);
  ui.toast(on ? "已加入我喜欢的音乐" : "已取消喜欢", "success");
}

function more(t: Track) {
  void player.addToQueue([t], false);
  ui.toast(`「${t.title}」已加入队列`);
}
</script>

<template>
  <div class="m-page home">
    <div class="m-top">
      <div class="chips">
        <button class="m-chip" :class="{ on: tab === 'recommend' }" @click="tab = 'recommend'">
          推荐
        </button>
        <button class="m-chip" :class="{ on: tab === 'radio' }" @click="tab = 'radio'">
          电台
        </button>
      </div>
      <button class="m-round" aria-label="设置" @click="emit('open-settings')">
        <Icon name="settings" :size="20" />
      </button>
    </div>

    <button class="search m-card" @click="emit('open-search')">
      <Icon name="search" :size="19" />
      <span>搜索歌曲，歌手</span>
    </button>

    <div class="tiles">
      <button class="tile" @click="playAll(library.favorites.length ? library.favorites : feed)">
        <CoverArt
          :cover-path="tileA?.coverPath"
          :cover-url="tileA?.coverUrl"
          size="100%"
          radius="0"
          class="bg"
        />
        <span class="veil" />
        <span class="txt">
          <b>猜你喜欢</b>
          <small>{{ library.favorites.length ? `${library.favorites.length} 首收藏` : "从收藏开始" }}</small>
        </span>
        <span class="play"><Icon name="play" :size="16" filled /></span>
      </button>

      <button class="tile" @click="playAll(library.tracks.length ? library.tracks : feed)">
        <CoverArt
          :cover-path="tileB?.coverPath"
          :cover-url="tileB?.coverUrl"
          size="100%"
          radius="0"
          class="bg"
        />
        <span class="veil" />
        <span class="txt">
          <b>新碟上架</b>
          <small>本地曲库</small>
        </span>
        <span class="play"><Icon name="play" :size="16" filled /></span>
      </button>
    </div>

    <div class="m-section">
      <h2>{{ sectionTitle }}</h2>
      <button class="m-round primary" aria-label="播放全部" @click="playAll(feed)">
        <Icon name="play" :size="18" filled />
      </button>
    </div>

    <div v-if="feed.length" class="rows">
      <MobileTrackRow
        v-for="t in feed"
        :key="t.id"
        :track="t"
        :fav="library.favoriteIds.has(t.id)"
        :current="player.snap.track?.id === t.id"
        @play="playRow"
        @favorite="fav"
        @more="more"
      />
    </div>
    <div v-else class="m-empty">
      <Icon name="music" :size="34" />
      <div>还没有歌曲</div>
      <button class="m-chip on" @click="emit('open-search')">去在线搜索</button>
    </div>
  </div>
</template>

<style scoped>
.home {
  padding-left: 20px;
  padding-right: 20px;
}
.chips {
  display: flex;
  gap: 10px;
}
.search {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  height: 50px;
  padding: 0 18px;
  margin: 4px 0 16px;
  color: var(--text-3);
  font-size: 14px;
  text-align: left;
}
.tiles {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px;
}
.tile {
  position: relative;
  aspect-ratio: 0.82;
  border-radius: var(--m-r-lg);
  overflow: hidden;
  box-shadow: var(--m-shadow);
  text-align: left;
}
.tile:active {
  transform: scale(0.985);
}
.bg {
  position: absolute;
  inset: 0;
}
.bg :deep(.cover) {
  box-shadow: none;
}
.veil {
  position: absolute;
  inset: 0;
  background: linear-gradient(180deg, rgba(0, 0, 0, 0.42), transparent 52%, rgba(0, 0, 0, 0.28));
}
.txt {
  position: absolute;
  top: 14px;
  left: 14px;
  right: 12px;
  display: flex;
  flex-direction: column;
  gap: 3px;
  color: #fff;
  text-shadow: 0 1px 6px rgba(0, 0, 0, 0.5);
}
.txt b {
  font-size: 15px;
  font-weight: 700;
}
.txt small {
  font-size: 11px;
  opacity: 0.85;
}
.play {
  position: absolute;
  right: 12px;
  bottom: 12px;
  display: grid;
  place-items: center;
  width: 34px;
  height: 34px;
  border-radius: 50%;
  background: rgba(255, 255, 255, 0.9);
  color: #222;
}
.rows {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
</style>
