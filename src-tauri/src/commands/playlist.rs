//! 歌单命令。

use tauri::{AppHandle, Manager};

use crate::error::{AppError, AppResult};
use crate::models::{Playlist, Track};
use crate::state::AppState;

#[tauri::command]
pub fn playlist_list(app: AppHandle) -> AppResult<Vec<Playlist>> {
    app.state::<AppState>().db.list_playlists()
}

#[tauri::command]
pub fn playlist_create(app: AppHandle, name: String) -> AppResult<Playlist> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::invalid("playlist name is empty"));
    }
    app.state::<AppState>().db.create_playlist(name)
}

#[tauri::command]
pub fn playlist_rename(app: AppHandle, id: String, name: String) -> AppResult<()> {
    app.state::<AppState>().db.rename_playlist(&id, name.trim())
}

#[tauri::command]
pub fn playlist_set_cover(app: AppHandle, id: String, cover_url: Option<String>) -> AppResult<()> {
    app.state::<AppState>()
        .db
        .set_playlist_cover_url(&id, cover_url.as_deref())
}

#[tauri::command]
pub fn playlist_set_remote_key(
    app: AppHandle,
    id: String,
    remote_key: Option<String>,
) -> AppResult<()> {
    app.state::<AppState>()
        .db
        .set_playlist_remote_key(&id, remote_key.as_deref())
}

#[tauri::command]
pub fn playlist_delete(app: AppHandle, id: String) -> AppResult<()> {
    app.state::<AppState>().db.delete_playlist(&id)
}

#[tauri::command]
pub fn playlist_tracks(app: AppHandle, id: String) -> AppResult<Vec<Track>> {
    app.state::<AppState>().db.playlist_tracks(&id)
}

#[tauri::command]
pub fn playlist_add(app: AppHandle, id: String, track_ids: Vec<String>) -> AppResult<()> {
    app.state::<AppState>().db.add_to_playlist(&id, &track_ids)
}

#[tauri::command]
pub fn playlist_remove(app: AppHandle, id: String, track_ids: Vec<String>) -> AppResult<()> {
    app.state::<AppState>()
        .db
        .remove_from_playlist(&id, &track_ids)
}
