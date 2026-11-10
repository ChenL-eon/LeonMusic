//! 应用全局状态。
//!
//! 注意：**播放状态不在 AppState 里**——它由 [`crate::audio::engine::SharedState`]
//! 单独持有，AppState 只持有引擎句柄。这样即便有人误用 AppState 也改不到播放状态。

use std::sync::Arc;
use std::sync::OnceLock;

use parking_lot::RwLock;

use crate::audio::engine::AudioEngine;
use crate::db::{Db, Settings};
use crate::library::Library;
use crate::models::{StreamQuality, Track};
use crate::net::{BlockingHttp, HttpClient, MediaCache};
use crate::platform::PlatformHost;
use crate::plugin::PluginManager;

pub struct AppState {
    pub engine: Arc<AudioEngine>,
    pub platform: Arc<dyn PlatformHost>,
    pub db: Arc<Db>,
    pub library: Arc<Library>,
    pub plugins: Arc<PluginManager>,
    pub cache: Arc<MediaCache>,
    /// 流式版后暂无 Rust 侧调用方（下载走了 `BlockingHttp` 流式会话），
    /// 保留给未来的异步下载 / 导出场景。
    #[allow(dead_code)]
    pub http: HttpClient,
    pub settings: RwLock<Settings>,
}

/// 供 Android JNI 层与平台回调访问的全局引擎句柄。
pub static ENGINE: OnceLock<Arc<AudioEngine>> = OnceLock::new();

/// 流式播放依赖三件套（`Session::open` 遇到 `stream_url` 时取用）。
///
/// 引擎线程在 AppState manage 之前就启动了，不能直接 `app.state::<AppState>()`
/// 的构造顺序依赖；这里用 OnceLock 在 setup 完成时注入，engine 侧窄依赖这
/// 三个能力，不背整个 AppState。
pub struct StreamDeps {
    pub cache: Arc<MediaCache>,
    /// 进程级共享客户端（与插件解析同一份连接池）。
    pub blocking: &'static BlockingHttp,
    pub db: Arc<Db>,
}

pub static STREAM_DEPS: OnceLock<StreamDeps> = OnceLock::new();

pub fn set_stream_deps(deps: StreamDeps) {
    let _ = STREAM_DEPS.set(deps);
}

pub fn stream_deps() -> Option<&'static StreamDeps> {
    STREAM_DEPS.get()
}

pub fn set_engine(engine: Arc<AudioEngine>) {
    let _ = ENGINE.set(engine);
}

/// 插件当前的「在线音质」档位：`quality_hint`（智能降级的临时档位）优先，
/// 其次插件配置，最后回落到 manifest 默认值。
///
/// 只有声明了 `level` 档位的插件才返回 `Some` —— Emby 这类自建服务的音质
/// 由服务端转码决定，谈「档位」没有意义。
fn plugin_level(st: &AppState, track: &Track) -> Option<String> {
    if let Some(hint) = track.quality_hint.as_deref() {
        return Some(hint.to_string());
    }
    let plugin_id = track.source.plugin_id()?;
    let info = st
        .plugins
        .list()
        .into_iter()
        .find(|p| p.manifest.id == plugin_id)?;
    let field = info.manifest.config.iter().find(|f| f.key == "level")?;
    // 用户没动过音质设置时配置是空对象，实际生效的是 manifest 的默认值
    Some(
        info.config
            .get("level")
            .or(field.default.as_ref())
            .cloned()
            .unwrap_or_else(|| "exhigh".to_string()),
    )
}

/// 「智能音质」：当前曲目还能再降一档吗？返回下一档档位。
///
/// 三个前提缺一不可：设置里开着、曲目来自插件音源、插件声明了 `level`
/// 档位配置。降级只对「同一个 URL 换更低码率能救回来」的场景有意义，
/// 本地文件打不开或插件根本不支持选档时不该参与重试。
pub fn smart_next_level(app: &tauri::AppHandle, track: &Track) -> Option<&'static str> {
    use tauri::Manager;

    let st = app.try_state::<AppState>()?;
    if !st.settings.read().smart_quality {
        return None;
    }
    let current = plugin_level(&st, track)?;
    crate::models::lower_quality(&current)
}

