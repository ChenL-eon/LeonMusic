<script setup lang="ts">
/** 桌面首页：搜索框 + 推荐歌单 + 推荐歌曲/最近播放。
 *  推荐数据来自网易云插件聚合接口（getRecommendFeed）：
 *  登录后是每日推荐歌单 ∪ 个性推荐歌单 + 每日推荐歌曲；
 *  未登录只有个性推荐歌单（免登录接口），歌曲流退到最近播放。
 *  搜索在本页内联完成（防抖 400ms），不再跳独立搜索页。 */
import { computed, onActivated, onMounted, ref } from "vue";

import CoverArt from "@/components/CoverArt.vue";
import Icon from "@/components/Icon.vue";
import TrackList from "@/components/TrackList.vue";
import { errorText, plugins as pluginApi } from "@/api/ipc";
import { useLibraryStore } from "@/stores/library";
import { usePlayerStore } from "@/stores/player";
import { useUiStore } from "@/stores/ui";
import type { RecommendFeed, RemotePlaylistMeta, Track } from "@/types";

const NETEASE = "netease";

const ui = useUiStore();
const library = useLibraryStore();
const player = usePlayerStore();

const feed = ref<RecommendFeed | null>(null);
const toplists = ref<RemotePlaylistMeta[]>([]);
const loading = ref(false);
const loadError = ref("");
const openingId = ref("");

const songList = computed<Track[]>(() =>
  feed.value?.loggedIn && feed.value.tracks.length ? feed.value.tracks : library.recent,
);
const songTitle = computed(() =>
  feed.value?.loggedIn ? "每日推荐歌曲" : "最近播放",
);

let loadedAt = 0;
const FEED_STALE_MS = 10 * 60 * 1000;
async function load(silent = false) {
  if (!silent) {
    loading.value = true;
    loadError.value = "";
  }
  try {
    // 排行榜与推荐流并行拉取；榜单失败不阻塞首页（静默留空）
    const [f, tl] = await Promise.all([
      pluginApi.recommendFeed(NETEASE, 12),
      pluginApi.toplists(NETEASE).catch(() => [] as RemotePlaylistMeta[]),
    ]);
    feed.value = f;
    toplists.value = tl.slice(0, 12);
    loadedAt = Date.now();
  } catch (e) {
    if (!silent) loadError.value = errorText(e);
  } finally {
    if (!silent) loading.value = false;
  }
}

onMounted(() => void load());
/* 页面被 KeepAlive 缓存后切回首页不再重挂载：内容秒出，
   超过 10 分钟才静默补一次推荐，不闪加载态。 */
onActivated(() => {
  if (loadedAt && Date.now() - loadedAt > FEED_STALE_MS && !loading.value) void load(true);
});

// ---------------------------------------------------------------------------
// 内联搜索：输入防抖后直接在下方出结果，清空即回到推荐内容
// ---------------------------------------------------------------------------

const q = ref("");
const results = ref<Track[]>([]);
const searching = ref(false);
let searchSeq = 0;
let debounceTimer: number | null = null;
const DEBOUNCE_MS = 400;

const showResults = computed(() => q.value.trim().length > 0 || searching.value);

function onSearchInput() {
  if (debounceTimer !== null) window.clearTimeout(debounceTimer);
  if (!q.value.trim()) {
    searchSeq += 1;
    searching.value = false;
    results.value = [];
    return;
  }
  debounceTimer = window.setTimeout(() => void runSearch(), DEBOUNCE_MS);
}

async function runSearch() {
  const kw = q.value.trim();
  if (!kw) return;
  if (debounceTimer !== null) {
    window.clearTimeout(debounceTimer);
    debounceTimer = null;
  }
  const seq = ++searchSeq;
  searching.value = true;
  try {
    const items = await pluginApi.search(NETEASE, kw, 1, 30);
    if (seq !== searchSeq) return; // 已有更新的请求，丢弃过期结果
    results.value = items;
  } catch (e) {
    if (seq === searchSeq) {
      results.value = [];
      ui.toast(errorText(e), "error");
    }
  } finally {
    if (seq === searchSeq) searching.value = false;
  }
}

