//! 播放控制命令。
//!
//! 所有命令都是「转发 → 立即返回」，真正的状态变化通过
//! `leon://player/state` 事件回到前端。命令本身不返回播放状态，
//! 避免前端拿两份真相。

use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use crate::audio::engine::{EngineCommand, SharedState};
use crate::error::{AppError, AppResult};
use crate::models::{
    AudioDiag, Lyrics, PlaybackStatus, PlayerSnapshot, QueueSnapshot, RepeatMode, Track,
    TrackSource,
};
use crate::state::AppState;

fn shared(app: &AppHandle) -> Arc<SharedState> {
    app.state::<AppState>().engine.shared().clone()
}

fn send(app: &AppHandle, cmd: EngineCommand) {
    app.state::<AppState>().engine.send(cmd);
}

#[tauri::command]
pub fn player_snapshot(app: AppHandle) -> PlayerSnapshot {
    shared(&app).snapshot()
}

#[tauri::command]
pub fn player_queue(app: AppHandle) -> QueueSnapshot {
    shared(&app).queue_snapshot()
}

#[tauri::command]
pub fn player_toggle(app: AppHandle) {
    send(&app, EngineCommand::Toggle);
}

#[tauri::command]
pub fn player_play(app: AppHandle) {
    send(&app, EngineCommand::Resume);
}

#[tauri::command]
pub fn player_pause(app: AppHandle) {
    send(&app, EngineCommand::Pause);
}

#[tauri::command]
pub fn player_stop(app: AppHandle) {
    send(&app, EngineCommand::Stop);
}

#[tauri::command]
pub fn player_next(app: AppHandle) {
    send(&app, EngineCommand::Next);
}

#[tauri::command]
pub fn player_prev(app: AppHandle) {
    send(&app, EngineCommand::Prev);
}

#[tauri::command]
pub fn player_seek(app: AppHandle, position_ms: u64) {
    send(&app, EngineCommand::Seek(position_ms));
}

#[tauri::command]
pub fn player_set_volume(app: AppHandle, volume: f32) {
    let v = volume.clamp(0.0, 1.0);
    send(&app, EngineCommand::SetVolume(v));

    // 播放条的音量滑块 / 静音按钮也必须写回设置。
    // 原来只改引擎：设置页仍显示旧值，重启后又从数据库恢复，
    // 用户看到的是「刚才静音莫名其妙自己好了」，而且完全无从自查。
    let st = app.state::<AppState>();
    let snapshot = {
        let mut guard = st.settings.write();
        guard.volume = v;
        guard.clone()
    };
    if let Err(e) = st.db.save_settings(&snapshot) {
        tracing::warn!("persist volume failed: {e}");
    }
}

/// 音频输出自检：把「声音到底卡在哪一段」翻译成设置页能直接读的数据。
#[tauri::command]
pub fn player_audio_diag(app: AppHandle) -> AppResult<AudioDiag> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    send(&app, EngineCommand::AudioDiag(tx));
    let mut d = rx
        .recv_timeout(Duration::from_millis(800))
        .map_err(|e| AppError::audio(format!("音频自检超时：{e}")))?;
    d.hint = audio_hint(&d);
    Ok(d)
}

/// 运行期间切换了系统默认输出设备时，用这条重新绑定（保持当前播放位置）。
#[tauri::command]
pub fn player_audio_reset(app: AppHandle) {
    send(&app, EngineCommand::ResetOutput);
}

fn audio_hint(d: &AudioDiag) -> String {
    if d.device.is_empty() {
        return "音频输出还没打开；点一次播放就会连接系统默认设备。".into();
    }
    if let Some(e) = &d.device_error {
        return format!("设备回调报错：{e}。点「重新连接」再试。");
    }
    if d.volume <= 0.0001 {
        return "应用内音量为 0（已静音）：点播放条的喇叭图标或把音量拉高。".into();
    }
    if d.played_frames == 0 {
        return "设备还没消费过音频帧，可能输出设备选错了；点「重新连接」。".into();
    }
    if d.buffered_samples == 0 && d.underruns > 0 {
        return "解码缓冲为空（有欠载）：音源已结束或读取失败。".into();
    }
    format!(
        "音频输出正常（{}，{} Hz / {} 声道）。若仍听不到，检查系统默认设备、该应用的音量合成器条目与物理音量。",
        d.device, d.sample_rate, d.channels
    )
}