/// 这几档要求无损容器（见 `QUALITY_LADDER`）。
fn level_wants_lossless(level: &str) -> bool {
    matches!(level, "lossless" | "hires" | "jymaster")
}

/// 落盘文件是不是无损容器。
///
/// `m4a` 按有损算：它既可能装 ALAC 也可能装 AAC，按扩展名分不出来；宁可
/// 重下一遍真无损，也别把 AAC 当成「已经是无损」。缓存只会产出 flac / mp3 /
/// m4a / ogg / opus 这几种，这里的白名单覆盖到就够。
fn is_lossless_file(path: &str) -> bool {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "flac" | "wav" | "ape" | "alac" | "aif" | "aiff" | "dsf" | "dff"
    )
}

/// 在线曲目挂着的缓存文件是否**低于**用户当前选的档位。
///
/// 典型场景：以前用 320k 播过这首歌，下载线程把 mp3 的路径写回
/// `tracks.file_path`；之后用户把在线音质改成无损，点播却仍然直接命中那份
/// MP3 —— 解析链根本没有机会跑，界面永远显示「无损未生效」。这里把这种
/// 「音质升级后失效的缓存绑定」清掉（内存 + 数据库），让调用方重新解析。
///
/// 只清绑定、不删文件：旧文件由 LRU 淘汰，删了可能踩到正在读它的解码线程。
///
/// 返回被丢掉的路径（`Some` 表示调用方可以放弃 `file_path`、改走在线解析）。
/// 调用方最好把它留作兜底：解析失败时还能退回这份有损缓存，离线也不至于
/// 从「能听」变成「报错」。
pub fn drop_stale_lossy_cache(app: &tauri::AppHandle, track: &mut Track) -> Option<String> {
    use tauri::Manager;

    let path = track.file_path.clone()?;
    // 本地曲库没有「在线档位」，也重解析不出更好的版本
    if track.source.plugin_id().is_none() {
        return None;
    }
    let st = app.try_state::<AppState>()?;
    let level = plugin_level(&st, track)?;
    if !level_wants_lossless(&level) || is_lossless_file(&path) {
        return None;
    }

    tracing::info!(
        "drop lossy cache for {} (level={level}): {path}",
        track.id
    );
    track.file_path = None;
    track.stream_url = None;
    if let Err(e) = st.db.clear_track_file_path(&track.id) {
        tracing::warn!("clear cached file_path failed: {e}");
    }
    Some(path)
}

/// 下一首预解析槽：当前曲目开始播放后，后台线程解析队列下一首的直链放进
/// 这里；真正切歌时 `ensure_playable` 命中直接取用，解析等待 ≈ 0。
/// 只存最近一个「下一首」——顺序播放是主场景，随机模式下命中率低属于正常退化。
#[derive(Clone)]
pub struct PreResolved {
    pub track: Track,
    pub quality: Option<StreamQuality>,
}

pub static PRE_RESOLVED: OnceLock<parking_lot::Mutex<Option<PreResolved>>> = OnceLock::new();

pub fn pre_resolved() -> &'static parking_lot::Mutex<Option<PreResolved>> {
    PRE_RESOLVED.get_or_init(|| parking_lot::Mutex::new(None))
}

/// 只有 Android 的 `nativeSnapshot` 会用到；桌面端直接走 Tauri command。
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn engine() -> Option<&'static Arc<AudioEngine>> {
    ENGINE.get()
}

/// Android 重建 WebView 后拉取完整状态。
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn snapshot_json() -> String {
    match engine() {
        Some(e) => serde_json::to_string(&e.shared().snapshot()).unwrap_or_else(|_| "null".into()),
        None => "null".into(),
    }
}
