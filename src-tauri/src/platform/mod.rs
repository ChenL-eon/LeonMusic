//! 平台差异层。
//!
//! 所有「跟操作系统打交道」的能力都收敛到 [`PlatformHost`]：
//! 托盘、全局快捷键、系统媒体会话、悬浮歌词窗、Android 前台服务。
//!
//! 上层（engine / commands）只调用 trait 方法，不出现任何 `#[cfg(windows)]`
//! 分支；平台选择发生在 [`create`]。

use std::sync::Arc;

use crossbeam_channel::Sender;
use tauri::AppHandle;

use crate::audio::engine::EngineCommand;
use crate::error::AppResult;
use crate::models::{PlatformInfo, PlayerSnapshot};

// 每个平台的实现都**只在对应目标上编译**。
// 这一点很关键：Windows 模块里全是 HWND / SMTC / 托盘 API，
// 一旦在 Android 目标上参与编译，`cargo check --target aarch64-linux-android`
// 会直接炸掉 20 多个 E0599。
#[cfg(target_os = "android")]
pub mod android;
#[cfg(windows)]
pub mod windows;
#[cfg(not(any(windows, target_os = "android")))]
pub mod fallback;

pub trait PlatformHost: Send + Sync {
    fn info(&self) -> PlatformInfo;

    /// 应用启动后初始化平台能力（托盘、快捷键、媒体会话）。
    fn init(&self, app: &AppHandle) -> AppResult<()>;

    /// 引擎创建完成后回注命令通道，系统媒体按键通过它驱动播放。
    fn attach_engine(&self, tx: Sender<EngineCommand>);

    /// 曲目 / 播放状态变化：更新系统媒体面板（SMTC / MediaSession）。
    fn update_now_playing(&self, snapshot: &PlayerSnapshot);

    fn update_playback_state(&self, playing: bool);

    /// 进度同步：频率低（约 1s），只用于系统控件的进度条。
    fn update_position(&self, position_ms: u64, playing: bool);

    // ---- 桌面悬浮歌词（Android 不支持，调用返回 Unsupported）----
    // 返回操作后窗口的可见状态（显示 true / 隐藏 false），前端开关按钮直接消费。
    fn show_lyric_window(&self, app: &AppHandle) -> AppResult<bool>;
    fn hide_lyric_window(&self, app: &AppHandle) -> AppResult<bool>;
    fn toggle_lyric_window(&self, app: &AppHandle) -> AppResult<bool>;

    /// 歌词窗口当前是否可见（主窗启动时同步开关按钮的初始状态）。
    fn lyric_window_visible(&self) -> bool {
        false
    }

    /// Android：拉起/停止前台服务；桌面：空实现。
    fn request_background(&self, enabled: bool) -> AppResult<()>;

    /// 系统栏明暗跟随应用主题。
    ///
    /// Activity 开了 edge-to-edge，状态栏和手势条直接压在 WebView 上，
    /// 图标颜色只能由系统决定 —— 应用切深色后如果图标还是深色，就完全看不见了。
    /// Android 侧转成 `LeonBridge.dispatch("theme")` 交给系统栏控制器；
    /// 桌面端状态栏由窗口装饰处理，这里是空实现。
    fn set_dark_appearance(&self, dark: bool) -> AppResult<()>;
}

pub fn create() -> Arc<dyn PlatformHost> {
    #[cfg(windows)]
    {
        Arc::new(windows::WindowsPlatform::default())
    }
    #[cfg(target_os = "android")]
    {
        Arc::new(android::AndroidPlatform::default())
    }
    #[cfg(not(any(windows, target_os = "android")))]
    {
        Arc::new(fallback::NoopPlatform)
    }
}
