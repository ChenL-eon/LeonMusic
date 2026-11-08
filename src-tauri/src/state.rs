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
    let plugin_id = track.source.plugin_id()?;
    // 只有声明了 `level` 档位的插件才谈得上「降一档」；Emby 这类自建服务
    // 的音质由服务端转码决定，重试同一个地址只会白等。
    let info = st
        .plugins
        .list()
        .into_iter()
        .find(|p| p.manifest.id == plugin_id)?;
    let field = info.manifest.config.iter().find(|f| f.key == "level")?;
    // 用户没动过音质设置时配置是空对象，实际生效的是 manifest 的默认值
    let base = info
        .config
        .get("level")
        .or(field.default.as_ref())
        .cloned()
        .unwrap_or_else(|| "exhigh".to_string());
    let current = track.quality_hint.as_deref().unwrap_or(base.as_str());
    crate::models::lower_quality(current)
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
