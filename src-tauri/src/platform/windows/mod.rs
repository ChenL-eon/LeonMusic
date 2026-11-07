//! Windows 平台实现。
//!
//! | 能力 | 实现 |
//! |---|---|
//! | 托盘 | tauri tray-icon |
//! | 全局快捷键 | tauri-plugin-global-shortcut |
//! | 系统媒体会话 | WinRT `SystemMediaTransportControls`（经 `ISystemMediaTransportControlsInterop::GetForWindow`）|
//! | 悬浮歌词 | 独立透明置顶窗口 `lyric` |

mod hotkeys;
mod lyrics;
mod smtc;
mod tray;

use std::sync::Arc;

use crossbeam_channel::Sender;
use parking_lot::Mutex;
use tauri::AppHandle;

use crate::audio::engine::EngineCommand;
use crate::error::AppResult;
use crate::models::{PlatformInfo, PlatformKind, PlayerSnapshot};

use super::PlatformHost;

pub struct WindowsPlatform {
    tx: SharedTx,
    smtc: Mutex<Option<smtc::Smtc>>,
    lyric_visible: Mutex<bool>,
    tray: Mutex<Option<tauri::tray::TrayIcon>>,
}

impl Default for WindowsPlatform {
    fn default() -> Self {
        Self {
            tx: Arc::new(Mutex::new(None)),
            smtc: Mutex::new(None),
            lyric_visible: Mutex::new(false),
            tray: Mutex::new(None),
        }
    }
}

impl PlatformHost for WindowsPlatform {
    fn info(&self) -> PlatformInfo {
        PlatformInfo {
            kind: PlatformKind::Windows,
            os_version: os_version(),
            arch: std::env::consts::ARCH.to_string(),
            supports_lyric_window: true,
            supports_tray: true,
            supports_global_shortcut: true,
            supports_media_session: true,
            cache_dir: crate::paths::cache_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }

    fn init(&self, app: &AppHandle) -> AppResult<()> {
        // 托盘必须保活，否则图标会被 drop 掉
        let tray = tray::build(app, self.tx.clone())?;
        *self.tray.lock() = Some(tray);

        hotkeys::register(app, self.tx.clone())?;

        // 悬浮歌词窗口提前建好并隐藏，切歌时只是 show/hide，避免反复创建 WebView
        lyrics::preload(app)?;

        // SMTC 依赖主窗口 HWND，Windows 上窗口一定存在
        if let Some(win) = tauri::Manager::get_webview_window(app, "main") {
            match win.hwnd() {
                Ok(hwnd) => {
                    let raw = hwnd.0 as isize;
                    match smtc::Smtc::new(raw, self.tx.clone()) {
                        Ok(s) => {
                            tracing::info!("system media transport controls ready");
                            *self.smtc.lock() = Some(s);
                        }
                        Err(e) => tracing::warn!("SMTC init failed: {e}"),
                    }
                }
                Err(e) => tracing::warn!("cannot get hwnd: {e}"),
            }
        }
        Ok(())
    }

    fn attach_engine(&self, tx: Sender<EngineCommand>) {
        *self.tx.lock() = Some(tx);
    }

    fn update_now_playing(&self, snapshot: &PlayerSnapshot) {
        if let Some(s) = self.smtc.lock().as_ref() {
            if let Err(e) = s.update(snapshot) {
                tracing::debug!("smtc update: {e}");
            }
        }
        // 主窗口标题跟着曲目走，任务栏缩略图能看出来
        if let Some(t) = &snapshot.track {
            tray::set_tooltip(&t.title, &t.artist);
        }
    }

    fn update_playback_state(&self, playing: bool) {
        if let Some(s) = self.smtc.lock().as_ref() {
            let _ = s.set_playing(playing);
        }
    }

    fn update_position(&self, _position_ms: u64, _playing: bool) {
        // WinRT SMTC 不支持写入进度（时间轴由系统根据状态推算），
        // 这里保留接口，Android 端会用 MediaSession 的 PlaybackState 实现。
    }

    fn show_lyric_window(&self, app: &AppHandle) -> AppResult<bool> {
        lyrics::show(app)?;
        *self.lyric_visible.lock() = true;
        Ok(true)
    }

    fn hide_lyric_window(&self, app: &AppHandle) -> AppResult<bool> {
        lyrics::hide(app)?;
        *self.lyric_visible.lock() = false;
        Ok(false)
    }

    fn toggle_lyric_window(&self, app: &AppHandle) -> AppResult<bool> {
        let visible = *self.lyric_visible.lock();
        if visible {
            self.hide_lyric_window(app)
        } else {
            self.show_lyric_window(app)
        }
    }

    fn lyric_window_visible(&self) -> bool {
        *self.lyric_visible.lock()
    }

    fn request_background(&self, _enabled: bool) -> AppResult<()> {
        // 桌面端没有后台服务概念
        Ok(())
    }

    fn set_dark_appearance(&self, _dark: bool) -> AppResult<()> {
        // 桌面端状态栏属于窗口装饰，跟随系统设置，应用无权干预
        Ok(())
    }
}

fn os_version() -> String {
    // 避免额外依赖 windows-version，直接读环境变量
    std::env::var("OS").unwrap_or_else(|_| "Windows".into())
}

pub(crate) type SharedTx = Arc<Mutex<Option<Sender<EngineCommand>>>>;
