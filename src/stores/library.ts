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
  /** 已缓存的在线曲目（后端按真实缓存文件过滤），与本地曲库是两套语义 */
  const cachedTracks = ref<Track[]>([]);
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

  /** 云端红心（插件曲目），以完整 Track.id（"netease:<songId>"）为键。
   *  必须声明在 `favoriteIds` 之前：红心显示取「本地收藏 ∪ 云端红心」的并集。 */
  const cloudLiked = ref<Set<string>>(new Set());

  const localFavoriteIds = computed(() => new Set(favorites.value.map((t) => t.id)));

  /** 红心按钮的显示集合：本地收藏 ∪ 云端红心。
   *  列表组件只拿到一个 Set，它必须和 `isFav()` 完全一致，否则会出现
   *  「图标是空心、点一下却是在取消」的错位。 */
  const favoriteIds = computed(() => new Set([...localFavoriteIds.value, ...cloudLiked.value]));

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

  /** 拉完整曲库（`source=local`）。
   *
   *  后端 `library_list` 单次最多返回 1000 条（page_size clamp），旧代码只
   *  请求第 0 页（默认 300 条），超过的部分在曲库页看不见也搜不到。
   *  这里逐页拉完再交给 UI——搜索、排序、计数都基于这份完整数组。
   *  页数上限只是防后端异常时死循环（1000 × 200 = 20 万首）。 */
  async function loadAllTracks(): Promise<Track[]> {
    const pageSize = 1000;
    const all: Track[] = [];
    for (let page = 0; page < 200; page++) {
      const chunk = await library.list(undefined, "local", page, pageSize);
      all.push(...chunk);
      if (chunk.length < pageSize) break;
    }
    return all;
  }

  async function refresh() {
    loading.value = true;
    try {
      const [list, favs, recents, lists, cached] = await Promise.all([
        loadAllTracks(),
        library.favorites(),
        library.recent(80),
        playlistApi.list(),
        // 缓存查询失败不该拖垮整个曲库刷新（老数据 / 权限异常时退化为空）
        library.cachedTracks().catch(() => [] as Track[]),
      ]);
      tracks.value = list;
      favorites.value = favs;
      recent.value = recents;
      playlists.value = lists;
      cachedTracks.value = cached;
      if (activePlaylist.value) {
        await openPlaylist(activePlaylist.value.id);
      }
    } finally {
      loading.value = false;
      ready.value = true;
    }
  }

  /** 单独刷新缓存列表：下载完成、清缓存、文件被外部删除后，
   *  移动端「我的」和缓存列表重新进入时都能拿到最新结果。 */
  async function refreshCached() {
    try {
      cachedTracks.value = await library.cachedTracks();
    } catch {
      // 保持原值：一次查询失败不清空已有列表
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

  /** 按 id 在已加载的曲目里找完整对象：列表页只传 id，但插件曲目必须拿到
   *  `source.pluginId` / `pluginTrackId` 才能同步云端红心。 */
  function findTrack(id: string): Track | undefined {
    for (const list of [
      tracks.value,
      favorites.value,
      recent.value,
      cachedTracks.value,
      playlistTracks.value,
    ]) {
      const hit = list.find((x) => x.id === id);
      if (hit) return hit;
    }
    return undefined;
  }

  /** 收藏入口（列表页 / 收藏页 / 播放条共用）。
   *
   *  历史上列表页只传 trackId，插件曲目永远走不到云端红心那条路：本地取消
   *  了收藏、网易云仍然是红心。这里能按 id 找到完整曲目就统一走
   *  `toggleFavoriteTrack`，找不到才退化为纯本地写入。 */
  async function toggleFavorite(target: string | Track): Promise<boolean> {
    if (typeof target !== "string") return toggleFavoriteTrack(target);
    const known = findTrack(target);
    if (known) return toggleFavoriteTrack(known);
    return setLocalFavorite(target, !localFavoriteIds.value.has(target));
  }

  // -------------------------------------------------------------------------
  // 云端红心（网易云插件 2.14.0）
  //
  // 插件曲目的「喜欢」以云端红心为准（不用先入库，点了立即进网易云
  // 「我喜欢的音乐」）；本地库已有的曲目同时写本地收藏，保证收藏页一致。
  // cloudLiked 以完整 Track.id（"netease:<songId>"）为键缓存云端状态。
  // -------------------------------------------------------------------------

  /** cloudLiked 必须整体替换（Set 的 add/delete 不触发 Vue 响应）。 */
  function cloudMark(trackKey: string, on: boolean) {
    const next = new Set(cloudLiked.value);
    if (on) next.add(trackKey);
    else next.delete(trackKey);
    cloudLiked.value = next;
  }

  /** 统一的收藏判定：本地收藏 ∪ 云端红心。 */
  function isFav(t: Track | null): boolean {
    return !!t && favoriteIds.value.has(t.id);
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

  /** 曲目是否真在本地库里。收藏关系挂在 tracks 表上（外键约束），
   *  没入库的插件曲目不能凭空写本地收藏。 */
  async function inLocalLibrary(trackId: string): Promise<boolean> {
    if (findTrack(trackId)) return true;
    try {
      return (await library.track(trackId)) !== null;
    } catch {
      return false;
    }
  }

  /** 本地收藏按目标状态写（幂等），落库后重拉收藏列表保证显示一致。 */
  async function setLocalFavorite(trackId: string, want: boolean): Promise<boolean> {
    const on = await library.setFavorite(trackId, want);
    favorites.value = await library.favorites();
    return on;
  }

  /**
   * 红心（播放条 / 播放页 / 各列表页共用）。
   *
   * 先按 `isFav()` 算出**目标状态**再写，不用 toggle：本地收藏与云端红心
   * 可能本来就不同步（在网易云 App 里红心过、本地没入库），盲 toggle 会把
   * 用户想去的方向翻反；连点两次也会变成一来一回的两次云端请求。
   *
   * 返回最终状态（与 `isFav()` 一致）；确实没做成时抛错，由调用方提示，
   * 不用「返回值没变」冒充成功。
   */
  async function toggleFavoriteTrack(t: Track | null): Promise<boolean> {
    if (!t) return false;
    const want = !isFav(t);

    // 本地曲目：只有本地收藏这一套状态
    if (t.source.kind !== "plugin" || !t.pluginTrackId) {
      return setLocalFavorite(t.id, want);
    }

    const { pluginId } = t.source;
    const pluginTrackId = t.pluginTrackId;
    // 云端和本地都要写，先确认这首歌在不在库里（外键约束，不在库不能写）
    const known = await inLocalLibrary(t.id);

    /** 云端是否写成功；null = 没写成（未登录 / 网络失败 / 插件拒绝） */
    let cloudOn: boolean | null = null;
    let notLoggedIn = false;
    /** 插件明确拒绝时的说明（版权 / 风控等），不能退化成「本地收藏成功」 */
    let rejected: string | null = null;
    try {
      const r = await pluginApi.likeTrack(pluginId, pluginTrackId, want);
      if (r.ok) {
        // 以插件返回的状态为准：服务端可能已处于目标状态
        cloudOn = r.liked;
        cloudMark(t.id, r.liked);
      } else if (r.code === 301) {
        notLoggedIn = true;
      } else {
        rejected = r.message || "红心被音源拒绝";
      }
    } catch {
      // 网络失败
    }

    if (cloudOn !== null) {
      // 云端成功：库里有的曲目同步到同一状态，收藏页才不会和红心对不上
      if (known) return setLocalFavorite(t.id, cloudOn);
      return cloudOn;
    }

    // 插件明确拒绝：本地也不写，否则界面显示已收藏、云端其实没有
    if (rejected) throw new Error(rejected);

    // 未登录 / 网络失败：本地库里有这首歌就退化为本地收藏，保证离线可用
    if (!known) {
      throw new Error(
        notLoggedIn ? "未登录，且这首歌还没加入曲库，无法收藏" : "红心同步失败，请稍后重试",
      );
    }
    return setLocalFavorite(t.id, want);
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

  /** 重命名歌单（前端入口；后端 rename_playlist 拒系统歌单）。
   *  本地 active 若是它，名字也跟着刷新，免得详情页头还是旧名。 */
  async function renamePlaylist(id: string, name: string) {
    await playlistApi.rename(id, name);
    playlists.value = await playlistApi.list();
    if (activePlaylist.value?.id === id) {
      activePlaylist.value = playlists.value.find((p) => p.id === id) ?? null;
    }
  }

  /** 更新云端歌单：按 remoteKey（pluginId:cloudId）重新拉云端曲目，
   *  按云端顺序原子重建本地曲目（与「覆盖导入」同一条路径），再更新封面。
   *  返回新增/移除数量供 UI 提示；无 remoteKey 的本地歌单返回 null。 */
  async function updatePlaylist(
    id: string,
  ): Promise<{ added: number; removed: number; total: number } | null> {
    const p = playlists.value.find((x) => x.id === id);
    if (!p?.remoteKey) return null;
    const sep = p.remoteKey.indexOf(":");
    if (sep <= 0) return null;
    const pluginId = p.remoteKey.slice(0, sep);
    const cloudId = p.remoteKey.slice(sep + 1);

    const detail = await pluginApi.cloudPlaylist(pluginId, cloudId);
    if (!detail.tracks.length) throw new Error("云端歌单没有拿到曲目");

    // 先写元数据，播放时才能按 id 解析到流；这一步会发 library_changed
    await library.upsertTracks(detail.tracks);

    const local = await playlistApi.tracks(id);
    const localIds = new Set(local.map((t) => t.id));
    const cloudIds = new Set(detail.tracks.map((t) => t.id));
    const added = detail.tracks.filter((t) => !localIds.has(t.id)).length;
    const removed = local.filter((t) => !cloudIds.has(t.id)).length;

    // 按云端顺序原子重建整张歌单。差量 add 会把新歌追加到末尾，
    // 云端调整过曲目顺序时本地就对不上；replaceTracks 与覆盖导入同一路径，
    // 保证「更新歌单」和「覆盖导入」得到完全一致的顺序。
    await playlistApi.replaceTracks(
      id,
      detail.tracks.map((t) => t.id),
    );
    if (detail.coverUrl && detail.coverUrl !== p.coverUrl) {
      await playlistApi.setCover(id, detail.coverUrl);
    }

    // 命令层不发 library_changed，主动刷新；若当前打开的就是它，曲目也重读
    playlists.value = await playlistApi.list();
    if (activePlaylist.value?.id === id) {
      activePlaylist.value = playlists.value.find((x) => x.id === id) ?? null;
      playlistTracks.value = await playlistApi.tracks(id);
    }
    return { added, removed, total: detail.tracks.length };
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
    cachedTracks,
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
    refreshCached,
    scan,
    toggleFavorite,
    isFav,
    refreshCloudLiked,
    toggleFavoriteTrack,
    openPlaylist,
    createPlaylist,
    renamePlaylist,
    updatePlaylist,
    deletePlaylist,
    addToPlaylist,
  };
});
