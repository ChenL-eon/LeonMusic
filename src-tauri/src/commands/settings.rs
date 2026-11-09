//! 设置、平台信息、缓存管理。

use std::fs;

use base64::Engine;
use tauri::{AppHandle, Emitter, Manager};

use crate::audio::engine::EngineCommand;
use crate::db::Settings;
use crate::error::{AppError, AppResult};
use crate::events::EVT_SETTINGS_CHANGED;
use crate::models::{MediaStatus, PlatformInfo};
use crate::state::AppState;

#[tauri::command]
pub fn settings_get(app: AppHandle) -> Settings {
    app.state::<AppState>().settings.read().clone()
}

#[tauri::command]
pub fn settings_update(app: AppHandle, settings: Settings) -> AppResult<Settings> {
    let st = app.state::<AppState>();
    {
        let mut guard = st.settings.write();
        *guard = settings.clone();
    }
    st.db.save_settings(&settings)?;

    // 即时生效的部分。
    // repeat/shuffle 不在此列：播放模式以 playback-state.json 为准
    // （随队列/进度一起恢复），用户改设置不该顺手覆盖它。
    st.engine.send(EngineCommand::SetVolume(settings.volume));
    st.engine.send(EngineCommand::SetSpeed(settings.speed));
    st.engine.send(EngineCommand::SetPitch(settings.pitch));
    st.engine.send(EngineCommand::SetGain(settings.gain_db));

    // 广播给所有窗口（含桌面歌词窗）：两窗各有一份 Pinia settings 实例，
    // 靠这条事件保持同步（歌词窗工具条改字号/锁定，设置页立即跟着变）。
    let _ = app.emit(EVT_SETTINGS_CHANGED, settings.clone());

    Ok(settings)
}

#[tauri::command]
pub fn app_platform_info(app: AppHandle) -> PlatformInfo {
    app.state::<AppState>().platform.info()
}

#[tauri::command]
pub fn cache_size(app: AppHandle) -> AppResult<i64> {
    app.state::<AppState>().db.cache_total_size()
}

#[tauri::command]
pub fn cache_clear(app: AppHandle) -> AppResult<()> {
    let st = app.state::<AppState>();
    st.cache.clear_all()?;
    // 清空索引
    for (key, path, _) in st.db.cache_entries_oldest(10_000)? {
        let _ = std::fs::remove_file(path);
        let _ = st.db.drop_cache(&key);
    }
    Ok(())
}

#[tauri::command]
pub fn cache_enforce_limit(app: AppHandle) -> AppResult<()> {
    let st = app.state::<AppState>();
    let limit = st.settings.read().cache_limit_mb;
    st.cache.enforce_limit(limit)
}

/// Android 把应用加入电池优化白名单（桌面返回 Unsupported）。
#[tauri::command]
pub fn request_background_mode(app: AppHandle, enabled: bool) -> AppResult<()> {
    app.state::<AppState>().platform.request_background(enabled)
}

/// 系统栏图标明暗跟随应用主题（桌面空实现）。
///
/// 必须在 `applyTheme` 里跟着调：Android 开了 edge-to-edge，
/// 深色主题下不通知系统就会得到「深色背景 + 深色状态栏图标」。
#[tauri::command]
pub fn app_set_dark_appearance(app: AppHandle, dark: bool) -> AppResult<()> {
    app.state::<AppState>().platform.set_dark_appearance(dark)
}

