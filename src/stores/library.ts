/**
 * 媒体库 store：本地曲目、收藏、歌单、最近播放。
 *
 * 扫描是 Rust 侧的后台任务，这里只负责发起与展示进度。
 */

import { defineStore } from "pinia";
import { computed, ref } from "vue";
import type { UnlistenFn } from "@tauri-apps/api/event";

import { EVT, library, on, playlist as playlistApi, plugins as pluginApi } from "@/api/ipc";
import type { Playlist, Track } from "@/types";

export const useLibraryStore = defineStore("library", () => {
  const tracks = ref<Track[]>([]);
  const favorites = ref<Track[]>([]);
  const recent = ref<Track[]>([]);
  const playlists = ref<Playlist[]>([]);
  const activePlaylist = ref<Playlist | null>(null);
  const playlistTracks = ref<Track[]>([]);
  const loading = ref(false);
  /** 首次 refresh 完成（成功或失败）。启动时曲库不再阻塞首屏，
   *  页面靠它区分「还没读到」和「真的没有歌」。 */
  const ready = ref(false);
  const scanning = ref(false);
  const scanProgress = ref({ done: 0, total: 0 });
  const query = ref("");

  const filtered = computed(() => {
    const q = query.value.trim().toLowerCase();
    if (!q) return tracks.value;
    return tracks.value.filter(
      (t) =>
        t.title.toLowerCase().includes(q) ||
        t.artist.toLowerCase().includes(q) ||
        t.album.toLowerCase().includes(q),
    );
  });

  const favoriteIds = computed(() => new Set(favorites.value.map((t) => t.id)));

  const unlisteners: UnlistenFn[] = [];
  /** 并发兜底：启动时 App 与页面可能同时调用 init，共享同一个请求，
   *  后来者等的是结果而不是被 `initialized` 提前打发走。 */
  let initPromise: Promise<void> | null = null;

  function init() {
    if (!initPromise) {
      initPromise = doInit().catch((e) => {
        // 启动时一次 IPC 失败不该把整条初始化永久钉死：释放缓存，
        // 页面重新挂载时还能再试一次（否则之后所有 init 都拿到同一个拒绝）。
        initPromise = null;
        throw e;
      });
    }
    return initPromise;
  }

  async function doInit() {
    // 先订阅、再拉数据：即使首次读取失败，后面的扫描 / 导入事件也不会丢。
    if (!unlisteners.length) {
      unlisteners.push(
        await on(EVT.libraryChanged, () => void refresh()),
        await on<{ done: number; total: number }>(EVT.scanProgress, (p) => {
          scanProgress.value = p;
        }),
      );
    }
    await refresh();
  }

  async function refresh() {
    loading.value = true;
    try {
      const [list, favs, recents, lists] = await Promise.all([
        library.list(undefined, "local"),
        library.favorites(),
        library.recent(80),
        playlistApi.list(),
      ]);
      tracks.value = list;
      favorites.value = favs;
      recent.value = recents;
      playlists.value = lists;
      if (activePlaylist.value) {
        await openPlaylist(activePlaylist.value.id);
      }
    } finally {
      loading.value = false;
      ready.value = true;
    }
  }

  async function scan(paths: string[]) {
    if (!paths.length) return null;
    scanning.value = true;
    scanProgress.value = { done: 0, total: 0 };
    try {
      const summary = await library.scan(paths);
      await refresh();
      return summary;
    } finally {
      scanning.value = false;
    }
  }

  async function toggleFavorite(trackId: string): Promise<boolean> {
    const nowFavorite = await library.toggleFavorite(trackId);
    favorites.value = await library.favorites();
    // 云端红心联动（网易云插件 2.14.0）：插件曲目在本地收藏/取消的同时，
    // 把红心同步到音源平台（进「我喜欢的音乐」，计入听歌排行）。
    // 未登录时插件返回 ok:false + code:301，静默忽略；失败也不影响本地收藏。
    const t = tracks.value.find((x) => x.id === trackId);
    if (t && t.source.kind === "plugin" && t.pluginTrackId) {
      void pluginApi
        .likeTrack(t.source.pluginId, t.pluginTrackId, nowFavorite)
        .catch(() => {});
    }
    return nowFavorite;
  }

  // -------------------------------------------------------------------------
  // 云端红心（网易云插件 2.14.0）
  //
  // 插件曲目的「喜欢」以云端红心为准（不用先入库，点了立即进网易云
  // 「我喜欢的音乐」）；本地库已有的曲目同时翻本地收藏，保证收藏页一致。
  // cloudLiked 以完整 Track.id（"netease:<songId>"）为键缓存云端状态。
  // -------------------------------------------------------------------------
  const cloudLiked = ref<Set<string>>(new Set());

  /** cloudLiked 必须整体替换（Set 的 add/delete 不触发 Vue 响应）。 */
  function cloudMark(trackKey: string, on: boolean) {
    const next = new Set(cloudLiked.value);
    if (on) next.add(trackKey);
    else next.delete(trackKey);
    cloudLiked.value = next;
  }

  /** 统一的收藏判定：插件曲目看云端红心 ∪ 本地收藏，本地曲目看本地收藏。 */
  function isFav(t: Track | null): boolean {
    if (!t) return false;
    if (t.source.kind === "plugin" && t.pluginTrackId) {
      return cloudLiked.value.has(t.id) || favoriteIds.value.has(t.id);
    }
    return favoriteIds.value.has(t.id);
  }

  /** 切歌时刷新云端红心状态（轻量：一次账号红心 id 列表查询）。 */
  async function refreshCloudLiked(t: Track | null): Promise<void> {
    if (!t || t.source.kind !== "plugin" || !t.pluginTrackId) return;
    try {
      const r = await pluginApi.isLiked(t.source.pluginId, t.pluginTrackId);
      cloudMark(t.id, r.loggedIn && r.liked);
    } catch {
      // 网络失败保持现状，不打断播放
    }
  }

  /** 播放条 / 播放页的红心：插件曲目走云端红心，本地曲目走本地收藏。 */
  async function toggleFavoriteTrack(t: Track | null): Promise<boolean> {
    if (!t) return false;
    if (t.source.kind === "plugin" && t.pluginTrackId) {
      const want = !isFav(t);
      try {
        const r = await pluginApi.likeTrack(t.source.pluginId, t.pluginTrackId, want);
        if (r.ok) {
          cloudMark(t.id, r.liked);
        } else if (r.code === 301) {
          // 未登录：云端做不了，退化为纯本地收藏
          return toggleFavorite(t.id);
        }
      } catch {
        // 网络失败：本地有这首歌就退化为本地收藏，否则如实反馈失败
        if (!tracks.value.some((x) => x.id === t.id)) return false;
        return toggleFavorite(t.id);
      }
      // 本地已入库的插件曲目同步翻本地收藏（收藏页显示一致）
      if (tracks.value.some((x) => x.id === t.id)) {
        await toggleFavorite(t.id);
      }
      return want;
    }
    return toggleFavorite(t.id);
  }

  async function openPlaylist(id: string) {
    const p = playlists.value.find((x) => x.id === id) ?? null;
    activePlaylist.value = p;
    playlistTracks.value = await playlistApi.tracks(id);
  }

  async function createPlaylist(name: string) {
    const p = await playlistApi.create(name);
    playlists.value = await playlistApi.list();
    return p;
  }

  async function deletePlaylist(id: string) {
    await playlistApi.remove(id);
    if (activePlaylist.value?.id === id) {
      activePlaylist.value = null;
      playlistTracks.value = [];
    }
    playlists.value = await playlistApi.list();
  }

  async function addToPlaylist(id: string, trackIds: string[]) {
    await playlistApi.add(id, trackIds);
    playlists.value = await playlistApi.list();
    if (activePlaylist.value?.id === id) {
      playlistTracks.value = await playlistApi.tracks(id);
    }
  }

  return {
    tracks,
    favorites,
    recent,
    playlists,
    activePlaylist,
    playlistTracks,
    loading,
    ready,
    scanning,
    scanProgress,
    query,
    filtered,
    favoriteIds,
    init,
    refresh,
    scan,
    toggleFavorite,
    isFav,
    refreshCloudLiked,
    toggleFavoriteTrack,
    openPlaylist,
    createPlaylist,
    deletePlaylist,
    addToPlaylist,
  };
});
