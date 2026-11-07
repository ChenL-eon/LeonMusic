//! Android 平台实现。
//!
//! # 为什么必须有一个 Kotlin 前台服务
//!
//! Tauri 在 Android 上的 Activity/WebView 会被系统回收（切后台、内存压力、
//! 分屏切换）。如果播放依赖 WebView 存活，一旦 Activity 销毁音频就断了。
//!
//! 因此：
//! * 音频引擎在 **Rust 进程**里跑（`leon-audio-engine` 线程），与 WebView 无关
//! * `PlaybackService` 是**前台服务**（`foregroundServiceType="mediaPlayback"`），
//!   持有 `MediaSession` + 通知，让进程保持存活并拿到媒体键/锁屏控制
//! * WebView 只是「遥控器」：它可以被销毁重建，重建后从 Rust 拉一次快照即可恢复 UI
//!
//! # 音频焦点
//!
//! `AudioFocusHelper` 处理：
//! * 来电 / 其他应用播放 → `AUDIOFOCUS_LOSS_TRANSIENT` → 暂停
//! * 导航播报 → `AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK` → 降低音量
//! * 永久失去焦点 → 暂停并放弃
//!
//! # 电池优化
//!
//! 首次进入时引导用户把应用加入「不受限制」白名单（`REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`
//! 只在用户主动点击时才能弹，不能静默申请）。

mod bridge;

use crossbeam_channel::Sender;
use parking_lot::Mutex;
use serde::Serialize;
use tauri::AppHandle;

use crate::audio::engine::EngineCommand;
use crate::error::{AppError, AppResult};
use crate::models::{PlatformInfo, PlatformKind, PlaybackStatus, PlayerSnapshot};

use super::PlatformHost;

/// Android 平台实现。
///
/// 状态几乎都在 Kotlin 侧（MediaSession / 通知），这里只做**翻译**：
/// Rust 的快照 → JSON → `LeonBridge.dispatch`。
/// 唯一的本地状态是「上次推送的封面」，用来避免重复下载同一张图。
pub struct AndroidPlatform {
    last_cover: Mutex<Option<String>>,
}

impl Default for AndroidPlatform {
    fn default() -> Self {
        Self {
            last_cover: Mutex::new(None),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NowPlayingPayload<'a> {
    title: &'a str,
    artist: &'a str,
    album: &'a str,
    duration_ms: u64,
    position_ms: u64,
    playing: bool,
    cover_url: Option<&'a str>,
    track_id: &'a str,
}

impl PlatformHost for AndroidPlatform {
    fn info(&self) -> PlatformInfo {
        PlatformInfo {
            kind: PlatformKind::Android,
            os_version: android_version(),
            arch: std::env::consts::ARCH.to_string(),
            supports_lyric_window: false,
            supports_tray: false,
            supports_global_shortcut: false,
            supports_media_session: true,
            cache_dir: crate::paths::cache_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }

    fn init(&self, _app: &AppHandle) -> AppResult<()> {
        bridge::call("init", "{}");
        Ok(())
    }

    fn attach_engine(&self, tx: Sender<EngineCommand>) {
        // 系统媒体按键不经过 trait，直接从 JNI 回调进 channel
        bridge::install_command_sink(tx);
    }

    fn update_now_playing(&self, snapshot: &PlayerSnapshot) {
        let Some(track) = &snapshot.track else {
            bridge::call("clear", "{}");
            return;
        };
        let cover = track
            .cover_url
            .as_deref()
            .or(track.cover_path.as_deref());
        let payload = NowPlayingPayload {
            title: &track.title,
            artist: &track.artist,
            album: &track.album,
            duration_ms: snapshot.duration_ms,
            position_ms: snapshot.position_ms,
            playing: snapshot.status == PlaybackStatus::Playing,
            cover_url: cover,
            track_id: &track.id,
        };
        if let Ok(json) = serde_json::to_string(&payload) {
            *self.last_cover.lock() = cover.map(|s| s.to_string());
            bridge::call("metadata", &json);
        }
    }

    fn update_playback_state(&self, playing: bool) {
        let json = format!("{{\"playing\":{playing}}}");
        bridge::call("state", &json);
    }

    fn update_position(&self, position_ms: u64, playing: bool) {
        let json = format!("{{\"positionMs\":{position_ms},\"playing\":{playing}}}");
        bridge::call("position", &json);
    }

    fn show_lyric_window(&self, _app: &AppHandle) -> AppResult<bool> {
        Err(AppError::Unsupported("desktop lyric window".into()))
    }

    fn hide_lyric_window(&self, _app: &AppHandle) -> AppResult<bool> {
        Ok(false)
    }

    fn toggle_lyric_window(&self, _app: &AppHandle) -> AppResult<bool> {
        Err(AppError::Unsupported("desktop lyric window".into()))
    }

    fn request_background(&self, enabled: bool) -> AppResult<()> {
        let json = format!("{{\"enabled\":{enabled}}}");
        bridge::call("foreground", &json);
        Ok(())
    }

    fn set_dark_appearance(&self, dark: bool) -> AppResult<()> {
        // 桥断了只是图标颜色不对，不该让主题切换整体失败
        let json = format!("{{\"dark\":{dark}}}");
        bridge::call("theme", &json);
        Ok(())
    }
}

fn android_version() -> String {
    // 由 Kotlin 侧在 init 时补全，这里先用 SDK 常量
    format!("Android SDK {}", bridge::sdk_int().unwrap_or(0))
}