#[tauri::command]
pub fn app_data_dir(app: AppHandle) -> String {
    let _ = app;
    crate::paths::data_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ---- Android 通知栏 / 锁屏媒体控制（桌面端走 PlatformHost 的默认空实现）----

/// 设置页展示的实时状态：通知权限、MediaSession、前台服务、两个开关。
#[tauri::command]
pub fn app_media_status(app: AppHandle) -> MediaStatus {
    app.state::<AppState>().platform.media_status()
}

/// 用户在设置页切换「通知栏播放控制」/「锁屏播放状态」。
#[tauri::command]
pub fn app_set_media_options(
    app: AppHandle,
    notification: bool,
    lockscreen: bool,
) -> AppResult<()> {
    app.state::<AppState>()
        .platform
        .set_media_options(notification, lockscreen);
    Ok(())
}

/// 用户拒绝过 POST_NOTIFICATIONS 后，从这里跳系统通知设置自行开启。
#[tauri::command]
pub fn app_open_notification_settings(app: AppHandle) -> AppResult<()> {
    app.state::<AppState>().platform.open_notification_settings();
    Ok(())
}

// ---- 自定义软件背景 --------------------------------------------------------
//
// 图片由前端 <input type="file"> 读成 base64 传过来，而不是走系统文件对话框：
// Android 返回的是 content:// URI，Rust 没法直接 fs::copy；File API 在
// WebView 里两端行为一致。落盘到 appData 是必要的，用户随时可能删掉原图。

const BG_FILE_PREFIX: &str = "background.";
const BG_MAX_BYTES: usize = 20 * 1024 * 1024;

fn bg_dir() -> AppResult<std::path::PathBuf> {
    let dir = crate::paths::data_dir()?.join("backgrounds");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// 按文件头认格式，扩展名不信任前端传值；浏览器能显示的常见图片都收。
fn sniff_image_ext(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.starts_with(b"BM") {
        Some("bmp")
    } else {
        None
    }
}

/// 删掉旧的 background.*（换图时扩展名可能不同，只覆盖同名会留垃圾）。
fn remove_saved_background(dir: &std::path::Path) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(BG_FILE_PREFIX) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// 改设置 → 落库 → 广播，和 `settings_update` 保持同一条状态同步链，
/// 这样主窗口与歌词窗的 Pinia store 都能实时跟上。
fn commit_settings(app: &AppHandle, patch: impl FnOnce(&mut Settings)) -> AppResult<Settings> {
    let st = app.state::<AppState>();
    let updated = {
        let mut guard = st.settings.write();
        patch(&mut guard);
        guard.clone()
    };
    st.db.save_settings(&updated)?;
    let _ = app.emit(EVT_SETTINGS_CHANGED, &updated);
    Ok(updated)
}

/// 返回已保存图片的绝对路径（前端用 `convertFileSrc` 转成 asset URL）。
/// 文件丢失或没选过图时返回空串，前端据此隐藏背景层。
#[tauri::command]
pub fn app_background_path(app: AppHandle) -> String {
    let name = app.state::<AppState>().settings.read().bg_image.clone();
    if name.is_empty() {
        return String::new();
    }
    let Ok(base) = crate::paths::data_dir() else {
        return String::new();
    };
    let path = base.join("backgrounds").join(name);
    if path.is_file() {
        path.to_string_lossy().into_owned()
    } else {
        String::new()
    }
}

/// 保存一张背景图（data 是纯 base64，不含 `data:image/...;base64,` 前缀）。
#[tauri::command]
pub fn app_set_background_image(app: AppHandle, data: String) -> AppResult<Settings> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .map_err(|e| AppError::invalid(format!("图片数据无效：{e}")))?;
    if bytes.is_empty() || bytes.len() > BG_MAX_BYTES {
        return Err(AppError::invalid("请选择有效的图片，且大小不超过 20 MB"));
    }
    let ext = sniff_image_ext(&bytes)
        .ok_or_else(|| AppError::invalid("仅支持 PNG / JPG / WebP / GIF / BMP 图片"))?;

    let dir = bg_dir()?;
    // 文件名带时间戳：同名覆盖时 asset URL 不变，浏览器会一直用缓存里的旧图。
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let name = format!("{BG_FILE_PREFIX}{stamp}.{ext}");
    // 先写临时文件再改名，避免写一半崩溃后留下坏图覆盖掉原来的好图。
    // 临时名不能以 background. 开头，否则会被下面的清理一起删掉。
    let tmp = dir.join("upload.tmp");
    fs::write(&tmp, &bytes)?;
    remove_saved_background(&dir);
    fs::rename(&tmp, dir.join(&name))?;

    commit_settings(&app, |s| s.bg_image = name)
}

/// 清除背景图：删文件并清空字段，开关本身保留（用户可能马上换一张）。
#[tauri::command]
pub fn app_clear_background_image(app: AppHandle) -> AppResult<Settings> {
    if let Ok(dir) = bg_dir() {
        remove_saved_background(&dir);
    }
    commit_settings(&app, |s| s.bg_image.clear())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_common_image_formats() {
        assert_eq!(sniff_image_ext(b"\x89PNG\r\n\x1a\nrest"), Some("png"));
        assert_eq!(sniff_image_ext(&[0xff, 0xd8, 0xff, 0xe0]), Some("jpg"));
        assert_eq!(sniff_image_ext(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(sniff_image_ext(b"GIF89a...."), Some("gif"));
        assert_eq!(sniff_image_ext(b"BM...."), Some("bmp"));
        assert_eq!(sniff_image_ext(b"not an image"), None);
        assert_eq!(sniff_image_ext(&[]), None, "短于文件头不能 panic");
        assert_eq!(sniff_image_ext(b"RIFF"), None, "截断的 RIFF 不能越界");
    }
}
