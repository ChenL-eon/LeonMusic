/**
 * 唯一的 IPC 入口。
 *
 * 约定：
 * - 前端只能通过这里发命令，禁止在组件里直接 `invoke("xxx")`
 * - 命令一律「发完就忘」，状态从事件流回来
 * - 所有错误统一转成 `{ code, message }`
 */

import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type {
  AudioDiag,
  CloudPlaylist,
  LikeCheck,
  LikeResult,
  Lyrics,
  MediaStatus,
  PlatformInfo,
  PlayerSnapshot,
  Playlist,
  PluginAccount,
  PluginCaptchaSent,
  PluginInfo,
  PluginLoginQr,
  PluginLoginState,
  QueueSnapshot,
  RecommendFeed,
  RemotePlaylistMeta,
  RepeatMode,
  ScanSummary,
  Settings,
  Track,
  UserPlaylists,
} from "@/types";

export interface AppError {
  code: string;
  message: string;
}

export function errorText(e: unknown): string {
  const raw =
    typeof e === "object" && e && "message" in e
      ? String((e as AppError).message)
      : String(e);
  // Rust 侧统一带 `kind: ` 前缀（plugin: / net: / io: …）。
  // `plugin:` 对用户没有信息量，去掉；剩下的 `netease: search: 原因` 保留，
  // 多音源时能看出是哪个插件出的问题。
  return raw.replace(/^plugin: /, "");
}

/** 事件名，与 `src-tauri/src/events.rs` 保持一致。 */
export const EVT = {
  state: "leon://player/state",
  queue: "leon://player/queue",
  libraryChanged: "leon://library/changed",
  scanProgress: "leon://library/scan-progress",
  buffering: "leon://player/buffering",
  bufferProgress: "leon://player/buffer-progress",
  ended: "leon://player/track-ended",
  settingsChanged: "leon://settings/changed",
  updateProgress: "leon://update/progress",
} as const;

export const player = {
  snapshot: () => invoke<PlayerSnapshot>("player_snapshot"),
  queue: () => invoke<QueueSnapshot>("player_queue"),
  toggle: () => invoke<void>("player_toggle"),
  play: () => invoke<void>("player_play"),
  pause: () => invoke<void>("player_pause"),
  stop: () => invoke<void>("player_stop"),
  next: () => invoke<void>("player_next"),
  prev: () => invoke<void>("player_prev"),
  seek: (positionMs: number) => invoke<void>("player_seek", { positionMs }),
  setVolume: (volume: number) => invoke<void>("player_set_volume", { volume }),
  audioDiag: () => invoke<AudioDiag>("player_audio_diag"),
  audioReset: () => invoke<void>("player_audio_reset"),
  setSpeed: (speed: number) => invoke<void>("player_set_speed", { speed }),
  setPitch: (semitones: number) => invoke<void>("player_set_pitch", { semitones }),
  setGain: (gainDb: number) => invoke<void>("player_set_gain", { gainDb }),
  setRepeat: (mode: RepeatMode) => invoke<void>("player_set_repeat", { mode }),
  setShuffle: (enabled: boolean) => invoke<void>("player_set_shuffle", { enabled }),
  playTrack: (trackId: string, startMs = 0) =>
    invoke<void>("player_play_track", { trackId, startMs }),
  playQueue: (trackIds: string[], startIndex = 0) =>
    invoke<void>("player_play_queue", { trackIds, startIndex }),
  /** 用曲目对象整体替换队列：搜索结果的在线曲目还没入库，只有 id 查不到。 */
  playTracks: (tracks: Track[], startIndex = 0) =>
    invoke<void>("player_play_tracks", { tracks, startIndex }),
  addToQueue: (trackIds: string[], playNext = false) =>
    invoke<void>("player_queue_add", { trackIds, playNext }),
  addTracksToQueue: (tracks: Track[], playNext = false) =>
    invoke<void>("player_queue_add_tracks", { tracks, playNext }),
  removeFromQueue: (index: number) => invoke<void>("player_queue_remove", { index }),
  moveQueue: (from: number, to: number) => invoke<void>("player_queue_move", { from, to }),
  clearQueue: () => invoke<void>("player_queue_clear"),
  lyrics: () => invoke<Lyrics>("player_lyrics"),
  fetchRemoteLyrics: () => invoke<Lyrics>("player_fetch_remote_lyrics"),
  /** 桌面歌词窗操作：返回操作后的可见状态，供开关按钮消费 */
  showLyricWindow: () => invoke<boolean>("lyric_window_show"),
  hideLyricWindow: () => invoke<boolean>("lyric_window_hide"),
  toggleLyricWindow: () => invoke<boolean>("lyric_window_toggle"),
  lyricWindowVisible: () => invoke<boolean>("lyric_window_visible"),
};

/** 歌词窗自行关闭（工具条 ✕ / 右键）时 JS 层广播的事件，主窗监听同步开关状态。
 *  纯前端事件，不经过 events.rs。 */
