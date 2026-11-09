/** 与 Rust 侧 `models.rs` 一一对应的类型定义。 */

export type RepeatMode = "off" | "all" | "one";

export type PlaybackStatus =
  | "idle"
  | "buffering"
  | "playing"
  | "paused"
  | "stopped"
  | "error";

export type TrackSource =
  | { kind: "local" }
  | { kind: "plugin"; pluginId: string }
  | { kind: "stream" };

export interface Track {
  id: string;
  source: TrackSource;
  title: string;
  artist: string;
  album: string;
  durationMs: number;
  coverPath?: string | null;
  coverUrl?: string | null;
  filePath?: string | null;
  pluginTrackId?: string | null;
  format?: string | null;
  bitrate?: number | null;
  sampleRate?: number | null;
  channels?: number | null;
  addedAt: number;
  /** 本次解析实际请求的档位（智能音质降级重试时由 Rust 填入，普通播放不出现） */
  qualityHint?: string;
}

/** 插件实际下发的音质（不是用户选的档位）。官方接口常静默降级。 */
export interface StreamQuality {
  /** official / resolved / trial / plugin */
  source: string;
  /** bps */
  bitrate: number | null;
  /** mp3 / flac / aac ... */
  format: string | null;
  trial: boolean;
  /** 用户所选档位 */
  requested: string | null;
  /** 实际规格低于所选档位 */
  downgraded: boolean;
  /** 宿主用来在切歌后丢弃旧规格，UI 不展示 */
  trackId?: string;
}

export interface PlayerSnapshot {
  status: PlaybackStatus;
  track: Track | null;
  positionMs: number;
  durationMs: number;
  volume: number;
  speed: number;
  pitchSemitones: number;
  gainDb: number;
  repeat: RepeatMode;
  shuffle: boolean;
  queueIndex: number | null;
  queueLen: number;
  outputSampleRate: number;
  /** 当前曲目的源文件采样率（0 = 没有活动会话）；与输出采样率分开显示 */
  sourceSampleRate?: number;
  error?: string | null;
  /** 在线曲目的真实规格；本地文件为 null */
  quality?: StreamQuality | null;
  revision: number;
}

/** 音频输出自检，设置页用来定位「进度条在走但没声音」。 */
export interface AudioDiag {
  volume: number;
  ducked: boolean;
  device: string;
  sampleRate: number;
  channels: number;
  playedFrames: number;
  underruns: number;
  bufferedSamples: number;
  deviceError?: string | null;
  hint: string;
}

export interface QueueSnapshot {
  items: Track[];
  current: number | null;
  repeat: RepeatMode;
  shuffle: boolean;
}

export interface LyricWord {
  startMs: number;
  endMs: number;
  text: string;
}

export interface LyricLine {
  startMs: number;
  endMs: number;
  text: string;
  words: LyricWord[];
  translation?: string | null;
}

export interface Lyrics {
  kind: "plain" | "wordByWord";
  title?: string | null;
  artist?: string | null;
  album?: string | null;
  offsetMs: number;
  lines: LyricLine[];
}

export interface Playlist {
  id: string;
  name: string;
  coverPath?: string | null;
  /** 云端导入时记下的网络封面；本地歌单回退用第一首曲目的封面 */
  coverUrl?: string | null;
  /** 云端来源（`pluginId:cloudId`），用于「已导入过」检测 */
  remoteKey?: string | null;
  trackCount: number;
  createdAt: number;
  updatedAt: number;
  system: boolean;
}

export interface PluginConfigField {
  key: string;
  label: string;
  fieldType: string;
  default?: string | null;
  required: boolean;
  placeholder?: string | null;
}

export interface PluginManifest {
  id: string;
  name: string;
  version: string;
  api: number;
  description: string;
  author: string;
  main: string;
  capabilities: string[];
  config: PluginConfigField[];
}

export interface PluginInfo {
  manifest: PluginManifest;
  enabled: boolean;
  installed: boolean;
  builtin: boolean;
  config: Record<string, string>;
  error?: string | null;
}

/** 扫码登录：一张待扫的二维码 */
export interface PluginLoginQr {
  /** 会话 key，轮询时原样传回 */
  key: string;
  /** 二维码里编码的内容 */
  url: string;
  /** 建码时的浏览器会话 Cookie，轮询必须原样带回 */
  session: string;
  /** 宿主渲染好的 SVG，直接丢进 DOM */
  qrSvg: string;
}