function clearSearch() {
  q.value = "";
  results.value = [];
  searching.value = false;
  searchSeq += 1;
  if (debounceTimer !== null) {
    window.clearTimeout(debounceTimer);
    debounceTimer = null;
  }
}

function playResult(_t: Track, i: number) {
  playTracks(results.value, i);
}

async function fav(t: Track) {
  // 在线曲目需要先入库，收藏才有意义
  const existing = library.tracks.find((x) => x.id === t.id);
  if (!existing) {
    ui.toast("在线曲目先播放一次即可收藏", "info");
    return;
  }
  await library.toggleFavorite(t.id);
}

function addQueue(t: Track) {
  void player.addToQueue([t], false);
  ui.toast("已加入队列");
}

function playTracks(list: Track[], i = 0) {
  if (!list.length) {
    ui.toast("还没有可播放的歌曲", "info");
    return;
  }
  void player.playTracks(list, i);
  ui.toast("已开始播放");
}

/** 私人FM：拉一批口味推荐直接开播。未登录也能试（服务端只给 1 首试听）。 */
const fmLoading = ref(false);
async function playFm() {
  if (fmLoading.value) return;
  fmLoading.value = true;
  try {
    const tracks = await pluginApi.personalFm(NETEASE);
    if (!tracks.length) {
      ui.toast("私人FM没有返回歌曲，稍后再试", "info");
      return;
    }
    await player.playTracks(tracks, 0);
    ui.toast(`私人FM · ${tracks.length} 首`);
  } catch (e) {
    ui.toast(errorText(e), "error");
  } finally {
    fmLoading.value = false;
  }
}

async function openCloudPlaylist(p: RemotePlaylistMeta) {
  if (openingId.value) return;
  openingId.value = p.id;
  try {
    const detail = await pluginApi.cloudPlaylist(NETEASE, p.id);
    if (!detail.tracks.length) {
      ui.toast("这个歌单还没有歌曲", "info");
      return;
    }
    await player.playTracks(detail.tracks, 0);
    ui.toast(`正在播放「${detail.name}」`);
  } catch (e) {
    ui.toast(errorText(e), "error");
  } finally {
    openingId.value = "";
  }
}

function playCount(n: number): string {
  if (n >= 100000000) return `${(n / 100000000).toFixed(1)}亿`;
  if (n >= 10000) return `${Math.round(n / 10000)}万`;
  return String(n);
}
</script>

