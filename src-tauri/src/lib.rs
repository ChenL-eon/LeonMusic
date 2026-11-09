//! LeonMusic —— Tauri v2 + Rust 音频内核。
//!
//! # 分层
//!
//! ```text
//! ┌──────────────────────────────────────────────┐
//! │ Vue3 + Pinia（只读镜像，不做播放决策）          │
//! └───────────────┬──────────────────────────────┘
//!                 │ invoke（命令，单向）
//!                 │ emit（状态，单向）
//! ┌───────────────▼──────────────────────────────┐
//! │ commands  —— 参数校验 / 数据装配 / 转发         │
//! ├──────────────────────────────────────────────┤
//! │ audio::engine —— 唯一播放状态源（独立线程）     │
//! │ library / db / net / plugin                  │
//! ├──────────────────────────────────────────────┤
//! │ platform（trait）Windows | Android            │
//! └──────────────────────────────────────────────┘
//! ```
//!
//! 关键点：`commands` 层与 `platform` 层都**不持有**播放状态，
//! 状态只在 `audio::engine::SharedState` 里，由引擎线程单写。

mod audio;
mod commands;
mod db;
mod error;
mod events;
mod library;
mod models;
mod net;
mod paths;
mod platform;
mod plugin;
mod state;

use std::sync::Arc;

use tauri::{Listener, Manager, RunEvent};

use audio::engine::{AudioEngine, EngineCommand};
use db::Db;
use library::Library;
use net::{HttpClient, MediaCache};
use plugin::PluginManager;
use state::AppState;

