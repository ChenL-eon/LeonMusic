//! 媒体库命令。

use std::path::PathBuf;

use tauri::{AppHandle, Emitter, Manager};

use crate::error::{AppError, AppResult};
use crate::models::{ScanSummary, Track};
use crate::state::AppState;

#[tauri::command]
pub async fn library_scan(app: AppHandle, paths: Vec<String>) -> AppResult<ScanSummary> {
    if paths.is_empty() {
        return Err(AppError::invalid("no path provided"));
    }
    let (db, app2, dirs) = {
        let st = app.state::<AppState>();
        let dirs: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        // 记住扫描目录
        {
            let mut s = st.settings.write();
            for p in &paths {
                if !s.library_paths.contains(p) {
                    s.library_paths.push(p.clone());
                }
            }
            let _ = st.db.save_settings(&s);
        }
        (st.db.clone(), app.clone(), dirs)
    };

    tauri::async_runtime::spawn_blocking(move || crate::library::scan_paths(&db, dirs, &app2))
        .await
        .map_err(|e| AppError::invalid(format!("scan join: {e}")))?
}

#[tauri::command]
pub fn library_list(
    app: AppHandle,
    query: Option<String>,
    source: Option<String>,
    page: Option<i64>,
    page_size: Option<i64>,
) -> AppResult<Vec<Track>> {
    let st = app.state::<AppState>();
    let page = page.unwrap_or(0).max(0);
    let size = page_size.unwrap_or(200).clamp(1, 1000);
    st.db
        .list_tracks(query.as_deref(), source.as_deref(), size, page * size)
}

#[tauri::command]
pub fn library_count(app: AppHandle, query: Option<String>) -> AppResult<i64> {
    app.state::<AppState>().db.count_tracks(query.as_deref())
}

#[tauri::command]
pub fn library_track(app: AppHandle, id: String) -> AppResult<Option<Track>> {
    app.state::<AppState>().db.get_track(&id)
}

#[tauri::command]
pub fn library_toggle_favorite(app: AppHandle, track_id: String) -> AppResult<bool> {
    app.state::<AppState>().library.toggle_favorite(&track_id)
}

/// 批量写入远程曲目（云端歌单导入用）。
/// 只 upsert 元数据，不动收藏/歌单 —— 挂接由前端编排（playlist_add / favorites）。
/// 成功后发 library_changed，前端各 store 自动刷新。
#[tauri::command]
pub fn library_upsert_tracks(app: AppHandle, tracks: Vec<Track>) -> AppResult<usize> {
    let st = app.state::<AppState>();
    let n = st.db.upsert_tracks(&tracks)?;
    let _ = app.emit(crate::events::EVT_LIBRARY_CHANGED, ());
    Ok(n)
}

#[tauri::command]
pub fn library_favorites(app: AppHandle) -> AppResult<Vec<Track>> {
    let st = app.state::<AppState>();
    let ids = st.db.favorite_ids()?;
    st.db.tracks_by_ids(&ids)
}

#[tauri::command]
pub fn library_recent(app: AppHandle, limit: Option<i64>) -> AppResult<Vec<Track>> {
    app.state::<AppState>()
        .db
        .recent_tracks(limit.unwrap_or(100).clamp(1, 500))
}

#[tauri::command]
pub fn library_delete(app: AppHandle, id: String) -> AppResult<()> {
    app.state::<AppState>().db.delete_track(&id)
}

/// 直接读一个音频文件的信息（用于拖拽单曲播放）。
#[tauri::command]
pub fn library_probe_file(app: AppHandle, path: String) -> AppResult<Track> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err(AppError::NotFound(path));
    }
    let scanned = crate::library::read_track(&p)?;
    let st = app.state::<AppState>();
    st.db.upsert_track(&scanned.track)?;
    Ok(scanned.track)
}
