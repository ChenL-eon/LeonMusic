//! 应用级命令：外部链接、版本相关。

use crate::error::{AppError, AppResult};

/// 用系统默认浏览器打开外部链接（「更新与关于」的「前往下载」等入口）。
/// 只放行 http(s)，防止前端借道调起任意协议。
#[tauri::command]
pub fn open_external(url: String) -> AppResult<()> {
    let url = url.trim().to_string();
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(AppError::invalid("not an http(s) url"));
    }
    open::that(&url).map_err(|e| AppError::invalid(format!("open url: {e}")))
}

/// 当前应用版本。「更新与关于」用它和 GitHub latest release 比较。
/// 版本号以 tauri.conf.json 的 package.version 为唯一来源（发布只改那里），
/// 不用 CARGO_PKG_VERSION——Cargo.toml 的 version 和它不同步（0.1.0 vs 0.2.0）。
#[tauri::command]
pub fn get_app_version(app: tauri::AppHandle) -> AppResult<String> {
    Ok(app.config().version.clone().unwrap_or_default())
}