#[tauri::command]
pub fn player_set_speed(app: AppHandle, speed: f32) {
    send(&app, EngineCommand::SetSpeed(speed));
}

#[tauri::command]
pub fn player_set_pitch(app: AppHandle, semitones: f32) {
    send(&app, EngineCommand::SetPitch(semitones));
}

#[tauri::command]
pub fn player_set_gain(app: AppHandle, gain_db: f32) {
    send(&app, EngineCommand::SetGain(gain_db));
}

#[tauri::command]
pub fn player_set_repeat(app: AppHandle, mode: String) {
    let mode = match mode.as_str() {
        "off" => RepeatMode::Off,
        "one" => RepeatMode::One,
        _ => RepeatMode::All,
    };
    send(&app, EngineCommand::SetRepeat(mode));
}

#[tauri::command]
pub fn player_set_shuffle(app: AppHandle, enabled: bool) {
    send(&app, EngineCommand::SetShuffle(enabled));
}

/// 队列整体替换并从 `start_index` 开始播放。
#[tauri::command]
pub fn player_play_queue(app: AppHandle, track_ids: Vec<String>, start_index: Option<usize>) -> AppResult<()> {
    let st = app.state::<AppState>();
    let tracks = st.db.tracks_by_ids(&track_ids)?;
    if tracks.is_empty() {
        return Err(AppError::NotFound("no playable tracks in queue".into()));
    }
    // `tracks_by_ids` 查不到的 id 会被静默跳过。数量不一致说明有曲目
    // 不在库里，此时 `start_index` 已经错位——宁可报错，也不要「点了第 5 首
    // 结果播的是第 2 首」。
    if tracks.len() != track_ids.len() {
        return Err(AppError::NotFound(format!(
            "{} of {} tracks are not in library",
            track_ids.len() - tracks.len(),
            track_ids.len()
        )));
    }
    st.engine.send(EngineCommand::SetQueue {
        tracks,
        index: Some(start_index.unwrap_or(0).min(track_ids.len().saturating_sub(1))),
    });
    Ok(())
}

/// 用 Track 对象整体替换队列并从 `start_index` 播放。
///
/// 搜索结果里的曲目**还没入库**，只有 id 的话 `player_play_queue` 查不到。
/// 引擎收到没有 `file_path` 的曲目会自动发 `resolve-request`，
/// 由上层解析直链、落盘，再回来播放——所以这条路不需要先入库。
#[tauri::command]
pub fn player_play_tracks(app: AppHandle, tracks: Vec<Track>, start_index: Option<usize>) {
    let st = app.state::<AppState>();
    st.engine.send(EngineCommand::SetQueue {
        tracks,
        index: Some(start_index.unwrap_or(0)),
    });
}

/// 播放单曲（本地直接播；在线音源先解析 + 缓存）。
#[tauri::command]
pub async fn player_play_track(app: AppHandle, track_id: String, start_ms: Option<u64>) -> AppResult<()> {
    let mut track = {
        let st = app.state::<AppState>();
        st.db
            .get_track(&track_id)?
            .ok_or_else(|| AppError::NotFound(format!("track {track_id}")))?
    };

    ensure_playable(&app, &mut track).await?;

    let st = app.state::<AppState>();
    st.engine.send(EngineCommand::PlayTrack {
        track,
        start_ms: start_ms.unwrap_or(0),
    });
    preload_next(&app);
    Ok(())
}

/// 由 `EVT_RESOLVE_REQUEST` 触发：把在线曲目解析成本地文件后重新发起播放。
///
/// 引擎只负责「发现没文件 → 发事件」，真正的网络与落盘在这里做，
/// 这样音频线程永远不会被 HTTP 阻塞。
pub(crate) async fn resolve_and_play(app: &AppHandle, mut track: Track) -> AppResult<()> {
    ensure_playable(app, &mut track).await?;
    let st = app.state::<AppState>();
    st.engine.send(EngineCommand::PlayTrack {
        track,
        start_ms: 0,
    });
    preload_next(app);
    Ok(())
}

