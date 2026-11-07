//! 插件加载与调用。
//!
//! 插件目录在启动时全部读进内存（manifest + 脚本），调用时不再碰磁盘。
//! 启用状态与配置存在 SQLite，卸载重装也不丢配置。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::models::{PluginInfo, PluginManifest, StreamQuality, Track, TrackSource};

use super::js_runtime;
use super::protocol::{
    validate, RemotePlaylistMeta, RemoteTrack, UserPlaylists,
};

/// 随二进制编译进去的插件。用户目录里没有时用来兜底，
/// 保证「装完就能用」而不是打开发现列表为空。
struct Builtin {
    id: &'static str,
    manifest: &'static str,
    script: &'static str,
}

const BUILTINS: &[Builtin] = &[
    Builtin {
        id: "netease",
        manifest: include_str!("builtin/netease/manifest.json"),
        script: include_str!("builtin/netease/index.js"),
    },
    Builtin {
        id: "navidrome",
        manifest: include_str!("builtin/navidrome/manifest.json"),
        script: include_str!("builtin/navidrome/index.js"),
    },
    Builtin {
        id: "jellyfin",
        manifest: include_str!("builtin/jellyfin/manifest.json"),
        script: include_str!("builtin/jellyfin/index.js"),
    },
    Builtin {
        id: "emby",
        manifest: include_str!("builtin/emby/manifest.json"),
        script: include_str!("builtin/emby/index.js"),
    },
];

struct LoadedPlugin {
    manifest: PluginManifest,
    script: String,
    builtin: bool,
}

/// 登录凭据在插件配置里的键名，协议层约定（见 `protocol.rs`）。
const LOGIN_COOKIE_KEY: &str = "cookie";

/// 解析出来的直链 + 它的真实规格（新协议插件才有规格）。
#[derive(Debug, Clone)]
pub struct StreamUrl {
    pub url: String,
    pub quality: Option<StreamQuality>,
}

/// 推荐聚合（前端可直接展示 / 播放）。插件返回 RemoteTrack，这里统一转成本地 Track。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecommendFeed {
    pub logged_in: bool,
    pub user_id: i64,
    pub nickname: String,
    pub playlists: Vec<RemotePlaylistMeta>,
    pub tracks: Vec<Track>,
}

/// 云端歌单详情（导入用）。tracks 已转成本地 Track，upsert 即可入库。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudPlaylist {
    pub id: String,
    pub name: String,
    pub cover_url: Option<String>,
    pub track_count: i64,
    pub tracks: Vec<Track>,
}

pub struct PluginManager {
    db: Arc<Db>,
    plugins: RwLock<HashMap<String, LoadedPlugin>>,
}

impl PluginManager {
    pub fn init(db: Arc<Db>, roots: Vec<PathBuf>) -> AppResult<Self> {
        let mgr = Self {
            db,
            plugins: RwLock::new(HashMap::new()),
        };
        mgr.seed_builtin(&roots)?;
        mgr.reload(&roots);
        Ok(mgr)
    }

    /// 把内置插件写到用户插件目录（仅当不存在时），方便用户直接改。
    fn seed_builtin(&self, roots: &[PathBuf]) -> AppResult<()> {
        let Some(root) = roots.first() else {
            return Ok(());
        };
        for b in BUILTINS {
            let dir = root.join(b.id);
            std::fs::create_dir_all(&dir)?;
            let manifest_path = dir.join("manifest.json");
            let script_path = dir.join("index.js");

            // 内置插件随版本升级要能覆盖旧脚本，否则用户升级 App 之后
            // 磁盘上还是第一次播种的那份（`if !exists` 永远跳过）。
            // 配置存在 SQLite，覆盖文件不影响用户已填的 cookie。
            // 兜底：除版本号外，磁盘内容与编译内容不一致也强制同步——
            // 防止「改了 builtin 脚本忘升 manifest version」导致旧脚本
            // 一直跑（2026-11-08 推荐流事故就是这姿势踩出来的）。
            let on_disk_manifest = std::fs::read_to_string(&manifest_path).ok();
            let newer = match on_disk_manifest
                .as_deref()
                .and_then(|s| serde_json::from_str::<PluginManifest>(s).ok())
            {
                None => true,
                Some(on_disk) => {
                    let builtin = serde_json::from_str::<PluginManifest>(b.manifest)
                        .map(|m| m.version)
                        .unwrap_or_default();
                    version_gt(&builtin, &on_disk.version)
                }
            };

            if newer || on_disk_manifest.as_deref() != Some(b.manifest) {
                std::fs::write(&manifest_path, b.manifest)?;
            }
            if newer || std::fs::read_to_string(&script_path).ok().as_deref() != Some(b.script) {
                std::fs::write(&script_path, b.script)?;
            }
        }
        Ok(())
    }