<template>
  <div class="page home">
    <header class="head">
      <div>
        <h1 class="page-title">
          首页<span v-if="feed?.loggedIn && feed.nickname" class="hi">，{{ feed.nickname }}</span>
        </h1>
        <p class="page-sub">
          {{ feed?.loggedIn ? "根据你的听歌口味生成的每日推荐" : "登录网易云账号后，这里会变成你的每日推荐" }}
        </p>
      </div>
      <div class="search-box">
        <Icon name="search" :size="17" />
        <input
          v-model="q"
          class="search-input"
          placeholder="搜索歌曲、歌手、专辑"
          spellcheck="false"
          @input="onSearchInput"
          @keyup.enter="runSearch"
        />
        <button v-if="q" class="clear" aria-label="清空搜索" @click="clearSearch">
          <Icon name="close" :size="13" />
        </button>
        <kbd>网易云</kbd>
      </div>
    </header>

    <!-- 内容滚动区：推荐内容长了以后在这里上下滚，页头和搜索框保持不动 -->
    <div class="body scroll">
      <div v-if="loading && !feed" class="dim pad">正在获取推荐…</div>
      <div v-else-if="loadError && !feed" class="notice">
        <Icon name="plug" :size="16" />
        <div>
          推荐获取失败：<span class="dim">{{ loadError }}</span>
        </div>
      </div>

      <!-- 搜索态：输入即搜，结果顶掉推荐内容 -->
      <section v-else-if="showResults" class="sec">
        <div class="sec-head">
          <h2>
            搜索结果<span v-if="q.trim()" class="kw">「{{ q.trim() }}」</span>
          </h2>
          <button class="btn ghost" :disabled="!results.length" @click="playTracks(results)">
            <Icon name="play" :size="14" filled />
            播放全部
          </button>
        </div>
        <TrackList
          :tracks="results"
          :current-id="player.snap.track?.id"
          :favorite-ids="library.favoriteIds"
          :empty-text="searching ? '搜索中…' : '没有找到结果'"
          @play="playResult"
          @favorite="fav"
          @add-queue="addQueue"
          @add-playlist="addQueue"
          @remove="() => {}"
        />
      </section>

      <template v-else>
        <section v-if="feed?.playlists.length" class="sec">
          <div class="sec-head">
            <h2>{{ feed.loggedIn ? "推荐歌单" : "个性推荐歌单" }}</h2>
            <span class="dim">{{ feed.playlists.length }} 个</span>
          </div>
          <div class="pl-grid">
            <button
              v-for="p in feed.playlists"
              :key="p.id"
              class="pl-card"
              :disabled="openingId === p.id"
              :title="p.name"
              @click="openCloudPlaylist(p)"
            >
              <span class="pl-cover">
                <CoverArt :cover-url="p.coverUrl" size="100%" radius="var(--r-md, 14px)" />
                <span class="plays">
                  <Icon name="play" :size="11" />
                  {{ playCount(p.playCount) }}
                </span>
                <span v-if="openingId === p.id" class="opening">解析中…</span>
              </span>
              <span class="pl-name">{{ p.name }}</span>
              <span v-if="p.copywriter" class="pl-sub">{{ p.copywriter }}</span>
            </button>
          </div>
        </section>

        <section v-if="toplists.length" class="sec">
          <div class="sec-head">
            <h2>排行榜</h2>
            <span class="dim">官方榜单 · 免登录可用</span>
          </div>
          <div class="pl-grid toplist-grid">
            <button
              v-for="p in toplists"
              :key="p.id"
              class="pl-card"
              :disabled="openingId === p.id"
              :title="p.name"
              @click="openCloudPlaylist(p)"
            >
              <span class="pl-cover">
                <CoverArt :cover-url="p.coverUrl" size="100%" radius="var(--r-md, 14px)" />
                <span class="plays">
                  <Icon name="play" :size="11" />
                  {{ playCount(p.trackCount) }}
                </span>
                <span v-if="openingId === p.id" class="opening">解析中…</span>
              </span>
              <span class="pl-name">{{ p.name }}</span>
              <span v-if="p.updateFrequency" class="pl-sub">{{ p.updateFrequency }}</span>
            </button>
          </div>
        </section>

        <section class="sec">
          <div class="sec-head">
            <h2>{{ songTitle }}</h2>
            <button class="btn ghost" @click="playTracks(songList)">
              <Icon name="play" :size="14" filled />
              播放全部
            </button>
            <button class="btn ghost" title="按你的口味随机推几首（登录后效果更好）" @click="playFm">
              <Icon name="shuffle" :size="14" />
              私人FM
            </button>
          </div>
          <div class="rows">
            <button
              v-for="(t, i) in songList.slice(0, 12)"
              :key="t.id"
              class="row"
              :class="{ current: player.snap.track?.id === t.id }"
              @click="playTracks(songList, i)"
            >
              <CoverArt
                :cover-path="t.coverPath"
                :cover-url="t.coverUrl"
                size="42px"
                radius="10px"
              />
              <span class="row-text">
                <b>{{ t.title }}</b>
                <small>{{ t.artist }}<template v-if="t.album"> · {{ t.album }}</template></small>
              </span>
              <span class="row-dur mono">{{
                t.durationMs ? `${Math.floor(t.durationMs / 60000)}:${String(Math.floor((t.durationMs % 60000) / 1000)).padStart(2, "0")}` : ""
              }}</span>
              <Icon name="play" :size="15" class="row-go" />
            </button>
            <div v-if="!songList.length" class="dim pad">
              这里还空着——去上面搜几首喜欢的歌吧
            </div>
          </div>
        </section>

        <p v-if="!feed?.loggedIn" class="dim login-hint">
          登录后可同步账号歌单与「我喜欢的音乐」：
          <button class="link" @click="ui.go('account')">去账号页</button>
        </p>
      </template>
    </div>
  </div>
</template>