export interface PluginLoginState {
  /** blocked = 网易云风控或异常码，message 里有原因 */
  state: "waiting" | "scanned" | "success" | "expired" | "blocked";
  message?: string | null;
}

/** 发送短信验证码的结果（验证码登录第一步） */
export interface PluginCaptchaSent {
  /** sent / failed；失败时 message 里有服务端原因（格式错误、频率限制） */
  state: "sent" | "failed";
  message?: string | null;
  /** 发码时的浏览器会话指纹，登录时原样带回 */
  session: string;
}

export interface PluginAccount {
  loggedIn: boolean;
  /** 网易云用户 id；未登录为 0。拉账号歌单 / 判断登录态用 */
  userId: number;
  nickname: string;
  avatarUrl: string;
  /** 0 无 / 10 音乐包 / 11 黑胶 VIP */
  vipType: number;
  vipLabel: string;
}

/** 云端歌单元数据（推荐流 / 账号歌单里的项） */
export interface RemotePlaylistMeta {
  id: string;
  name: string;
  coverUrl: string | null;
  trackCount: number;
  playCount: number;
  copywriter: string;
  /** 排行榜的更新频率（如「每天更新」）；推荐流 / 账号歌单里为空 */
  updateFrequency?: string;
}

/** 红心操作结果（网易云插件 2.14.0） */
export interface LikeResult {
  ok: boolean;
  code: number;
  liked: boolean;
  playlistId: string;
  message?: string;
}

/** 红心按钮初始状态 */
export interface LikeCheck {
  loggedIn: boolean;
  liked: boolean;
}

/** 插件推荐聚合：登录后含每日推荐歌单+歌曲，未登录只有个性推荐歌单 */
export interface RecommendFeed {
  loggedIn: boolean;
  userId: number;
  nickname: string;
  playlists: RemotePlaylistMeta[];
  tracks: Track[];
}

/** 账号歌单：`liked` 是服务端固定排第一的「我喜欢的音乐」 */
export interface UserPlaylists {
  loggedIn: boolean;
  userId: number;
  liked: RemotePlaylistMeta | null;
  playlists: RemotePlaylistMeta[];
}

/** 云端歌单详情（导入用），tracks 已是可直接入库/播放的 Track */
export interface CloudPlaylist {
  id: string;
  name: string;
  coverUrl: string | null;
  trackCount: number;
  tracks: Track[];
}

export interface Settings {
  theme: "system" | "light" | "dark";
  accent: string;
  volume: number;
  speed: number;
  pitch: number;
  gainDb: number;
  repeat: RepeatMode;
  shuffle: boolean;
  libraryPaths: string[];
  lyricFontSize: number;
  lyricAlign: "left" | "center";
  lyricShowTranslation: boolean;
  closeToTray: boolean;
  /** 播放页背景来源：cover = 当前曲目封面，off = 关闭 */
  nowBgSource: "cover" | "off";
  /** 播放页封面背景模糊半径（px），0 为不模糊 */
  nowBgBlur: number;
  /** 桌面歌词：锁定后窗口鼠标穿透（只能从设置页解除） */
  dlLocked: boolean;
  /** 桌面歌词：排版方向 */
  dlDirection: "horizontal" | "vertical";
  /** 桌面歌词：对齐方式 */
  dlAlign: "left" | "center" | "right";
  /** 桌面歌词：双行歌词（上一句 + 下一句交替，开启后隐藏翻译） */
  dlDualLine: boolean;
  /** 桌面歌词：字号（px） */
  dlFontSize: number;
  /** 桌面歌词：颜色模式 theme = 主题渐变，custom = 自定义纯色 */
  dlColorMode: "theme" | "custom";
  /** 桌面歌词：歌词颜色（自定义模式，#RRGGBB[AA]） */
  dlColorLyric: string;
  /** 桌面歌词：翻译/旁行颜色（自定义模式） */
  dlColorTrans: string;
  /** 桌面歌词：窗口置顶 */
  dlAlwaysOnTop: boolean;
  /** 「流媒体」板块总开关：默认关闭，关闭时隐藏配置并停用三个自建服务插件 */
  streamEnabled: boolean;
  /** 智能音质：播放失败时自动降一级音质重试，默认开启 */
  smartQuality: boolean;
  /** 记住搜索平台：重启后恢复上次选中的搜索音源，默认开启 */
  rememberSearchSource: boolean;
  /** 自定义软件背景：总开关 */
  bgEnabled: boolean;
  /** 已保存的背景图文件名，空串表示未选择 */
  bgImage: string;
  /** 背景图不透明度百分比 5–100 */
  bgOpacity: number;
  cacheLimitMb: number;
  crossfadeMs: number;
}