    pub fn reload(&self, roots: &[PathBuf]) {
        let mut map = HashMap::new();
        for root in roots {
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            for entry in entries.flatten() {
                let dir = entry.path();
                if !dir.is_dir() {
                    continue;
                }
                match load_one(&dir, false) {
                    Ok(p) => {
                        map.insert(p.manifest.id.clone(), p);
                    }
                    Err(e) => {
                        tracing::warn!("skip plugin at {}: {e}", dir.display());
                    }
                }
            }
        }
        // 内置兜底：用户目录里没有时，用编译进去的版本
        for b in BUILTINS {
            if map.contains_key(b.id) {
                continue;
            }
            if let Ok(m) = serde_json::from_str::<PluginManifest>(b.manifest) {
                map.insert(
                    b.id.to_string(),
                    LoadedPlugin {
                        manifest: m,
                        script: b.script.to_string(),
                        builtin: true,
                    },
                );
            }
        }
        *self.plugins.write() = map;
    }

    pub fn list(&self) -> Vec<PluginInfo> {
        let plugins = self.plugins.read();
        let mut out: Vec<PluginInfo> = plugins
            .values()
            .map(|p| {
                let (enabled, config) = self
                    .db
                    .plugin_row(&p.manifest.id)
                    .ok()
                    .flatten()
                    .map(|(e, json)| {
                        let cfg: HashMap<String, String> =
                            serde_json::from_str(&json).unwrap_or_default();
                        (e, cfg)
                    })
                    .unwrap_or((true, HashMap::new()));
                PluginInfo {
                    manifest: p.manifest.clone(),
                    enabled,
                    installed: true,
                    builtin: p.builtin,
                    config,
                    error: None,
                }
            })
            .collect();
        out.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
        out
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> AppResult<()> {
        if !self.plugins.read().contains_key(id) {
            return Err(AppError::NotFound(format!("plugin {id}")));
        }
        self.db.set_plugin_enabled(id, enabled)
    }

    pub fn set_config(&self, id: &str, config: HashMap<String, String>) -> AppResult<()> {
        if !self.plugins.read().contains_key(id) {
            return Err(AppError::NotFound(format!("plugin {id}")));
        }
        let json = serde_json::to_string(&config)
            .map_err(|e| AppError::plugin(format!("config: {e}")))?;
        self.db.set_plugin_config(id, &json)
    }

    pub fn config_of(&self, id: &str) -> HashMap<String, String> {
        self.db
            .plugin_row(id)
            .ok()
            .flatten()
            .and_then(|(_, json)| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    /// 安装：把一段脚本写进插件目录（前端「导入插件」用）。
    pub fn install(&self, root: &Path, manifest_json: &str, script: &str) -> AppResult<String> {
        let manifest: PluginManifest = serde_json::from_str(manifest_json)
            .map_err(|e| AppError::plugin(format!("manifest: {e}")))?;
        validate(&manifest).map_err(AppError::plugin)?;
        let dir = root.join(&manifest.id);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("manifest.json"), manifest_json)?;
        std::fs::write(dir.join("index.js"), script)?;
        Ok(manifest.id)
    }

    /// 异步调用插件函数（在阻塞线程池里跑 boa）。
    pub async fn call(
        &self,
        plugin_id: &str,
        func: &str,
        args: Vec<serde_json::Value>,
    ) -> AppResult<serde_json::Value> {
        let (script, enabled) = {
            let plugins = self.plugins.read();
            let p = plugins
                .get(plugin_id)
                .ok_or_else(|| AppError::NotFound(format!("plugin {plugin_id}")))?;
            (p.script.clone(), true)
        };
        let _ = enabled;
        if !self.is_enabled(plugin_id) {
            return Err(AppError::Plugin(format!("plugin {plugin_id} is disabled")));
        }
        let config = self.config_of(plugin_id);
        let func = func.to_string();
        let id = plugin_id.to_string();
        let fail_id = id.clone();

        tauri::async_runtime::spawn_blocking(move || {
            js_runtime::call_function(&script, &func, &args, config)
                // 用 detail() 而不是 Display：否则会拼成 `plugin: emby: plugin: ...`
                .map_err(|e| AppError::Plugin(format!("{id}: {}", e.detail())))
        })
        .await
        .map_err(|e| {
            // tauri 把 tokio 的 JoinError 包成了 `tauri::Error::JoinError`。
            // 失败只有两种：任务被取消、或脚本 panic。panic 载荷里只有第一行
            // 有用（boa 的平台断言、数组越界之类），后面全是 unwinding 栈噪声。
            let tauri::Error::JoinError(join) = e else {
                return AppError::plugin(format!("{fail_id}: 插件任务被中断"));
            };
            if !join.is_panic() {
                return AppError::plugin(format!("{fail_id}: 插件任务被中断"));
            }
            let payload = join.into_panic();
            let raw = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            let first = raw.lines().next().unwrap_or("").trim();
            AppError::plugin(format!("{fail_id}: 插件执行崩溃: {first}"))
        })?
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        self.db
            .plugin_row(id)
            .ok()
            .flatten()
            .map(|(e, _)| e)
            .unwrap_or(true)
    }

    /// 推荐聚合：登录后是每日推荐（歌单+歌曲）∪ 个性推荐，未登录只有个性推荐。
    pub async fn recommend_feed(&self, plugin_id: &str, limit: u32) -> AppResult<RecommendFeed> {
        let value = self
            .call(
                plugin_id,
                "getRecommendFeed",
                vec![serde_json::Value::from(limit)],
            )
            .await?;
        let raw: super::protocol::RecommendFeed = serde_json::from_value(value)
            .map_err(|e| AppError::plugin(format!("recommend feed shape: {e}")))?;
        Ok(RecommendFeed {
            logged_in: raw.logged_in,
            user_id: raw.user_id,
            nickname: raw.nickname,
            playlists: raw.playlists,
            tracks: raw
                .tracks
                .into_iter()
                .map(|r| remote_to_track(plugin_id, r))
                .collect(),
        })
    }

    /// 账号歌单（`liked` = 服务端固定排第一的「我喜欢的音乐」）。
    pub async fn user_playlists(&self, plugin_id: &str) -> AppResult<UserPlaylists> {
        let value = self.call(plugin_id, "getUserPlaylists", vec![]).await?;
        serde_json::from_value(value).map_err(|e| AppError::plugin(format!("user playlists shape: {e}")))
    }

    /// 云端歌单详情（导入用）。曲目已转成本地 Track，upsert 即可入库。
    pub async fn cloud_playlist(&self, plugin_id: &str, id: &str) -> AppResult<CloudPlaylist> {
        let value = self
            .call(
                plugin_id,
                "getPlaylist",
                vec![serde_json::Value::String(id.into())],
            )
            .await?;
        let raw: super::protocol::RemotePlaylist = serde_json::from_value(value)
            .map_err(|e| AppError::plugin(format!("cloud playlist shape: {e}")))?;
        Ok(CloudPlaylist {
            id: if raw.id.is_empty() { id.to_string() } else { raw.id },
            name: raw.name,
            cover_url: raw.cover_url,
            track_count: raw.track_count,
            tracks: raw
                .tracks
                .into_iter()
                .map(|r| remote_to_track(plugin_id, r))
                .collect(),
        })
    }

    /// 听歌打卡：把一条真实播放同步给音源平台（网易云走 clientlog eapi）。
    /// `listened_ms` 是本次播放累计毫秒；未登录/关闭打卡时插件返回 `ok:false`。
    pub async fn scrobble(
        &self,
        plugin_id: &str,
        track_id: &str,
        listened_ms: i64,
    ) -> AppResult<serde_json::Value> {
        self.call(
            plugin_id,
            "scrobble",
            vec![
                serde_json::Value::String(track_id.into()),
                serde_json::Value::from(listened_ms),
            ],
        )
        .await
    }

    pub async fn search(
        &self,
        plugin_id: &str,
        keyword: &str,
        page: u32,
        limit: u32,
    ) -> AppResult<Vec<Track>> {
        let value = self
            .call(
                plugin_id,
                "search",
                vec![
                    serde_json::Value::String(keyword.into()),
                    serde_json::Value::from(page),
                    serde_json::Value::from(limit),
                ],
            )
            .await?;

        let items: Vec<RemoteTrack> = serde_json::from_value(value)
            .map_err(|e| AppError::plugin(format!("search result shape: {e}")))?;

        Ok(items
            .into_iter()
            .map(|r| remote_to_track(plugin_id, r))
            .collect())
    }

    /// 兼容两种插件返回：
    /// * 旧协议：字符串 URL（第三方 / 自建插件，规格未知）
    /// * 新协议：`{ url, source, bitrate, format, trial, requested, downgraded }`
    ///
    /// 规格字段缺省都是安全的，宿主只负责把它塞进播放快照，
    /// 由前端如实显示「实际拿到的音质」，而不是显示用户选了什么。
    pub async fn stream_url(&self, plugin_id: &str, track: &Track) -> AppResult<StreamUrl> {
        let value = self
            .call(plugin_id, "getStreamUrl", vec![track_to_json(track)])
            .await?;
        match value {
            serde_json::Value::String(url) if !url.is_empty() => {
                Ok(StreamUrl { url, quality: None })
            }
            serde_json::Value::Object(_) => {
                let url = value
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if url.is_empty() {
                    return Err(AppError::plugin("plugin returned empty stream url"));
                }
                let mut quality: StreamQuality = serde_json::from_value(value)
                    .map_err(|e| AppError::plugin(format!("stream quality shape: {e}")))?;
                quality.track_id = track.id.clone();
                Ok(StreamUrl {
                    url,
                    quality: Some(quality),
                })
            }
            _ => Err(AppError::plugin("plugin returned empty stream url")),
        }
    }

    pub async fn lyric(&self, plugin_id: &str, track: &Track) -> AppResult<String> {
        let value = self
            .call(plugin_id, "getLyric", vec![track_to_json(track)])
            .await?;
        Ok(value.as_str().unwrap_or_default().to_string())
    }

    /// 扫码登录：让插件向服务端申请一个二维码。
    /// 返回 `(会话 key, 二维码内容, 会话 Cookie)`；会话 Cookie 由前端在
    /// 轮询时原样带回，保证建码与确认是同一个浏览器会话。
    pub async fn login_qr(&self, plugin_id: &str) -> AppResult<(String, String, String)> {
        let value = self.call(plugin_id, "loginQrCreate", vec![]).await?;
        let field = |name: &str| {
            value
                .get(name)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let key = field("key");
        let url = field("url");
        if key.is_empty() || url.is_empty() {
            return Err(AppError::plugin("插件没有返回二维码内容"));
        }
        Ok((key, url, field("session")))
    }

    /// 轮询扫码状态。`success` 时把插件给的 cookie 并进配置——
    /// 之后的 `getStreamUrl` 就会自动带上账号权限。
    pub async fn login_poll(
        &self,
        plugin_id: &str,
        key: &str,
        session: &str,
    ) -> AppResult<serde_json::Value> {
        let value = self
            .call(
                plugin_id,
                "loginQrPoll",
                vec![serde_json::json!(key), serde_json::json!(session)],
            )
            .await?;
        let state = value
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("waiting");
        if state != "success" {
            return Ok(value);
        }

        let cookie = value
            .get("cookie")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim()
            .to_string();
        if cookie.is_empty() {
            return Err(AppError::plugin("登录成功，但插件没有返回 cookie"));
        }
        let mut config = self.config_of(plugin_id);
        config.insert(LOGIN_COOKIE_KEY.to_string(), cookie);
        self.set_config(plugin_id, config)?;
        Ok(value)
    }

    /// 当前账号信息（昵称 / 头像 / VIP）。未登录时插件返回 `{ loggedIn: false }`。
    pub async fn login_info(&self, plugin_id: &str) -> AppResult<serde_json::Value> {
        self.call(plugin_id, "loginInfo", vec![]).await
    }

    /// 发送短信验证码（验证码登录第一步）。返回 `{ state, message, session }`，
    /// session 是发码时的浏览器会话指纹，登录时必须原样带回（与扫码同构）。
    pub async fn captcha_send(
        &self,
        plugin_id: &str,
        phone: &str,
        ctcode: &str,
        session: &str,
    ) -> AppResult<serde_json::Value> {
        self.call(
            plugin_id,
            "captchaSend",
            vec![
                serde_json::json!(phone),
                serde_json::json!(ctcode),
                serde_json::json!(session),
            ],
        )
        .await
    }

    /// 短信验证码登录。成功时把插件返回的 cookie 并进配置——
    /// 与扫码成功同一条落库路径，之后的取址/打卡自动带上账号权限。
    pub async fn login_cellphone(
        &self,
        plugin_id: &str,
        phone: &str,
        captcha: &str,
        ctcode: &str,
        session: &str,
    ) -> AppResult<serde_json::Value> {
        let value = self
            .call(
                plugin_id,
                "loginCellphone",
                vec![
                    serde_json::json!(phone),
                    serde_json::json!(captcha),
                    serde_json::json!(ctcode),
                    serde_json::json!(session),
                ],
            )
            .await?;
        let state = value
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("failed");
        if state != "success" {
            return Ok(value);
        }

        let cookie = value
            .get("cookie")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim()
            .to_string();
        if cookie.is_empty() {
            return Err(AppError::plugin("登录成功，但插件没有返回 cookie"));
        }
        let mut config = self.config_of(plugin_id);
        config.insert(LOGIN_COOKIE_KEY.to_string(), cookie);
        self.set_config(plugin_id, config)?;
        Ok(value)
    }

    /// 退出登录：只删配置里的 cookie，不动其他设置。
    pub fn logout(&self, plugin_id: &str) -> AppResult<()> {
        let mut config = self.config_of(plugin_id);
        if config.remove(LOGIN_COOKIE_KEY).is_none() {
            return Ok(());
        }
        self.set_config(plugin_id, config)
    }
}

fn load_one(dir: &Path, builtin: bool) -> AppResult<LoadedPlugin> {
    let manifest_path = dir.join("manifest.json");
    let text = std::fs::read_to_string(&manifest_path)?;
    let manifest: PluginManifest = serde_json::from_str(&text)
        .map_err(|e| AppError::plugin(format!("{}: {e}", manifest_path.display())))?;
    validate(&manifest).map_err(AppError::plugin)?;
    let script = std::fs::read_to_string(dir.join(&manifest.main))?;
    Ok(LoadedPlugin {
        manifest,
        script,
        builtin,
    })
}

fn remote_to_track(plugin_id: &str, r: RemoteTrack) -> Track {
    Track {
        id: format!("{plugin_id}:{}", r.id),
        source: TrackSource::Plugin {
            plugin_id: plugin_id.to_string(),
        },
        title: if r.title.is_empty() {
            "未知曲目".into()
        } else {
            r.title
        },
        artist: r.artist,
        album: r.album,
        duration_ms: r.duration_ms,
        cover_path: None,
        cover_url: r.cover_url,
        file_path: None,
        stream_url: None,
        plugin_track_id: Some(r.id),
        format: None,
        bitrate: None,
        sample_rate: None,
        channels: None,
        added_at: chrono::Utc::now().timestamp_millis(),
    }
}

fn track_to_json(t: &Track) -> serde_json::Value {
    serde_json::json!({
        "id": t.plugin_track_id.clone().unwrap_or_else(|| t.id.clone()),
        "title": t.title,
        "artist": t.artist,
        "album": t.album,
        "durationMs": t.duration_ms,
        "coverUrl": t.cover_url,
    })
}

/// `a` 的版本号是否高于 `b`。只按数字段比较，非数字段当 0。
fn version_gt(a: &str, b: &str) -> bool {
    fn parse(s: &str) -> Vec<u64> {
        s.split('.').map(|p| p.trim().parse().unwrap_or(0)).collect()
    }
    let (va, vb) = (parse(a), parse(b));
    for i in 0..va.len().max(vb.len()) {
        let x = va.get(i).copied().unwrap_or(0);
        let y = vb.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::version_gt;

    #[test]
    fn version_compare() {
        assert!(version_gt("2.0.0", "1.0.0"));
        assert!(version_gt("1.10.0", "1.9.0"));
        assert!(version_gt("1.0.1", "1.0.0"));
        assert!(!version_gt("1.0.0", "1.0.0"));
        assert!(!version_gt("1.0", "1.0.0"));
        assert!(!version_gt("1.0.0", "2.0.0"));
        // 磁盘上是坏版本号时按 0 处理 → 内置覆盖
        assert!(version_gt("1.0.0", ""));
    }
}
