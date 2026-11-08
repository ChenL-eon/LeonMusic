//! 设置、平台信息、缓存管理。

use tauri::{AppHandle, Emitter, Manager};

use crate::audio::engine::EngineCommand;
use crate::db::Settings;
use crate::error::AppResult;
use crate::events::EVT_SETTINGS_CHANGED;
use crate::models::PlatformInfo;
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