export const LYRIC_VIS_EVENT = "leon://lyric-window/visibility";

export const library = {
  scan: (paths: string[]) => invoke<ScanSummary>("library_scan", { paths }),
    list: (query?: string, source?: string, page = 0, pageSize = 300) =>
      invoke<Track[]>("library_list", { query, source, page, pageSize }),
    count: (query?: string) => invoke<number>("library_count", { query }),
    track: (id: string) => invoke<Track | null>("library_track", { id }),
    toggleFavorite: (trackId: string) => invoke<boolean>("library_toggle_favorite", { trackId }),
    favorites: () => invoke<Track[]>("library_favorites"),
    recent: (limit = 100) => invoke<Track[]>("library_recent", { limit }),
    remove: (id: string) => invoke<void>("library_delete", { id }),
    probeFile: (path: string) => invoke<Track>("library_probe_file", { path }),
    /** 批量写入远程曲目元数据（云端歌单导入第一步），完成会发 library_changed */
    upsertTracks: (tracks: Track[]) => invoke<number>("library_upsert_tracks", { tracks }),
  };

export const playlist = {
  list: () => invoke<Playlist[]>("playlist_list"),
  create: (name: string) => invoke<Playlist>("playlist_create", { name }),
  rename: (id: string, name: string) => invoke<void>("playlist_rename", { id, name }),
  /** 记录歌单封面（云端导入时写入网络封面 URL；传 null 清除） */
  setCover: (id: string, coverUrl: string | null) =>
    invoke<void>("playlist_set_cover", { id, coverUrl }),
  /** 记录歌单云端来源（`pluginId:cloudId`），用于「已导入过」检测 */
  setRemoteKey: (id: string, remoteKey: string | null) =>
    invoke<void>("playlist_set_remote_key", { id, remoteKey }),
  remove: (id: string) => invoke<void>("playlist_delete", { id }),
  tracks: (id: string) => invoke<Track[]>("playlist_tracks", { id }),
  add: (id: string, trackIds: string[]) => invoke<void>("playlist_add", { id, trackIds }),
  removeTracks: (id: string, trackIds: string[]) =>
    invoke<void>("playlist_remove", { id, trackIds }),
};

export const plugins = {
  list: () => invoke<PluginInfo[]>("plugin_list"),
  setEnabled: (id: string, enabled: boolean) =>
    invoke<void>("plugin_set_enabled", { id, enabled }),
  setConfig: (id: string, config: Record<string, string>) =>
    invoke<void>("plugin_set_config", { id, config }),
  search: (pluginId: string, keyword: string, page = 1, limit = 30) =>
    invoke<Track[]>("plugin_search", { pluginId, keyword, page, limit }),
  resolveStream: (pluginId: string, track: Track) =>
    invoke<string>("plugin_resolve_stream", { pluginId, track }),
  lyric: (pluginId: string, track: Track) =>
    invoke<string>("plugin_lyric", { pluginId, track }),
  install: (manifestJson: string, script: string) =>
    invoke<string>("plugin_install", { manifestJson, script }),
  reload: () => invoke<void>("plugin_reload"),
  /** 扫码登录第一步：拿二维码（SVG 由宿主渲染好） */
  loginQr: (pluginId: string) =>
    invoke<PluginLoginQr>("plugin_login_qr", { pluginId }),
  /** 轮询扫码状态，3 秒一次；session 必须与建码时一致 */
  loginPoll: (pluginId: string, key: string, session = "") =>
    invoke<PluginLoginState>("plugin_login_poll", { pluginId, key, session }),
  /** 当前账号；未登录时 loggedIn=false */
  loginInfo: (pluginId: string) =>
    invoke<PluginAccount>("plugin_login_info", { pluginId }),
  logout: (pluginId: string) => invoke<void>("plugin_logout", { pluginId }),
  /** 短信验证码登录第一步：发码（session 由前端保存、登录时带回） */
  captchaSend: (pluginId: string, phone: string, session = "") =>
    invoke<PluginCaptchaSent>("plugin_captcha_send", { pluginId, phone, session }),
  /** 短信验证码登录：成功时宿主已把 cookie 落库 */
  loginCellphone: (pluginId: string, phone: string, captcha: string, session = "") =>
    invoke<PluginLoginState>("plugin_login_cellphone", { pluginId, phone, captcha, session }),
  /** 推荐聚合：登录后含每日推荐歌单+歌曲，未登录只有个性推荐歌单 */
  recommendFeed: (pluginId: string, limit = 12) =>
    invoke<RecommendFeed>("plugin_recommend_feed", { pluginId, limit }),
  /** 账号歌单（liked = 「我喜欢的音乐」），需登录 */
  userPlaylists: (pluginId: string) =>
    invoke<UserPlaylists>("plugin_user_playlists", { pluginId }),
  /** 云端歌单详情（导入用） */
  cloudPlaylist: (pluginId: string, id: string) =>
    invoke<CloudPlaylist>("plugin_cloud_playlist", { pluginId, id }),
  /** 听歌打卡：把一条真实播放同步给音源平台（未登录/已关闭时返回 ok:false） */
  scrobble: (pluginId: string, trackId: string, listenedMs: number) =>
    invoke<{ ok: boolean; code: number; message?: string }>(
      "plugin_scrobble",
      { pluginId, trackId, listenedMs },
    ),
  /** 红心 / 取消红心（进「我喜欢的音乐」）。未登录返回 ok:false + code:301 */
  likeTrack: (pluginId: string, trackId: string, like: boolean) =>
    invoke<LikeResult>("plugin_like_track", { pluginId, trackId, like }),
  /** 查一首歌是否已被红心（红心按钮初始状态） */
  isLiked: (pluginId: string, trackId: string) =>
    invoke<LikeCheck>("plugin_is_liked", { pluginId, trackId }),
  /** 官方排行榜列表（免登录可用）；曲目复用 cloudPlaylist 拉取 */
  toplists: (pluginId: string) =>
    invoke<RemotePlaylistMeta[]>("plugin_toplists", { pluginId }),
  /** 私人FM：按账号口味推歌，登录态一次 3 首 */
  personalFm: (pluginId: string) =>
    invoke<Track[]>("plugin_personal_fm", { pluginId }),
  /** 相似歌曲：播放页「听相似」用 */
  similarSongs: (pluginId: string, trackId: string, limit = 30) =>
    invoke<Track[]>("plugin_similar_songs", { pluginId, trackId, limit }),
};