export interface PlatformInfo {
  kind: "windows" | "android" | "other";
  osVersion: string;
  arch: string;
  supportsLyricWindow: boolean;
  supportsTray: boolean;
  supportsGlobalShortcut: boolean;
  supportsMediaSession: boolean;
  cacheDir: string;
}

/** Android 通知栏 / 锁屏媒体控制的实时状态（桌面端全 false）。 */
export interface MediaStatus {
  notificationGranted: boolean;
  sessionActive: boolean;
  foreground: boolean;
  notificationControl: boolean;
  lockscreenControl: boolean;
}

export interface ScanSummary {
  scanned: number;
  added: number;
  updated: number;
  failed: number;
  errors: string[];
}

const QUALITY_NAMES: Record<string, string> = {
  standard: "标准",
  higher: "较高",
  exhigh: "极高",
  lossless: "无损",
  hires: "Hi-Res",
  jymaster: "高清臻音",
};

export function qualityName(level?: string | null): string {
  return level ? QUALITY_NAMES[level] || level : "";
}

/** 实际拿到的规格。没有码率信息时不编造数字。 */
export function qualityLabel(q: StreamQuality): string {
  const fmt = (q.format || "").toLowerCase();
  if (fmt === "flac") return "FLAC 无损";
  const kbps = q.bitrate ? Math.round(q.bitrate / 1000) : 0;
  if (!kbps) return fmt ? fmt.toUpperCase() : "在线音源";
  const ext = fmt && fmt !== "mp3" ? fmt.toUpperCase() : "MP3";
  return `${kbps}k ${ext}`;
}

/** 播放条/播放页的短音质徽标：LQ(≤128k) / SQ(192k) / HQ(≥320k) / FLAC(无损) /
 *  Hi-Res(无损高码率或所选档位 hires)。text 只留最短标识 + 试听角标，
 *  详细规格与来源挪进悬浮说明；颜色由 UI 按 tone 区分。 */
export type QualityTone = "lq" | "sq" | "hq" | "flac" | "hires";

export function qualityTag(q?: StreamQuality | null) {
  if (!q || !q.source) return null;
  const origin =
    q.source === "resolved"
      ? "解析"
      : q.source === "trial"
        ? "试听"
        : q.source === "official"
          ? "官方"
          : "插件";
  const fmt = (q.format || "").toLowerCase();
  const kbps = q.bitrate ? Math.round(q.bitrate / 1000) : 0;

  let text = "在线";
  let tone: QualityTone = "lq";
  if (fmt === "flac") {
    if (q.requested === "hires" || q.requested === "jymaster" || kbps >= 1200) {
      text = "Hi-Res";
      tone = "hires";
    } else {
      text = "FLAC";
      tone = "flac";
    }
  } else if (kbps >= 320) {
    text = "HQ";
    tone = "hq";
  } else if (kbps >= 192) {
    text = "SQ";
    tone = "sq";
  } else if (kbps > 0) {
    text = "LQ";
    tone = "lq";
  } else {
    text = fmt ? fmt.toUpperCase() : "在线";
    tone = "lq";
  }
  if (q.trial) text += "·试听";

  const want = q.requested ? QUALITY_NAMES[q.requested] || q.requested : "";
  const title = [
    `实际音质：${qualityLabel(q)}`,
    `来源：${origin}`,
    want ? `所选档位：${want}` : "",
    q.downgraded ? "所选档位未生效，已按实际规格播放" : "",
    "输出统一重采样到 48 kHz",
  ]
    .filter(Boolean)
    .join(" · ");
  return {
    text,
    tone,
    title,
    warn: q.downgraded || q.trial,
  };
}

export const emptySnapshot = (): PlayerSnapshot => ({
  status: "idle",
  track: null,
  positionMs: 0,
  durationMs: 0,
  volume: 0.85,
  speed: 1,
  pitchSemitones: 0,
  gainDb: 0,
  repeat: "all",
  shuffle: false,
  queueIndex: null,
  queueLen: 0,
  outputSampleRate: 0,
  error: null,
  quality: null,
  revision: 0,
});

export const emptyLyrics = (): Lyrics => ({
  kind: "plain",
  lines: [],
  offsetMs: 0,
});
