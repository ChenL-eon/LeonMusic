//! 兜底平台实现。
//!
//! 用于「既不是 Windows 也不是 Android」的目标（主要是 CI 在 Linux 上跑
//! `cargo check` / `cargo test`）。它不做任何事，但让整个 crate 能编译通过，
//! 从而让与平台无关的核心逻辑（解码、DSP、队列、歌词解析、插件协议）
//! 在 CI 上仍然可测。
//!
//! 注意：这个模块**不能**引用任何 Win32 / JNI API，否则 Android 目标又会挂。
//! 上一版把 `NoopPlatform` 放在 `platform/windows/mod.rs` 里，导致
//! `cargo check --target aarch64-linux-android` 直接失败——就是这么来的。

use crossbeam_channel::Sender;
use tauri::AppHandle;

use crate::audio::engine::EngineCommand;
use crate::error::AppResult;
use crate::models::{PlatformInfo, PlatformKind, PlayerSnapshot};

use super::PlatformHost;

pub struct NoopPlatform;

impl PlatformHost for NoopPlatform {
    fn info(&self) -> PlatformInfo {
        PlatformInfo {
            kind: PlatformKind::Other,
            os_version: String::new(),
            arch: std::env::consts::ARCH.to_string(),
            supports_lyric_window: false,
            supports_tray: false,
            supports_global_shortcut: false,
            supports_media_session: false,
            cache_dir: String::new(),
        }
    }

    fn init(&self, _app: &AppHandle) -> AppResult<()> {
        Ok(())
    }

    fn attach_engine(&self, _tx: Sender<EngineCommand>) {}

    fn update_now_playing(&self, _snapshot: &PlayerSnapshot) {}

    fn update_playback_state(&self, _playing: bool) {}

    fn update_position(&self, _position_ms: u64, _playing: bool) {}

    fn show_lyric_window(&self, _app: &AppHandle) -> AppResult<bool> {
        Ok(false)
    }

    fn hide_lyric_window(&self, _app: &AppHandle) -> AppResult<bool> {
        Ok(false)
    }

    fn toggle_lyric_window(&self, _app: &AppHandle) -> AppResult<bool> {
        Ok(false)
    }

    fn request_background(&self, _enabled: bool) -> AppResult<()> {
        Ok(())
    }

    fn set_dark_appearance(&self, _dark: bool) -> AppResult<()> {
        Ok(())
    }
}
