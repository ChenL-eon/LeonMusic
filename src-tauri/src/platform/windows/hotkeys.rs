//! 全局快捷键。
//!
//! 媒体键（键盘上的 ⏯ ⏭ ⏮）**不在这里注册**——它们由 SMTC 通过系统
//! 媒体会话事件送来，注册热键反而会和系统抢占导致失效。
//! 这里只处理「应用自定义快捷键」。

use tauri::AppHandle;
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use crate::audio::engine::EngineCommand;
use crate::error::AppResult;

use super::SharedTx;

pub fn register(app: &AppHandle, tx: SharedTx) -> AppResult<()> {
    let bindings: [(Shortcut, &'static str); 3] = [
        (
            Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::Space),
            "toggle",
        ),
        (
            Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::ArrowRight),
            "next",
        ),
        (
            Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::ArrowLeft),
            "prev",
        ),
    ];

    for (shortcut, action) in bindings {
        let tx = tx.clone();
        let result = app.global_shortcut().on_shortcut(shortcut, move |_app, _sc, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            let cmd = match action {
                "toggle" => EngineCommand::Toggle,
                "next" => EngineCommand::Next,
                _ => EngineCommand::Prev,
            };
            if let Some(tx) = tx.lock().as_ref() {
                let _ = tx.send(cmd);
            }
        });
        if let Err(e) = result {
            // 快捷键被别的软件占用不该导致启动失败
            tracing::warn!("register global shortcut failed ({action}): {e}");
        }
    }
    Ok(())
}