export const settings = {
  get: () => invoke<Settings>("settings_get"),
  update: (value: Settings) => invoke<Settings>("settings_update", { settings: value }),
  platformInfo: () => invoke<PlatformInfo>("app_platform_info"),
  cacheSize: () => invoke<number>("cache_size"),
  cacheClear: () => invoke<void>("cache_clear"),
  cacheEnforce: () => invoke<void>("cache_enforce_limit"),
  requestBackground: (enabled: boolean) =>
    invoke<void>("request_background_mode", { enabled }),
  /** Android：状态栏/手势条图标跟随主题明暗（桌面空实现） */
  setDarkAppearance: (dark: boolean) =>
    invoke<void>("app_set_dark_appearance", { dark }),
  /** Android：通知权限 / MediaSession / 前台服务 / 两个开关的实时状态 */
  mediaStatus: () => invoke<MediaStatus>("app_media_status"),
  /** Android：切换「通知栏播放控制」与「锁屏播放状态」 */
  setMediaOptions: (notification: boolean, lockscreen: boolean) =>
    invoke<void>("app_set_media_options", { notification, lockscreen }),
  /** Android：跳到系统的应用通知设置页（权限被拒后自救） */
  openNotificationSettings: () => invoke<void>("app_open_notification_settings"),
  dataDir: () => invoke<string>("app_data_dir"),
  /** 已保存背景图的绝对路径（供 convertFileSrc 转成 asset URL），无图时为空串 */
  backgroundPath: () => invoke<string>("app_background_path"),
  /** 传入纯 base64 图片数据，落盘到应用数据目录并广播设置变更 */
  setBackgroundImage: (data: string) => invoke<Settings>("app_set_background_image", { data }),
  /** 删除已保存的背景图 */
  clearBackgroundImage: () => invoke<Settings>("app_clear_background_image"),
};

/** 与插件无关的网络辅助（宿主命令） */
export const net = {
  /** 跟随重定向解析分享短链，返回最终 URL（163cn.tv 等短链拿歌单 id 用） */
  resolveRedirect: (url: string) => invoke<string>("resolve_redirect", { url }),
};

/** 应用壳辅助：外部链接、版本信息。 */
export const app = {
  /** 当前应用版本（tauri.conf.json 的 version） */
  getVersion: () => getVersion(),
  /** 用系统默认浏览器打开 http(s) 链接（「更新与关于」→「前往下载」） */
  openExternal: (url: string) => invoke<void>("open_external", { url }),
  /** 下载更新包到本地缓存，返回落盘路径；进度走 EVT.updateProgress */
  updateDownload: (url: string, fileName: string) =>
    invoke<string>("update_download", { url, fileName }),
  /** 安装已下载的更新包：Windows 替换重启，Android 拉起系统安装器 */
  updateApply: (archive: string) => invoke<void>("update_apply", { archive }),
  /** 取消进行中的下载（半成品会被删除） */
  updateCancel: () => invoke<void>("update_cancel"),
};

/** 应用内更新的下载进度（字节；total 为 0 表示服务端没给长度） */
export interface UpdateProgress {
  downloaded: number;
  total: number;
}

/** 订阅事件，返回取消函数。组件卸载时必须调用。 */
export async function on<T>(event: string, cb: (payload: T) => void): Promise<UnlistenFn> {
  return listen<T>(event, (e) => cb(e.payload));
}