pub use error::{AppError, AppResult};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_tracing();
    install_panic_hook();

    // Android 上不需要再追加桌面插件，所以这里可能用不到 mut
    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_os::init());

    // 桌面专属插件：全局快捷键与单实例在 Android 上没有意义，
    // 依赖也放在 `[target.'cfg(not(target_os = "android"))'.dependencies]`
    #[cfg(not(target_os = "android"))]
    {
        builder = builder
            .plugin(tauri_plugin_global_shortcut::Builder::new().build())
            .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
                // 第二次启动：把已有窗口拉到前台
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.unminimize();
                    let _ = w.set_focus();
                }
            }));
    }

    builder
        .on_window_event(|window, event| {
            // 关窗行为遵循「关闭窗口时最小化到托盘」设置：
            // * 开（默认）→ 拦截关闭，隐藏窗口常驻托盘（音乐继续放）。
            //   旧版没拦 → 主窗口直接销毁，应用却因歌词窗等副窗口存活，
            //   托盘图标还在、窗口已亡——「双击托盘唤不回」的真凶。
            // * 关 → 真退出。不能只让窗口销毁：歌词窗隐藏存活会让
            //   ExitRequested 不触发，进程变成没有窗口的幽灵。
            //   app.exit(0) 会走 RunEvent::ExitRequested（发引擎 Shutdown）。
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() != "main" {
                    return;
                }
                let app = window.app_handle();
                let to_tray = app
                    .try_state::<AppState>()
                    .map(|st| st.settings.read().close_to_tray)
                    .unwrap_or(true);
                if to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                } else {
                    app.exit(0);
                }
            }
        })
        .setup(|app| {
            let handle = app.handle().clone();

            // 1. 路径 & 数据库
            paths::init(&handle)?;
            let db = Arc::new(Db::open(&paths::db_path()?)?);
            db::ensure_system_playlists(&db)?;
            let settings = db.load_settings()?;

            // 2. 平台能力（托盘 / 快捷键 / SMTC / 前台服务）
            let platform = platform::create();
            platform.init(&handle)?;

            // 3. 音频引擎（独立线程，与 WebView 生命周期无关）
            let engine = AudioEngine::spawn(handle.clone(), platform.clone())?;
            platform.attach_engine(engine.sender());
            state::set_engine(engine.clone());

            // 4. 把持久化的播放参数灌进引擎
            // 注意 repeat/shuffle 不在这里灌：播放模式随队列/进度一起
            // 持久化在 playback-state.json（见 audio::resume），由引擎
            // 启动恢复；settings 里遗留的字段只作存储兼容，若再灌会
            // 把恢复出来的模式覆盖回旧值。
            engine.send(EngineCommand::SetVolume(settings.volume));
            engine.send(EngineCommand::SetSpeed(settings.speed));
            engine.send(EngineCommand::SetPitch(settings.pitch));
            engine.send(EngineCommand::SetGain(settings.gain_db));

            // 5. 插件 & 缓存
            let plugins = Arc::new(PluginManager::init(
                db.clone(),
                vec![paths::plugins_dir()?, paths::bundled_plugins_dir(&handle)],
            )?);
            let cache = Arc::new(MediaCache::new(db.clone())?);
            let _ = cache.enforce_limit(settings.cache_limit_mb);
            // 缓存文件可能被外部清理（LRU 淘汰/手动删除/旧版命名升级残留），
            // 失效的 tracks.file_path 会让点播直接 io 报错且无法自愈，启动时清一遍。
            let _ = db.clear_missing_cache_paths();

            app.manage(AppState {
                engine,
                platform,
                library: Arc::new(Library::new(db.clone())),
                plugins,
                cache,
                http: HttpClient::new(),
                db,
                settings: parking_lot::RwLock::new(settings),
            });

            // 5.5 流式播放依赖：引擎线程在 manage AppState 之前就启动了，
            // Session::open 遇到 stream_url 时从这里取（OnceLock，setup 只设一次）。
            state::set_stream_deps(state::StreamDeps {
                cache: app.state::<AppState>().cache.clone(),
                // 与插件解析共用一份客户端：插件那条 Range 探测已经把 CDN
                // 连接建好，起播时直接复用，省掉一次 TLS 握手。
                blocking: crate::net::BlockingHttp::shared(),
                db: app.state::<AppState>().db.clone(),
            });

            tracing::info!("LeonMusic ready");

            // 6. 在线曲目解析回调
            //
            // 引擎线程不能等网络，所以它发现「这首没有本地文件」时只发一个事件，
            // 由这里异步解析（拉流 → 落盘 → 写库），完成后再把 PlayTrack 发回去。
            // 智能音质开启时，解析 / 开播失败会在 `resolve_and_play` 里逐档降级，
            // 降到最低档仍失败才把错误交给 UI，不会无限重试。
            {
                let handle = app.handle().clone();
                app.listen(events::EVT_RESOLVE_REQUEST, move |ev| {
                    let payload = ev.payload().to_string();
                    let app = handle.clone();
                    tauri::async_runtime::spawn(async move {
                        let Ok(track) = serde_json::from_str::<models::Track>(&payload) else {
                            tracing::warn!("bad resolve-request payload");
                            return;
                        };
                        let id = track.id.clone();
                        if let Err(e) = commands::resolve_and_play(&app, track, 0, true).await {
                            tracing::warn!("resolve online track {id} failed: {e}");
                            if let Some(st) = app.try_state::<AppState>() {
                                // detail() 去掉 `plugin: ` 前缀，前端直接 toast 这句话
                                let shared = st.engine.shared();
                                shared.set_error(format!("在线曲目播放失败：{}", e.detail()));
                                // 只写 error 不切状态会永远停在 Buffering，播放键一直转圈
                                shared.set_status(models::PlaybackStatus::Error);
                            }
                        }
                    });
                });
            }

            // 6.5 开播失败 → 降一档音质重试
            //
            // 引擎拿到直链却打不开（门户页 / 坏源 / 过期 CDN）时发这条事件，
            // payload 的 `qualityHint` 已经是下一档；这里重新解析并起播。
            {
                let handle = app.handle().clone();
                app.listen(events::EVT_PLAY_FAILED, move |ev| {
                    let payload = ev.payload().to_string();
                    let app = handle.clone();
                    tauri::async_runtime::spawn(async move {
                        let Ok(track) = serde_json::from_str::<models::Track>(&payload) else {
                            tracing::warn!("bad play-failed payload");
                            return;
                        };
                        tracing::info!(
                            "playback failed for {}, retry at {:?}",
                            track.id,
                            track.quality_hint
                        );
                        let id = track.id.clone();
                        if let Err(e) = commands::resolve_and_play(&app, track, 0, true).await {
                            tracing::warn!("degraded retry for {id} failed: {e}");
                            if let Some(st) = app.try_state::<AppState>() {
                                let shared = st.engine.shared();
                                shared.set_error(format!(
                                    "在线曲目播放失败（已自动降级重试）：{}",
                                    e.detail()
                                ));
                                shared.set_status(models::PlaybackStatus::Error);
                            }
                        }
                    });
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // app
            commands::open_external,
            commands::get_app_version,
            // 应用内更新（下载 → 安装）
            commands::update_download,
            commands::update_apply,
            commands::update_cancel,
            // player
            commands::player_snapshot,
            commands::player_queue,
            commands::player_toggle,
            commands::player_play,
            commands::player_pause,
            commands::player_stop,
            commands::player_next,
            commands::player_prev,
            commands::player_seek,
            commands::player_set_volume,
            commands::player_audio_diag,
            commands::player_audio_reset,
            commands::player_set_speed,
            commands::player_set_pitch,
            commands::player_set_gain,
            commands::player_set_repeat,
            commands::player_set_shuffle,
            commands::player_play_queue,
            commands::player_play_tracks,
            commands::player_play_track,
            commands::player_queue_add,
            commands::player_queue_add_tracks,
            commands::player_queue_remove,
            commands::player_queue_move,
            commands::player_queue_clear,
            commands::player_lyrics,
            commands::player_fetch_remote_lyrics,
            commands::lyric_window_show,
            commands::lyric_window_hide,
            commands::lyric_window_toggle,
            commands::lyric_window_visible,
            // library
            commands::library_scan,
            commands::library_list,
            commands::library_count,
            commands::library_track,
            commands::library_toggle_favorite,
            commands::library_favorites,
            commands::library_recent,
            commands::library_delete,
            commands::library_probe_file,
            commands::library_upsert_tracks,
            // playlist
            commands::playlist_list,
            commands::playlist_create,
            commands::playlist_rename,
            commands::playlist_set_cover,
            commands::playlist_set_remote_key,
            commands::playlist_delete,
            commands::playlist_tracks,
            commands::playlist_add,
            commands::playlist_remove,
            // net / misc
            commands::resolve_redirect,
            // plugin
            commands::plugin_list,
            commands::plugin_set_enabled,
            commands::plugin_set_config,
            commands::plugin_search,
            commands::plugin_resolve_stream,
            commands::plugin_lyric,
            commands::plugin_install,
            commands::plugin_reload,
            commands::plugin_open_dir,
            commands::plugin_login_qr,
            commands::plugin_login_poll,
            commands::plugin_captcha_send,
            commands::plugin_login_cellphone,
            commands::plugin_login_info,
            commands::plugin_logout,
            commands::plugin_recommend_feed,
            commands::plugin_user_playlists,
            commands::plugin_cloud_playlist,
            commands::plugin_scrobble,
            commands::plugin_like_track,
            commands::plugin_is_liked,
            commands::plugin_toplists,
            commands::plugin_personal_fm,
            commands::plugin_similar_songs,
            // settings
            commands::settings_get,
            commands::settings_update,
            commands::app_platform_info,
            commands::cache_size,
            commands::cache_clear,
            commands::cache_enforce_limit,
            commands::request_background_mode,
            commands::app_set_dark_appearance,
            commands::app_data_dir,
            commands::app_media_status,
            commands::app_set_media_options,
            commands::app_open_notification_settings,
            commands::app_background_path,
            commands::app_set_background_image,
            commands::app_clear_background_image,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build LeonMusic")
        .run(|app, event| {
            if let RunEvent::ExitRequested { .. } = event {
                tracing::info!("LeonMusic exiting");
                // 让引擎线程走正常收尾（停设备、关解码器），而不是被进程直接砍掉
                if let Some(state) = app.try_state::<AppState>() {
                    state.engine.send(crate::audio::engine::EngineCommand::Shutdown);
                }
            }
        });
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("leonmusic_lib=info,tauri=warn,wry=warn,boa=warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .try_init();
}

/// 全局 panic hook：把 panic 落盘到 `<数据目录>/crash.log`。
///
/// release 里 release 调试器 / 控制台往往看不见 stderr，panic 只留下
/// 「闪退」两个字毫无线索。hook 在任何路径初始化之前就装上，所以这里
/// 不依赖 `paths::init`（OnceLock 未就绪时退回 `%APPDATA%/<identifier>`，
/// 与 tauri 的 app_data_dir 同源）。
fn install_panic_hook() {
    let default_dir = dirs::data_dir()
        .map(|d| d.join("com.leonmusic.player"))
        .unwrap_or_else(std::env::temp_dir);
    std::panic::set_hook(Box::new(move |info| {
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "non-string panic payload".to_string()
        };
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".into());
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<unnamed>").to_string();
        let line = format!(
            "[{}] PANIC thread={name} at {loc} payload: {payload}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        );
        eprint!("{line}");
        let dir = crate::paths::data_dir().unwrap_or_else(|_| default_dir.clone());
        if std::fs::create_dir_all(&dir).is_ok() {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("crash.log"))
            {
                let _ = f.write_all(line.as_bytes());
            }
        }
        // panic = unwind：tokio 任务 panic 变 JoinError、独立线程 panic 只杀线程，
        // 进程存活；这里不做任何额外交互，只负责留证据。
    }));
}