<style scoped>
.home {
  display: flex;
  flex-direction: column;
  gap: 4px;
  height: 100%;
  min-height: 0;
}
/* 滚动区：推荐内容超出一屏时在这里上下滚（页头/搜索框固定） */
.body {
  flex: 1;
  min-height: 0;
  padding-bottom: 16px;
}
.head {
  display: flex;
  align-items: flex-end;
  justify-content: space-between;
  gap: 18px;
  flex-wrap: wrap;
}
.hi {
  color: var(--accent);
  font-weight: 700;
}

.search-box {
  display: flex;
  align-items: center;
  gap: 10px;
  width: min(380px, 42vw);
  height: 42px;
  padding: 0 16px;
  border-radius: 999px;
  color: var(--text-3);
  background: color-mix(in srgb, var(--panel) 82%, transparent);
  border: 1px solid var(--border);
  transition: border-color 0.15s, background 0.15s, box-shadow 0.15s;
}
.search-box:focus-within {
  border-color: var(--accent);
  background: var(--panel);
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 14%, transparent);
}
.search-input {
  flex: 1;
  min-width: 0;
  border: none;
  background: none;
  outline: none;
  color: var(--text);
  font-size: 13.5px;
}
.search-input::placeholder {
  color: var(--text-3);
}
.clear {
  display: grid;
  place-items: center;
  width: 22px;
  height: 22px;
  border-radius: 50%;
  color: var(--text-3);
  background: var(--hover);
  flex: none;
}
.clear:hover {
  color: var(--text);
}
.search-box kbd {
  flex: none;
  font-size: 10px;
  padding: 2px 8px;
  border-radius: 999px;
  color: var(--text-3);
  background: var(--hover);
  border: 1px solid var(--border);
}
.kw {
  font-size: 13px;
  font-weight: 500;
  color: var(--text-3);
  margin-left: 6px;
  max-width: 320px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  display: inline-block;
  vertical-align: bottom;
}

.sec {
  margin-top: 18px;
}
.sec-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 12px;
}
.sec-head h2 {
  font-size: 16px;
  font-weight: 700;
}

.pl-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(132px, 1fr));
  gap: 14px;
}
.pl-card {
  display: flex;
  flex-direction: column;
  gap: 7px;
  text-align: left;
}
.pl-card:active {
  transform: scale(0.98);
}
.pl-cover {
  position: relative;
  aspect-ratio: 1;
  border-radius: var(--r-md, 14px);
  overflow: hidden;
  box-shadow: var(--shadow);
}
.pl-cover :deep(.cover) {
  width: 100%;
  height: 100%;
}
.plays {
  position: absolute;
  top: 8px;
  right: 8px;
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 2px 8px;
  border-radius: 999px;
  font-size: 10px;
  color: #fff;
  background: rgba(0, 0, 0, 0.45);
  backdrop-filter: blur(6px);
}
.opening {
  position: absolute;
  inset: 0;
  display: grid;
  place-items: center;
  font-size: 12px;
  color: #fff;
  background: rgba(0, 0, 0, 0.45);
}
.pl-name {
  font-size: 12.5px;
  font-weight: 600;
  color: var(--text);
  overflow: hidden;
  text-overflow: ellipsis;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
}
.pl-sub {
  font-size: 10.5px;
  color: var(--text-3);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.rows {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.row {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 7px 10px;
  border-radius: 12px;
  text-align: left;
  color: var(--text-2);
}
.row:hover {
  background: var(--hover);
  color: var(--text);
}
.row.current {
  color: var(--accent);
}
.row-text {
  display: flex;
  flex-direction: column;
  gap: 2px;
  flex: 1;
  min-width: 0;
}
.row-text b {
  font-size: 13.5px;
  color: inherit;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.row-text small {
  font-size: 11.5px;
  color: var(--text-3);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.row-dur {
  font-size: 11px;
  color: var(--text-3);
}
.row-go {
  opacity: 0;
  color: var(--text-3);
  transition: opacity 0.12s;
}
.row:hover .row-go {
  opacity: 1;
}

.login-hint {
  margin-top: 20px;
  font-size: 12.5px;
}
.link {
  color: var(--accent);
  text-decoration: underline;
  font-size: 12.5px;
}
.pad {
  padding: 18px 4px;
}
</style>
