//! 音源插件协议（API v1）。
//!
//! # 目录结构
//!
//! ```text
//! plugins/<id>/
//!   manifest.json
//!   index.js
//! ```
//!
//! # index.js 约定
//!
//! 脚本在沙箱里执行，需要定义若干**全局函数**，宿主按名字调用：
//!
//! ```js
//! function search(keyword, page, limit) { return [ { id, title, artist, album, durationMs, coverUrl } ] }
//! function getStreamUrl(track) { return "https://..." }
//! function getLyric(track) { return "[00:01.00]..." }   // 可选
//! function getPlaylist(id) { return { name, tracks: [...] } }  // 可选
//! ```
//!
//! `getStreamUrl` 也可以返回对象，用来告诉用户**实际拿到**的音质（官方接口常
//! 静默降级，只写档位会骗人）：
//!
//! ```js
//! function getStreamUrl(track) {
//!   return {
//!     url: "https://...",
//!     source: "official" | "resolved" | "trial" | "plugin",
//!     bitrate: 320000,      // bps，未知用 null
//!     format: "mp3",        // mp3 / flac / aac ...
//!     trial: false,
//!     requested: "lossless",// 用户选的档位
//!     downgraded: true      // 实际规格低于 requested
//!   }
//! }
//! ```
//! 宿主兼容两种返回：字符串按「无规格信息」处理，对象里除 `url` 外的字段
//! 缺省都是安全的。规格会进 `PlayerSnapshot.quality`，由播放条显示。
//!
//! ## 登录（capabilities 里声明 `login` 才需要）
//!
//! ```js
//! function loginQrCreate() { return { key: "会话 key", url: "手机扫码用的 url", session?: "a=1; b=2" } }
//! function loginQrPoll(key, session?) {
//!   return { state: "waiting" | "scanned" | "success" | "expired" | "blocked",
//!            cookie?: "a=1; b=2", message?: "给用户看的原因" }
//! }
//! function loginInfo() { return { loggedIn, nickname, avatarUrl, vipType, vipLabel } }
//! ```
//!
//! 宿主负责：把 `url` 渲染成二维码给用户扫、每 3 秒调一次 `loginQrPoll`、
//! 在 `state === "success"` 时把返回的 `cookie` **写进插件配置项 `cookie`**，
//! 之后每次调用插件都会带上它。退出登录就是删掉那一项。
//! `session` 是建码时的浏览器会话 Cookie，轮询必须原样带回，否则服务端
//! 会把它当成没有指纹的第三方请求（手机端报「设备环境异常」/ `8821`）。
//! `blocked` 表示账号级风控或异常码：停止轮询并把 `message` 展示给用户，
//! 同时提供不依赖扫码的 Cookie 登录入口。
//!
//! 宿主注入 `leon` 对象：
//! * `leon.config.get(key)` → string
//! * `leon.http.get(url)` → string
//! * `leon.http.getEx(url, headers?)` → `{ status, headers, cookies, body }`
//!   —— 登录前预热会话用（`Set-Cookie` 会解析进 `cookies`）
//! * `leon.http.post(url, body)` → string（`Content-Type: application/json`）
//! * `leon.http.postForm(url, body, headers?)` → string（form-urlencoded）
//! * `leon.http.postFormEx(url, body, headers?)` → `{ status, headers, cookies, body }`
//!   —— 多返回响应头与已解析的 `Set-Cookie`，扫码登录靠它拿 `MUSIC_U`
//! * `leon.crypto.aesCbcB64(text, key, iv)` / `aesEcbHex(text, key)`
//! * `leon.crypto.rsaNoPadHex(text, pem)` / `randomB64(n)`
//! * `leon.log(msg)`
//! * `leon.md5(s)` / `leon.sha1(s)` / `leon.base64(s)`
//!
//! # 鉴权怎么办
//!
//! `leon.http.postForm` 支持自定义请求头（网易云要 `Referer` / `Cookie`）。
//! 其余服务（Jellyfin / Emby / Subsonic）把凭据放 query 更省事：
//! `?api_key=...` 或 `?u=..&t=..&s=..`，它们都原生支持。
//! 代价是 token 会出现在 URL 里——自建服务请走 HTTPS。
//!
//! # 安全边界
//!
//! * 没有 `require` / `import` / 文件系统 / 进程 API
//! * 只能通过 `leon.http` 发请求，域名由用户配置决定
//! * 单次调用有指令数上限（防止 `while(true)` 卡死引擎）
//! * 每个插件调用都在 `spawn_blocking` 线程里，不阻塞 UI 与音频线程

use serde::{Deserialize, Serialize};

use crate::models::PluginManifest;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTrack {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub duration_ms: i64,
    #[serde(default)]
    pub cover_url: Option<String>,
}

/// 推荐流 / 账号歌单里的歌单元数据（不含曲目列表）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePlaylistMeta {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub track_count: i64,
    #[serde(default)]
    pub play_count: i64,
    #[serde(default)]
    pub copywriter: String,
    /// 排行榜的更新频率（如「每天更新」）；推荐流 / 账号歌单里为空。
    #[serde(default)]
    pub update_frequency: String,
}

/// `likeTrack(id, like)` 的返回：红心操作结果（网易云插件 2.14.0）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LikeResult {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub code: i64,
    #[serde(default)]
    pub liked: bool,
    #[serde(default)]
    pub playlist_id: String,
    #[serde(default)]
    pub message: String,
}

/// `isLikedTrack(id)` 的返回：红心按钮的初始状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LikeCheck {
    #[serde(default)]
    pub logged_in: bool,
    #[serde(default)]
    pub liked: bool,
}

/// `getPlaylist(id)` 的返回：歌单元数据 + 曲目列表。
/// 协议早有预留，宿主在「账号歌单导入」时正式接入。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePlaylist {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub track_count: i64,
    #[serde(default)]
    pub tracks: Vec<RemoteTrack>,
}

/// `getRecommendFeed(limit)` 的聚合返回：登录后是每日推荐 + 个性推荐，
/// 未登录只有个性推荐歌单（免登录接口）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecommendFeed {
    #[serde(default)]
    pub logged_in: bool,
    #[serde(default)]
    pub user_id: i64,
    #[serde(default)]
    pub nickname: String,
    #[serde(default)]
    pub playlists: Vec<RemotePlaylistMeta>,
    #[serde(default)]
    pub tracks: Vec<RemoteTrack>,
}

/// `getUserPlaylists()` 的返回。网易云固定把「我喜欢的音乐」排在第一项，
/// 插件层已把它单独拆成 `liked`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserPlaylists {
    #[serde(default)]
    pub logged_in: bool,
    #[serde(default)]
    pub user_id: i64,
    #[serde(default)]
    pub liked: Option<RemotePlaylistMeta>,
    #[serde(default)]
    pub playlists: Vec<RemotePlaylistMeta>,
}

pub const API_VERSION: u32 = 1;

/// 校验 manifest 是否可加载。
pub fn validate(manifest: &PluginManifest) -> Result<(), String> {
    if manifest.id.trim().is_empty() {
        return Err("manifest.id is empty".into());
    }
    if !manifest
        .id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("manifest.id may only contain [a-z0-9_-]".into());
    }
    if manifest.api != API_VERSION {
        return Err(format!(
            "unsupported plugin api {} (host supports {API_VERSION})",
            manifest.api
        ));
    }
    if manifest.main.contains("..") {
        return Err("manifest.main must stay inside plugin dir".into());
    }
    Ok(())
}
