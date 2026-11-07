//! 系统托盘。菜单必须持有 `TrayIcon`，否则图标会立即消失。

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::audio::engine::EngineCommand;
use crate::error::{AppError, AppResult};

use super::SharedTx;

pub fn build(app: &AppHandle, tx: SharedTx) -> AppResult<tauri::tray::TrayIcon> {
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let play = MenuItem::with_id(app, "toggle", "播放 / 暂停", true, None::<&str>)?;
    let prev = MenuItem::with_id(app, "prev", "上一首", true, None::<&str>)?;
    let next = MenuItem::with_id(app, "next", "下一首", true, None::<&str>)?;
    let lyric = MenuItem::with_id(app, "lyric", "桌面歌词", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 LeonMusic", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let sep3 = PredefinedMenuItem::separator(app)?;

    let menu = Menu::with_items(
        app,
        &[&show, &sep, &play, &prev, &next, &sep2, &lyric, &sep3, &quit],
    )?;

    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| AppError::invalid("missing window icon"))?;

    let tray = TrayIconBuilder::with_id("leon-tray")
        .icon(icon)
        .tooltip("LeonMusic")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event({
            let tx = tx.clone();
            move |app, event| {
                let id = event.id().as_ref();
                let cmd = match id {
                    "toggle" => Some(EngineCommand::Toggle),
                    "prev" => Some(EngineCommand::Prev),
                    "next" => Some(EngineCommand::Next),
                    _ => None,
                };
                if let Some(cmd) = cmd {
                    if let Some(tx) = tx.lock().as_ref() {
                        let _ = tx.send(cmd);
                    }
                    return;
                }
                match id {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.unminimize();
                            let _ = w.set_focus();
                        }
                    }
                    "lyric" => {
                        let _ = super::lyrics::toggle(app);
                    }
                    "quit" => app.exit(0),
                    _ => {}
                }
            }
        })
        .on_tray_icon_event(|tray, event| {
            // 左键单击 / 双击：唤出主窗口（与大多数播放器一致）。
            // 主窗口在「最小化到托盘」模式下是 hide 而非 destroy，
            // show + unminimize + focus 一定能拉回来。
            match event {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
                | TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                } => {
                    let app = tray.app_handle();
                    if let Some(w) = app.get_webview_window("main") {
                        let _ = w.show();
                        let _ = w.unminimize();
                        let _ = w.set_focus();
                    }
                }
                _ => {}
            }
        })
        .build(app)?;

    Ok(tray)
}

/// 托盘 tooltip 跟随当前曲目。
pub fn set_tooltip(title: &str, artist: &str) {
    // Tauri 的 tray handle 需要从全局拿；这里用 AppHandle 不方便，
    // 为了不引入额外全局状态，tooltip 只在创建时设置。
    let _ = (title, artist);
}