#[tauri::command]
pub fn player_queue_add(app: AppHandle, track_ids: Vec<String>, play_next: bool) -> AppResult<()> {
    let st = app.state::<AppState>();
    let tracks = st.db.tracks_by_ids(&track_ids)?;
    st.engine.send(EngineCommand::QueueAdd { tracks, play_next });
    Ok(())
}

/// 直接把一组 Track 推进队列（搜索结果的「立即播放」用）。
#[tauri::command]
pub fn player_queue_add_tracks(app: AppHandle, tracks: Vec<Track>, play_next: bool) {
    let st = app.state::<AppState>();
    st.engine.send(EngineCommand::QueueAdd { tracks, play_next });
}

#[tauri::command]
pub fn player_queue_remove(app: AppHandle, index: usize) {
    send(&app, EngineCommand::QueueRemove(index));
}

#[tauri::command]
pub fn player_queue_move(app: AppHandle, from: usize, to: usize) {
    send(&app, EngineCommand::QueueMove { from, to });
}

#[tauri::command]
pub fn player_queue_clear(app: AppHandle) {
    send(&app, EngineCommand::QueueClear);
}

/// 当前曲目的歌词（解析后的结构，前端直接渲染）。
#[tauri::command]
pub fn player_lyrics(app: AppHandle) -> AppResult<Lyrics> {
    let st = app.state::<AppState>();
    let Some(track) = st.engine.shared().current_track() else {
        return Ok(Lyrics::empty());
    };

    // 在线音源优先用插件的歌词
    if let TrackSource::Plugin { plugin_id } = &track.source {
        if let Some(text) = st.db.load_lyrics(&track.id)? {
            return Ok(crate::library::parse_lrc(&text, track.duration_ms.max(0) as u64));
        }
        let _ = plugin_id;
    }

    st.library.lyrics_for(&track)
}

/// 拉取在线歌词（异步，走插件后写入缓存）。
#[tauri::command]
pub async fn player_fetch_remote_lyrics(app: AppHandle) -> AppResult<Lyrics> {
    let (track, plugins, db) = {
        let st = app.state::<AppState>();
        (st.engine.shared().current_track(), st.plugins.clone(), st.db.clone())
    };
    let Some(track) = track else {
        return Ok(Lyrics::empty());
    };
    let Some(plugin_id) = track.source.plugin_id().map(|s| s.to_string()) else {
        return Ok(Lyrics::empty());
    };

    if let Some(cached) = db.load_lyrics(&track.id)? {
        return Ok(crate::library::parse_lrc(&cached, track.duration_ms.max(0) as u64));
    }

    let text = plugins.lyric(&plugin_id, &track).await.unwrap_or_default();
    if text.trim().is_empty() {
        return Ok(Lyrics::empty());
    }
    let _ = db.save_lyrics(&track.id, &text, "plain");
    Ok(crate::library::parse_lrc(&text, track.duration_ms.max(0) as u64))
}

// ---- 桌面悬浮歌词 ----
// show/hide/toggle 返回操作后的可见状态，前端开关按钮直接消费。

#[tauri::command]
pub fn lyric_window_show(app: AppHandle) -> AppResult<bool> {
    app.state::<AppState>().platform.show_lyric_window(&app)
}

#[tauri::command]
pub fn lyric_window_hide(app: AppHandle) -> AppResult<bool> {
    app.state::<AppState>().platform.hide_lyric_window(&app)
}

#[tauri::command]
pub fn lyric_window_toggle(app: AppHandle) -> AppResult<bool> {
    app.state::<AppState>().platform.toggle_lyric_window(&app)
}

#[tauri::command]
pub fn lyric_window_visible(app: AppHandle) -> bool {
    app.state::<AppState>().platform.lyric_window_visible()
}

