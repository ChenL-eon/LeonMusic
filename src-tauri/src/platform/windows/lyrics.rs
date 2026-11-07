//! 桌面悬浮歌词窗口。
//!
//! 一个独立的透明置顶 WebView，加载 `lyric.html`。
//! 与主界面共用同一套 Rust 状态推送（订阅 `leon://player/state` 与
//! `leon://player/lyrics`），因此不存在「两套歌词逻辑」。

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::error::{AppError, AppResult};

const LABEL: &str = "lyric";

/// 启动时预创建并隐藏：切歌/开关歌词只是 show/hide，
/// 不重复创建 WebView（Windows 上创建 WebView2 约 100ms，会明显卡顿）。
pub fn preload(app: &AppHandle) -> AppResult<()> {
    if app.get_webview_window(LABEL).is_some() {
        return Ok(());
    }

    let win = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("lyric.html".into()))
        .title("LeonMusic Lyrics")
        // 与主窗口保持一致：https scheme 走 WebView2 的内存内 CustomScheme 通道，
        // 不经过 loopback socket。http scheme（默认）会被系统代理劫持——
        // 代理对 tauri.localhost 无法回源时整个窗口直接 ERR_CONNECTION_REFUSED。
        .use_https_scheme(true)
        .inner_size(860.0, 170.0)
        .min_inner_size(320.0, 90.0)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(true)
        .focused(false)
        .visible(false)
        .build()
        .map_err(|e| AppError::invalid(format!("create lyric window: {e}")))?;

    // 初始位置：屏幕下方居中
    if let Ok(Some(monitor)) = win.current_monitor() {
        let size = monitor.size();
        let scale = monitor.scale_factor();
        let w = 860.0 * scale;
        let h = 170.0 * scale;
        let x = (size.width as f64 - w) / 2.0;
        let y = size.height as f64 - h - 120.0 * scale;
        let _ = win.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
    }

    Ok(())
}

pub fn show(app: &AppHandle) -> AppResult<()> {
    preload(app)?;
    if let Some(w) = app.get_webview_window(LABEL) {
        w.show()
            .map_err(|e| AppError::invalid(format!("show lyric: {e}")))?;
        // 置顶跟随设置（工具条可取消置顶；这里不能硬编码 true，
        // 否则每次打开都会把用户关掉的置顶又顶回去）
        let pinned = app
            .try_state::<crate::state::AppState>()
            .map(|st| st.settings.read().dl_always_on_top)
            .unwrap_or(true);
        let _ = w.set_always_on_top(pinned);
    }
    Ok(())
}

pub fn hide(app: &AppHandle) -> AppResult<()> {
    if let Some(w) = app.get_webview_window(LABEL) {
        w.hide()
            .map_err(|e| AppError::invalid(format!("hide lyric: {e}")))?;
    }
    Ok(())
}

pub fn toggle(app: &AppHandle) -> AppResult<()> {
    if let Some(w) = app.get_webview_window(LABEL) {
        if w.is_visible().unwrap_or(false) {
            hide(app)
        } else {
            show(app)
        }
    } else {
        show(app)
    }
}