// ---------------------------------------------------------------------------
// 在线音源解析
// ---------------------------------------------------------------------------

async fn ensure_playable(app: &AppHandle, track: &mut Track) -> AppResult<()> {
    ensure_playable_ex(app, track, true).await.map(|_| ())
}

/// 预解析下一首：当前曲目开播后后台解析队列 `current + 1` 的直链放进全局
/// 槽（`state::pre_resolved`），切歌时 `ensure_playable` 命中直接取用。
/// 解析失败静默忽略——预解析只是热身，不能反过来影响播放。
fn preload_next(app: &AppHandle) {
    let snap = app.state::<AppState>().engine.shared().queue_snapshot();
    let Some(cur) = snap.current else { return };
    let Some(next) = snap.items.get(cur + 1).cloned() else { return };
    // 只有「无本地文件、无直链的插件源」才值得预解析
    if next.file_path.is_some() || next.stream_url.is_some() {
        return;
    }
    if next.source.plugin_id().is_none() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut t = next;
        match ensure_playable_ex(&app, &mut t, false).await {
            Ok(quality) => {
                *crate::state::pre_resolved().lock() = Some(crate::state::PreResolved {
                    track: t,
                    quality,
                });
            }
            Err(e) => {
                tracing::debug!("preload next track failed (ignored): {e}");
            }
        }
    });
}

/// `affect_ui = false` 是预解析模式：不写 Buffering 状态、不发事件、
/// 不覆盖当前播放曲目在 UI 上的规格显示。
/// 返回解析出的真实规格（预解析用它填充槽；UI 模式忽略）。
async fn ensure_playable_ex(
    app: &AppHandle,
    track: &mut Track,
    affect_ui: bool,
) -> AppResult<Option<crate::models::StreamQuality>> {
    let (plugins, engine) = {
        let st = app.state::<AppState>();
        (st.plugins.clone(), st.engine.clone())
    };

    if let Some(p) = &track.file_path {
        if std::path::Path::new(p).exists() {
            // 本地/流媒体文件没有「在线解析规格」。在线曲命中缓存时保留上次的
            // 规格，snapshot() 会按 trackId 过滤，切歌后自然不显示。
            if affect_ui && track.source.plugin_id().is_none() {
                engine.shared().set_quality(None);
            }
            return Ok(None);
        }
    }

    let Some(plugin_id) = track.source.plugin_id().map(|s| s.to_string()) else {
        return Err(AppError::invalid(format!(
            "track {} has no local file",
            track.id
        )));
    };

    // 预解析槽命中：上一首播放时后台已把这首解析好了，直接取用，
    // 省掉整条解析链的网络等待。
    let hit = {
        let mut slot = crate::state::pre_resolved().lock();
        if slot
            .as_ref()
            .is_some_and(|p| p.track.id == track.id && p.track.stream_url.is_some())
        {
            slot.take()
        } else {
            None
        }
    };
    if let Some(pre) = hit {
        tracing::info!("pre-resolved hit for {}", track.id);
        track.stream_url = pre.track.stream_url;
        track.file_path = pre.track.file_path;
        if affect_ui {
            engine.shared().set_quality(pre.quality.clone());
        }
        return Ok(pre.quality);
    }

    // 解析期间不显示上一首的规格；成功后再写入真实规格。
    if affect_ui {
        engine.shared().set_quality(None);
        engine.shared().set_status(PlaybackStatus::Buffering);
        let _ = app.emit(crate::events::EVT_BUFFERING, serde_json::json!({ "trackId": track.id }));
    }

    let stream = plugins.stream_url(&plugin_id, track).await?;

    // 流式起播：直链立即交给引擎（Session 边下边播，不等整曲下载），
    // 下载线程落盘完成后回填 file_path（DB + 内存队列），重播秒开。
    // 解析期间仍是 Buffering 状态，Session 打开后直接 Playing。
    track.stream_url = Some(stream.url);
    track.file_path = None;
    if affect_ui {
        engine.shared().set_quality(stream.quality.clone());
    }
    Ok(stream.quality)
}
