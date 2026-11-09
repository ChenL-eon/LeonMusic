//! 音频引擎：Rust 侧唯一的播放状态源。
//!
//! # 线程模型
//!
//! ```text
//!  Tauri command ──send──▶ ┌──────────────────────────┐
//!                          │ engine thread            │
//!                          │  recv command (8ms tick) │
//!                          │  decode → DSP → ring     │
//!                          └───────────┬──────────────┘
//!                                      │ rtrb (SPSC)
//!                          ┌───────────▼──────────────┐
//!                          │ audio callback (OS)      │
//!                          │  pop + volume + convert  │
//!                          └──────────────────────────┘
//!                                      │
//!  UI ◀──emit(120ms)── ticker thread ◀─┘ 读 SharedState（原子量）
//! ```
//!
//! 关键约束：
//! 1. **引擎线程与 WebView 完全解耦**——Android 上 WebView 被销毁/回收，
//!    引擎线程与前台 Service 仍然持有播放状态，音频继续。
//! 2. 前端永远只拿到 `PlayerSnapshot` 的副本，不存在「前端状态覆盖 Rust 状态」的路径。
//! 3. 位置由 `played_frames`（设备真实消费的帧数）推导，暂停 / seek / 变速都不漂移。
//! 4. **输出流常驻**：整个进程只创建一次 cpal stream，切歌只换解码源。
//!    这样 Android 不会反复申请 AudioTrack / 音频焦点，切歌看不出接缝。

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender};
use parking_lot::RwLock;
use tauri::{AppHandle, Emitter, Manager};

use super::decoder::DecodeSource;
use super::dsp::{adapt_channels, db_to_linear, soft_clip, StreamResampler, Wsola};
use super::queue::{PlayQueue, QueueSnapshot};
use super::resume;
use super::sink::{build_pipeline, AudioPipeline};
use crate::error::{AppError, AppResult};
use crate::events;
use crate::models::{PlaybackStatus, PlayerSnapshot, RepeatMode, StreamQuality, Track};
use crate::platform::PlatformHost;

const TICK: Duration = Duration::from_millis(8);
const CHUNK_FRAMES: usize = 2048;
/// 输出流采样率：Android 原生 48k，桌面也统一 48k，所有源重采样到这里
const OUTPUT_RATE: u32 = 48_000;
const OUTPUT_CHANNELS: u16 = 2;

// ---------------------------------------------------------------------------
// 共享状态
// ---------------------------------------------------------------------------

pub struct SharedState {
    status: AtomicU8,
    position_ms: AtomicU64,
    duration_ms: AtomicU64,
    volume: AtomicU32,
    speed: AtomicU32,
    pitch_semitones: AtomicU32,
    gain_db: AtomicU32,
    repeat: AtomicU8,
    shuffle: AtomicBool,
    /// 是否被临时压低音量（Android 导航播报 / 系统提示音抢占「可 duck」焦点）
    ducked: AtomicBool,
    output_rate: AtomicU32,
    /// 当前曲目源文件的采样率（Session 打开时探测）；0 = 没有活动会话。
    source_rate: AtomicU32,
    revision: AtomicU64,
    track: RwLock<Option<Track>>,
    queue: RwLock<QueueSnapshot>,
    error: RwLock<Option<String>>,
    /// 最近一次在线解析的真实规格；带 trackId，切歌后快照自动不显示。
    quality: RwLock<Option<StreamQuality>>,
    /// 待恢复的播放位置：只有启动恢复的那首歌第一次开播时才消费一次。
    /// `(track_id, position_ms)`；见 `audio::resume`。
    pending_resume: parking_lot::Mutex<Option<(String, u64)>>,
    /// 引擎启动恢复（含「无历史」判定）已完成。ticker 在此之前不能
    /// 清理/保存状态文件——它可能跑在引擎恢复段前面，把刚写好的
    /// 状态文件误删或用默认值覆盖。
    resume_done: AtomicBool,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            status: AtomicU8::new(PlaybackStatus::Idle as u8),
            position_ms: AtomicU64::new(0),
            duration_ms: AtomicU64::new(0),
            volume: AtomicU32::new(0.85f32.to_bits()),
            speed: AtomicU32::new(1.0f32.to_bits()),
            pitch_semitones: AtomicU32::new(0.0f32.to_bits()),
            gain_db: AtomicU32::new(0.0f32.to_bits()),
            repeat: AtomicU8::new(RepeatMode::All as u8),
            shuffle: AtomicBool::new(false),
            ducked: AtomicBool::new(false),
            output_rate: AtomicU32::new(0),
            source_rate: AtomicU32::new(0),
            revision: AtomicU64::new(0),
            track: RwLock::new(None),
            queue: RwLock::new(QueueSnapshot {
                items: Vec::new(),
                current: None,
                repeat: RepeatMode::All,
                shuffle: false,
            }),
            error: RwLock::new(None),
            quality: RwLock::new(None),
            pending_resume: parking_lot::Mutex::new(None),
            resume_done: AtomicBool::new(false),
        }
    }
}

fn status_from_u8(v: u8) -> PlaybackStatus {
    match v {
        0 => PlaybackStatus::Idle,
        1 => PlaybackStatus::Buffering,
        2 => PlaybackStatus::Playing,
        3 => PlaybackStatus::Paused,
        4 => PlaybackStatus::Stopped,
        _ => PlaybackStatus::Error,
    }
}

impl SharedState {
    #[inline]
    fn bump(&self) -> u64 {
        self.revision.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn status(&self) -> PlaybackStatus {
        status_from_u8(self.status.load(Ordering::Acquire))
    }

    pub fn set_status(&self, s: PlaybackStatus) {
        self.status.store(s as u8, Ordering::Release);
        self.bump();
    }

    pub fn position_ms(&self) -> u64 {
        self.position_ms.load(Ordering::Relaxed)
    }

    pub fn duration_ms(&self) -> u64 {
        self.duration_ms.load(Ordering::Relaxed)
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }

    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
        self.bump();
    }

    pub fn speed(&self) -> f32 {
        f32::from_bits(self.speed.load(Ordering::Relaxed))
    }

    pub fn set_speed(&self, v: f32) {
        self.speed.store(v.clamp(0.25, 4.0).to_bits(), Ordering::Relaxed);
        self.bump();
    }

    pub fn pitch_semitones(&self) -> f32 {
        f32::from_bits(self.pitch_semitones.load(Ordering::Relaxed))
    }

    pub fn set_pitch(&self, v: f32) {
        self.pitch_semitones
            .store(v.clamp(-12.0, 12.0).to_bits(), Ordering::Relaxed);
        self.bump();
    }

    pub fn pitch_ratio(&self) -> f32 {
        2f32.powf(self.pitch_semitones() / 12.0)
    }

    pub fn gain_db(&self) -> f32 {
        f32::from_bits(self.gain_db.load(Ordering::Relaxed))
    }

    pub fn set_gain(&self, v: f32) {
        self.gain_db.store(v.clamp(-24.0, 24.0).to_bits(), Ordering::Relaxed);
        self.bump();
    }

    pub fn repeat(&self) -> RepeatMode {
        match self.repeat.load(Ordering::Relaxed) {
            0 => RepeatMode::Off,
            1 => RepeatMode::All,
            _ => RepeatMode::One,
        }
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle.load(Ordering::Relaxed)
    }

    /// duck 是**临时**音量压制，不进 `PlayerSnapshot`、不动 `volume`，
    /// 所以前端滑块不会因为一个导航播报就跳一下。
    pub fn is_ducked(&self) -> bool {
        self.ducked.load(Ordering::Relaxed)
    }

    pub fn set_duck(&self, on: bool) {
        self.ducked.store(on, Ordering::Relaxed);
    }

    /// 由 commands 层写入错误（例如在线曲目解析失败）。
    pub fn set_error(&self, msg: impl Into<String>) {
        *self.error.write() = Some(msg.into());
        self.bump();
    }

    /// 在线解析完成后写入真实规格。`None` 用于播放本地文件时清掉残留。
    pub fn set_quality(&self, q: Option<StreamQuality>) {
        *self.quality.write() = q;
        self.bump();
    }

    pub fn current_track(&self) -> Option<Track> {
        self.track.read().clone()
    }

    pub fn queue_snapshot(&self) -> QueueSnapshot {
        self.queue.read().clone()
    }

    /// 记住「下次点播放要从这里继续」的位置（启动恢复写入）。
    pub fn set_resume(&self, track_id: String, position_ms: u64) {
        *self.pending_resume.lock() = Some((track_id, position_ms));
    }

    /// 只在当前要播的正是恢复的那首歌时返回记忆位置并消费，
    /// 避免之后手动重播同一首又被拉回旧进度。
    pub fn take_resume_for(&self, track_id: &str) -> Option<u64> {
        let mut g = self.pending_resume.lock();
        if g.as_ref().is_some_and(|(id, _)| id == track_id) {
            g.take().map(|(_, pos)| pos)
        } else {
            None
        }
    }

    /// 只读不消费：Buffering 期间进度条仍显示记忆位置。
    pub fn peek_resume_for(&self, track_id: &str) -> Option<u64> {
        self.pending_resume
            .lock()
            .iter()
            .find(|(id, _)| id == track_id)
            .map(|(_, pos)| *pos)
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        let track = self.current_track();
        // 先把 id 取出来：`track` 稍后会被移进快照，闭包里不能再借它。
        let current_id = track.as_ref().map(|t| t.id.clone());
        let q = self.queue.read();
        PlayerSnapshot {
            status: self.status(),
            track,
            position_ms: self.position_ms(),
            duration_ms: self.duration_ms(),
            volume: self.volume(),
            speed: self.speed(),
            pitch_semitones: self.pitch_semitones(),
            gain_db: self.gain_db(),
            repeat: self.repeat(),
            shuffle: self.shuffle(),
            queue_index: q.current,
            queue_len: q.items.len(),
            output_sample_rate: self.output_rate.load(Ordering::Relaxed),
            source_sample_rate: self.source_rate.load(Ordering::Relaxed),
            error: self.error.read().clone(),
            // 缓存命中的重复播放也会走这里：只要还是同一首就继续显示规格。
            quality: self.quality.read().clone().filter(|q| {
                !q.track_id.is_empty() && current_id.as_deref() == Some(q.track_id.as_str())
            }),
            revision: self.revision.load(Ordering::Acquire),
        }
    }
}

// ---------------------------------------------------------------------------
// 命令
// ---------------------------------------------------------------------------

pub enum EngineCommand {
    PlayTrack { track: Track, start_ms: u64 },
    Pause,
    Resume,
    Toggle,
    Stop,
    Seek(u64),
    SetVolume(f32),
    SetSpeed(f32),
    SetPitch(f32),
    SetGain(f32),
    SetRepeat(RepeatMode),
    SetShuffle(bool),
    /// 临时压低音量（音频焦点「可 duck」时由 Android 平台层发出）
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    Duck(bool),
    SetQueue { tracks: Vec<Track>, index: Option<usize> },
    QueueAdd { tracks: Vec<Track>, play_next: bool },
    QueueRemove(usize),
    QueueMove { from: usize, to: usize },
    QueueClear,
    /// 流式落盘完成：把缓存路径同步回内存队列，重播直接命中本地文件。
    UpdateTrackPath { track_id: String, path: String },
    Next,
    Prev,
    /// 请求一次音频输出自检。命令层通过 oneshot 收结果，避免把 pipeline
    /// （含 !Send 的 stream 句柄）暴露到引擎线程之外。
    AudioDiag(Sender<crate::models::AudioDiag>),
    /// 重新打开输出流。输出流是常驻的，只在启动时绑定一次系统默认设备；
    /// 用户运行期间切换了默认设备（插耳机 / 连蓝牙）时，声音仍会往旧设备送。
    ResetOutput,
    Shutdown,
}

// ---------------------------------------------------------------------------
// 引擎句柄
// ---------------------------------------------------------------------------

pub struct AudioEngine {
    tx: Sender<EngineCommand>,
    shared: Arc<SharedState>,
}

impl AudioEngine {
    pub fn spawn(app: AppHandle, platform: Arc<dyn PlatformHost>) -> AppResult<Arc<Self>> {
        let (tx, rx) = bounded::<EngineCommand>(256);
        let shared = Arc::new(SharedState::default());

        let engine_shared = shared.clone();
        let engine_app = app.clone();
        let engine_platform = platform.clone();
        thread::Builder::new()
            .name("leon-audio-engine".into())
            .spawn(move || {
                if let Err(e) = run_engine(rx, engine_shared, engine_app, engine_platform) {
                    tracing::error!("audio engine stopped: {e}");
                }
            })
            .map_err(AppError::audio)?;

        let tick_shared = shared.clone();
        let tick_app = app.clone();
        thread::Builder::new()
            .name("leon-state-ticker".into())
            .spawn(move || ticker(tick_app, tick_shared, platform))
            .map_err(AppError::audio)?;

        Ok(Arc::new(Self { tx, shared }))
    }

    pub fn send(&self, cmd: EngineCommand) {
        if self.tx.send(cmd).is_err() {
            tracing::warn!("audio engine command channel closed");
        }
    }

    pub fn shared(&self) -> &Arc<SharedState> {
        &self.shared
    }

    /// 平台层（SMTC / MediaSession / 托盘）通过它回送控制指令。
    pub fn sender(&self) -> Sender<EngineCommand> {
        self.tx.clone()
    }
}

/// 每 120ms 把完整状态推给前端，同时按更低频率同步到系统媒体控件。
fn ticker(app: AppHandle, shared: Arc<SharedState>, platform: Arc<dyn PlatformHost>) {
    let mut last_platform = std::time::Instant::now();
    let mut last_right = String::new();

    // 播放历史：切歌时把上一首真正听了多久写进库
    let mut last_track_id = String::new();
    let mut last_position_ms = 0u64;
    let mut listened_ms = 0u64;

    // 播放进度记忆：队列/曲目/模式变化立即落盘，播放中每 5 秒刷进度
    let mut last_saved_sig = String::new();
    let mut last_saved_at = std::time::Instant::now();

    loop {
        thread::sleep(Duration::from_millis(120));
        let snap = shared.snapshot();
        let right = format!(
            "{}|{}|{}|{}",
            snap.track.as_ref().map(|t| t.id.as_str()).unwrap_or(""),
            snap.status as u8,
            snap.duration_ms,
            snap.queue_len
        );
        // 曲目/状态类变化才更新系统媒体面板，进度更新单独走
        if right != last_right {
            last_right = right;
            platform.update_now_playing(&snap);
        }
        let _ = app.emit(events::EVT_PLAYER_STATE, &snap);

        if last_platform.elapsed() >= Duration::from_millis(900) {
            last_platform = std::time::Instant::now();
            platform.update_position(snap.position_ms, snap.status == PlaybackStatus::Playing);
        }

        // ---- 播放历史 ----
        let current_id = snap.track.as_ref().map(|t| t.id.as_str()).unwrap_or("");
        if current_id != last_track_id {
            if !last_track_id.is_empty() {
                record_history(&app, &last_track_id, listened_ms);
            }
            last_track_id = current_id.to_string();
            listened_ms = 0;
            last_position_ms = snap.position_ms;
            continue;
        }
        if snap.status == PlaybackStatus::Playing {
            let delta = snap.position_ms.saturating_sub(last_position_ms);
            // 一次 tick 才 120ms，delta 超过 3 秒只能是用户拖了进度条，
            // 这种「跳跃」不该算成听了这么久
            if delta <= 3_000 {
                listened_ms += delta;
            }
        }
        last_position_ms = snap.position_ms;

        // ---- 播放进度记忆 ----
        // 变化信号：曲目 | 模式 | 队列长度。播放中每 5 秒额外刷一次
        // 进度（seek 不改变信号，最多回退 5 秒，可接受）。
        // 恢复完成前不动状态文件（见 resume_done 字段说明）。
        if shared.resume_done.load(Ordering::Acquire) {
            let save_sig = format!(
                "{}|{}|{}|{}",
                current_id,
                shared.repeat() as u8,
                shared.shuffle(),
                snap.queue_len
            );
            if save_sig != last_saved_sig {
                last_saved_sig = save_sig;
                last_saved_at = std::time::Instant::now();
                if current_id.is_empty() {
                    resume::clear_async();
                } else {
                    resume::save_async(&shared);
                }
            } else if snap.status == PlaybackStatus::Playing
                && last_saved_at.elapsed() >= Duration::from_millis(5_000)
            {
                last_saved_at = std::time::Instant::now();
                resume::save_async(&shared);
            }
        }
    }
}

/// 把一次收听写进 `play_history`。
///
/// 门槛是 5 秒：快速划过十几首歌不该污染「最近播放」。
/// 写库放在独立线程，避免 SQLite 的 fsync 拖慢 120ms 的状态推送节奏。
fn record_history(app: &AppHandle, track_id: &str, listened_ms: u64) {
    if listened_ms < 5_000 {
        return;
    }
    let Some(state) = app.try_state::<crate::state::AppState>() else {
        return;
    };
    let db = state.db.clone();
    let id = track_id.to_string();
    let played = listened_ms as i64;
    thread::Builder::new()
        .name("leon-history".into())
        .spawn(move || {
            if let Err(e) = db.record_play(&id, played) {
                tracing::warn!("record play failed: {e}");
            }
        })
        .ok();
}

// ---------------------------------------------------------------------------
// 播放会话（只持有解码与 DSP，输出流是全局的）
// ---------------------------------------------------------------------------

struct Session {
    source: DecodeSource,
    wsola: Wsola,
    resampler: StreamResampler,
    /// 源文件采样率（探测自解码器），快照用来区分「文件规格」和「输出规格」。
    src_rate: u32,
    /// 锚点：`played_frames` 等于 `anchor_frames` 时，内容位置是 `anchor_ms`。
    ///
    /// 为什么不能直接 `base_ms + 已播秒数`：`played_frames` 是**墙钟**帧数，
    /// 2 倍速播放时 1 秒墙钟会消耗 2 秒内容。变速必须分段结算，
    /// 否则进度条会一直按 1 倍速走，切歌前还会突然跳。
    anchor_ms: u64,
    anchor_frames: u64,
    /// 上次参与换算的速度（用于检测变化）
    applied_speed: f32,
    /// DSP 已经算好、但 ring buffer 当时放不下的样本（按交错 f32 存）。
    ///
    /// 变速/变调时重采样比不再等于「源率 → 48k」，一块 2048 帧的输入可能
    /// 变成远超 2048 帧的输出；只按入参预估的 slots 阈值会不够，硬推就会
    /// 丢尾部样本（断续 + 颤音）。这里把装不下的部分攒着下一轮再送。
    pending_pcm: Vec<f32>,
    /// `pending_pcm` 已送出的前缀长度（避免每轮 drain 造成 O(n²) 拷贝）
    pending_pos: usize,
    paused: bool,
    eof: bool,
}

impl Session {
    fn open(track: &Track) -> AppResult<Self> {
        let source = match Self::open_source(track) {
            Ok(s) => s,
            Err(e) => {
                // 缓存文件本身不是音频（旧版本曾把门户页/错误页当音频落盘），
                // 删掉它让下次重播走在线解析，否则这首会永远「打开即失败」。
                // 只动缓存目录内的文件，用户本地音乐不受影响。
                if let Some(p) = track.file_path.as_deref() {
                    if let Some(deps) = crate::state::stream_deps() {
                        let path = Path::new(p);
                        if deps.cache.owns(path) {
                            tracing::warn!("discard unreadable cache {p}: {}", e.detail());
                            deps.cache.discard(path);
                        }
                    }
                }
                return Err(e);
            }
        };
        let src_rate = source.sample_rate();
        let channels = OUTPUT_CHANNELS as usize;

        Ok(Self {
            wsola: Wsola::new(src_rate, channels),
            resampler: StreamResampler::new(src_rate, OUTPUT_RATE, channels),
            source,
            src_rate,
            anchor_ms: 0,
            anchor_frames: 0,
            applied_speed: 1.0,
            pending_pcm: Vec::new(),
            pending_pos: 0,
            paused: false,
            eof: false,
        })
    }

    /// 解码源三分支：缓存文件（最快）→ 流式直链（边下边播）→ 本地缺失报错。
    fn open_source(track: &Track) -> AppResult<DecodeSource> {
        if let Some(p) = track.file_path.as_deref() {
            if Path::new(p).exists() {
                return DecodeSource::open(Path::new(p));
            }
        }
        if let Some(url) = track.stream_url.as_deref() {
            return Self::open_streaming(track, url);
        }
        // 本地源 / 缓存路径失效且没有在线兜底：保留原文件打开错误
        // （start_track 的 Err 分支会翻译成可读提示）
        if let Some(p) = track.file_path.as_deref() {
            return DecodeSource::open(Path::new(p));
        }
        Err(AppError::invalid(format!(
            "track {} has no playable path",
            track.id
        )))
    }

    /// 流式会话：先查缓存（resolve 后可能已落盘），未命中起「边下边播」，
    /// 并 spawn 回填线程等落盘完成后更新 DB 与内存队列。
    fn open_streaming(track: &Track, url: &str) -> AppResult<DecodeSource> {
        let Some(deps) = crate::state::stream_deps() else {
            return Err(AppError::audio("stream deps not initialized"));
        };
        if let Some(p) = deps.cache.cached_path(url)? {
            return DecodeSource::open(&p);
        }

        let pending = deps.cache.begin_stream(url, deps.blocking)?;
        let source = DecodeSource::open_stream(pending.stream, url)?;

        // 回填：下载线程 EOF（缓存就位）→ 更新 tracks.file_path + 内存队列。
        // 切歌取消时 sender 被 drop，recv 返回 Err，线程自然退出。
        let db = deps.db.clone();
        let track_id = track.id.clone();
        let spawn = std::thread::Builder::new()
            .name("stream-finalize".into())
            .spawn(move || {
                if let Ok(Ok(path)) = pending.done.recv() {
                    let ps = path.to_string_lossy().into_owned();
                    if let Err(e) = db.update_track_file_path(&track_id, &ps) {
                        tracing::warn!("update track file_path failed: {e}");
                    }
                    if let Some(engine) = crate::state::engine() {
                        engine.send(EngineCommand::UpdateTrackPath { track_id, path: ps });
                    }
                }
            });
        if let Err(e) = spawn {
            tracing::warn!("spawn stream finalize thread failed: {e}");
        }

        Ok(source)
    }

    fn duration_ms(&self) -> u64 {
        self.source.duration_ms()
    }

    /// 源文件采样率（解码探测值）。
    fn source_rate(&self) -> u32 {
        self.src_rate
    }

    /// 内容播放位置。
    ///
    /// 注意音高不参与：变调是「重采样 + WSOLA 预拉伸」的组合，
    /// 两者对时长的作用互相抵消，所以只有 speed 影响内容推进速度。
    fn position_ms(&self, output: &AudioPipeline) -> u64 {
        let played = output.shared.played_frames.load(Ordering::Relaxed);
        let delta_frames = played.saturating_sub(self.anchor_frames) as f64;
        let delta_ms =
            delta_frames / output.config.sample_rate as f64 * 1000.0 * self.applied_speed as f64;
        self.anchor_ms + delta_ms as u64
    }

    /// 把「到此刻为止」的内容位置结算进锚点，然后换用新的速度。
    fn rebase(&mut self, output: &AudioPipeline, speed: f32) {
        self.anchor_ms = self.position_ms(output);
        self.anchor_frames = output.shared.played_frames.load(Ordering::Relaxed);
        self.applied_speed = speed;
    }

    fn seek(&mut self, ms: u64, output: &AudioPipeline) -> AppResult<()> {
        self.source.seek(ms)?;
        output.shared.reset_transport();
        self.wsola.reset();
        self.resampler.reset();
        self.pending_pcm.clear();
        self.pending_pos = 0;
        self.anchor_ms = ms;
        self.anchor_frames = 0;
        self.eof = false;
        Ok(())
    }

    fn set_paused(&mut self, paused: bool, output: &AudioPipeline) -> AppResult<()> {
        // 不能因为 `self.paused == paused` 就提前返回：切歌会新建一个
        // paused=false 的 Session，而设备侧的 stream 可能还挂在上一次的暂停里。
        // 此时点「播放」若直接跳过，设备永远不会恢复 —— 必须按设备真实状态对齐。
        self.paused = paused;
        align_output(paused, output)
    }

    fn pump(&mut self, output: &mut AudioPipeline, shared: &SharedState) -> AppResult<()> {
        // 速度变化时先结算旧速度下的进度，再切新值。
        // 放在最前面（早于 paused/eof 提前返回）保证任何状态下都能对账。
        let speed_now = shared.speed();
        if (speed_now - self.applied_speed).abs() > 1e-4 {
            self.rebase(output, speed_now);
        }

        if self.paused {
            return Ok(());
        }

        // 上一轮没塞进 ring 的样本优先送完，否则它们会被新算出来的
        // 样本挤到后面，听感上就是周期性缺一块。
        // 放在 eof 判断之前：解码到结尾时也要把尾巴送完，不能截断。
        while self.pending_pos < self.pending_pcm.len() {
            match output.producer.push(self.pending_pcm[self.pending_pos]) {
                Ok(()) => self.pending_pos += 1,
                Err(_) => return Ok(()),
            }
        }
        self.pending_pcm.clear();
        self.pending_pos = 0;

        if self.eof {
            return Ok(());
        }

        let channels = OUTPUT_CHANNELS as usize;
        let need = CHUNK_FRAMES * channels;
        if output.producer.slots() < need + 2048 {
            return Ok(());
        }

        let chunk = match self.source.next_chunk(CHUNK_FRAMES)? {
            Some(c) => c,
            None => {
                self.eof = true;
                return Ok(());
            }
        };

        let adapted = adapt_channels(&chunk.samples, chunk.channels, channels);

        // 变速/变调：T = P / S，之后按 P 重采样
        let pitch = shared.pitch_ratio();
        let speed = shared.speed();
        let stretch = (pitch / speed).clamp(0.25, 4.0);
        let stretched = if (stretch - 1.0).abs() > 0.0005 {
            self.wsola.process(&adapted, stretch)
        } else {
            adapted
        };

        // 重采样器本身是「保持时长与音高」的采样率转换，
        // 把它的目标率设成 48k/P（P = 音高比），输出的帧数比输入多 P 倍，
        // 这些样本再以 48k 送进声卡，等效于「加速 P 倍」→ 音高升高 P 倍、
        // 时长缩短 1/P，正好被前面的 WSOLA（拉伸 P 倍）抵消。
        //
        // 这里曾经写成 `OUTPUT_RATE * pitch`，方向反了：变调会让音高反向走，
        // 同时把时长拉成 P² 倍。耳听不太出来（都一样是跑调），但一听就知道不对。
        let target_rate = ((OUTPUT_RATE as f32 / pitch).round() as u32).max(8_000);
        self.resampler.set_target_rate(target_rate);
        let mut pcm = self.resampler.process(&stretched);

        let gain = db_to_linear(shared.gain_db());
        if (gain - 1.0).abs() > 0.0005 {
            for s in pcm.iter_mut() {
                *s = soft_clip(*s * gain);
            }
        }

        // 装不下的部分不能丢：攒进 pending，下一轮先送（见字段注释）
        push_or_park(&mut output.producer, &pcm, &mut self.pending_pcm);
        Ok(())
    }
}

/// 把样本推进 ring；ring 满了之后剩下的**攒进 `pending`**，不能丢。
///
/// 老写法是 `if push(s).is_err() { break; }`：一块 4096 帧的 FLAC 包经
/// 44.1k→48k 重采样后约 8900 样本，而按 2048 帧入参预估的 slots 门槛
/// 只有 6144，尾部 30% 直接被扔掉 —— 听感就是持续断续加颤音。
fn push_or_park(producer: &mut rtrb::Producer<f32>, samples: &[f32], pending: &mut Vec<f32>) {
    let mut blocked = false;
    for &s in samples {
        if !blocked {
            if producer.push(s).is_ok() {
                continue;
            }
            blocked = true;
        }
        pending.push(s);
    }
}

// ---------------------------------------------------------------------------
// 引擎主循环
// ---------------------------------------------------------------------------
fn run_engine(
    rx: Receiver<EngineCommand>,
    shared: Arc<SharedState>,
    app: AppHandle,
    platform: Arc<dyn PlatformHost>,
) -> AppResult<()> {
    let mut queue = PlayQueue::default();
    let mut session: Option<Session> = None;
    let mut output: Option<AudioPipeline> = None;

    // ---- 上次播放恢复 ----
    // 只把队列/模式/当前曲就位（status 仍是 Idle），不自动播放：
    // 用户点播放（Toggle/Resume）才加载解码器，位置在 start_track 里消费。
    if let Some(saved) = resume::load() {
        let cur_track = saved
            .queue
            .current
            .and_then(|i| saved.queue.items.get(i))
            .cloned();
        if let Some(track) = cur_track {
            queue.set_items(saved.queue.items, saved.queue.current);
            queue.set_repeat(saved.queue.repeat);
            queue.set_shuffle(saved.queue.shuffle);
            // 旧存档可能存着 shuffle+One 这种非法组合，一律用归一化后的值回写
            let (shuffle, repeat) = queue.mode();
            shared.repeat.store(repeat as u8, Ordering::Relaxed);
            shared.shuffle.store(shuffle, Ordering::Relaxed);
            *shared.queue.write() = queue.snapshot();
            shared.set_resume(track.id.clone(), saved.position_ms);
            shared
                .duration_ms
                .store(track.duration_ms.max(0) as u64, Ordering::Relaxed);
            shared.position_ms.store(saved.position_ms, Ordering::Relaxed);
            *shared.track.write() = Some(track);
            shared.bump();
            tracing::info!("playback state restored from last session");
        }
    }
    shared.resume_done.store(true, Ordering::Release);

    loop {
        match rx.recv_timeout(TICK) {
            Ok(EngineCommand::Shutdown) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                break;
            }
            Ok(cmd) => {
                handle_command(
                    cmd,
                    &mut queue,
                    &mut session,
                    &mut output,
                    &shared,
                    &app,
                    &platform,
                );
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
        }

        let Some(out) = output.as_mut() else {
            continue;
        };

        if let Some(s) = session.as_mut() {
            if let Err(e) = s.pump(out, &shared) {
                tracing::warn!("decode error: {e}");
                *shared.error.write() = Some(e.to_string());
                shared.set_status(PlaybackStatus::Error);
                session = None;
            } else {
                shared.position_ms.store(s.position_ms(out), Ordering::Relaxed);
            }

            // 自然结束：解码到 EOF 且 ring 已经被设备消费空
            let finished = session
                .as_ref()
                .map(|s| s.eof && out.producer.slots() >= out.capacity)
                .unwrap_or(false);

            if finished {
                let _ = app.emit(events::EVT_TRACK_ENDED, ());
                session = None;
                if let Some(idx) = queue.next(true) {
                    // 自动续播同样要广播：不然「播完一首自动跳下一首」时
                    // 队列面板还停在上一首。
                    broadcast_queue(&queue, &shared, &app);
                    start_index(idx, &queue, &mut session, out, &shared, &app, &platform, 0);
                } else {
                    shared.set_status(PlaybackStatus::Stopped);
                    shared.position_ms.store(0, Ordering::Relaxed);
                    platform.update_playback_state(false);
                }
            }
        }
    }

    drop(session);
    drop(output);
    tracing::info!("audio engine loop exited");
    Ok(())
}

/// 让设备侧的播放状态与 Session 对齐，幂等。
///
/// 暂停会 `suspend()` 输出流；但切歌是新建一个 `paused=false` 的 Session，
/// 不会自动把挂起的 stream 拉回来。后果是：解码照常把前两秒数据填满 ring，
/// 状态是 Playing、前端进度条靠墙钟插值继续走，设备却一个回调都不跑 ——
/// 「有进度没声音」。用户先暂停再播放恰好触发 `resume()` 才出声，而且是从
/// ring 里堆积的开头数据开始，听起来像从头重播。
///
/// 所以开播前和每次暂停/恢复都要按 `is_suspended()` 核对一次，
/// 而不是依赖 Session 自己的布尔值。
fn align_output(paused: bool, out: &AudioPipeline) -> AppResult<()> {
    let suspended = out.output.is_suspended();
    if paused && !suspended {
        out.output.suspend()
    } else if !paused && suspended {
        out.output.resume()
    } else {
        Ok(())
    }
}

fn ensure_output<'a>(
    slot: &'a mut Option<AudioPipeline>,
    shared: &SharedState,
) -> AppResult<&'a mut AudioPipeline> {
    if slot.is_none() {
        let p = build_pipeline(OUTPUT_RATE, OUTPUT_CHANNELS)?;
        p.shared.set_volume(shared.volume());
        p.shared.set_duck(shared.is_ducked());
        shared
            .output_rate
            .store(p.config.sample_rate, Ordering::Relaxed);
        shared.bump();
        *slot = Some(p);
    }
    Ok(slot.as_mut().expect("just inserted"))
}

/// 「打开输出 → 播放第 index 首」的唯一入口。
///
/// 单独抽出来是为了不再写 `if let Ok(out) = ensure_output(..)`：
/// 那样设备打不开时既不报错也不改状态，用户只看到「点了播放没反应」。
/// 这里把失败翻译成可见的状态，UI 才有机会提示。
#[allow(clippy::too_many_arguments)]
fn play_index(
    index: usize,
    queue: &PlayQueue,
    session: &mut Option<Session>,
    output: &mut Option<AudioPipeline>,
    shared: &Arc<SharedState>,
    app: &AppHandle,
    platform: &Arc<dyn PlatformHost>,
) -> bool {
    let out = match ensure_output(output, shared) {
        Ok(o) => o,
        Err(e) => {
            tracing::error!("open audio output failed: {e}");
            shared.set_error(format!("无法打开音频输出：{e}"));
            shared.set_status(PlaybackStatus::Error);
            return false;
        }
    };
    start_index(index, queue, session, out, shared, app, platform, 0)
}

#[allow(clippy::too_many_arguments)]
fn start_index(
    index: usize,
    queue: &PlayQueue,
    session: &mut Option<Session>,
    output: &mut AudioPipeline,
    shared: &SharedState,
    app: &AppHandle,
    platform: &Arc<dyn PlatformHost>,
    start_ms: u64,
) -> bool {
    let Some(track) = queue.items().get(index).cloned() else {
        return false;
    };
    start_track(track, session, output, shared, app, platform, start_ms)
}

#[allow(clippy::too_many_arguments)]
fn start_track(
    mut track: Track,
    session: &mut Option<Session>,
    output: &mut AudioPipeline,
    shared: &SharedState,
    app: &AppHandle,
    platform: &Arc<dyn PlatformHost>,
    start_ms: u64,
) -> bool {
    *session = None;
    // 会话即将重建/失败：清掉上一首的源采样率，避免快照残留旧值
    shared.source_rate.store(0, Ordering::Relaxed);

    // file_path 指向的缓存文件可能已被外部清理（LRU 淘汰、手动删除 media
    // 目录、旧版缓存命名升级后的残留）。插件源把它清空，落到下面的 resolve
    // 分支重新解析自愈；本地源没有在线兜底，保留路径让 Session::open 报错，
    // 由下面的 Err 分支翻译成可读提示。
    if let Some(p) = track.file_path.as_deref() {
        if track.source.plugin_id().is_some() && !std::path::Path::new(p).exists() {
            tracing::info!("cached file gone, re-resolving {}: {p}", track.id);
            track.file_path = None;
            // 旧直链可能已过期（CDN 签名/防盗链），一并清掉重新解析
            track.stream_url = None;
        }
    }

    // 在线曲目还没落盘、也没有解析好的流式直链（从搜索结果直接入队的情况）。
    // 这里不能干等：引擎线程要维持 8ms 节拍，网络 IO 会直接把播放拖死。
    // 所以先把「这首歌 + 缓冲中」推给 UI 和系统面板，再请求上层异步解析，
    // 解析完成后会重新发一条 PlayTrack 回来（带 stream_url，不再进这里）。
    if track.file_path.is_none() && track.stream_url.is_none() {
        *shared.error.write() = None;
        *shared.track.write() = Some(track.clone());
        shared
            .duration_ms
            .store(track.duration_ms.max(0) as u64, Ordering::Relaxed);
        // 恢复场景：解析期间进度条仍显示记忆位置（真正开播时才消费）
        shared
            .position_ms
            .store(shared.peek_resume_for(&track.id).unwrap_or(0), Ordering::Relaxed);
        shared.set_status(PlaybackStatus::Buffering);
        platform.update_now_playing(&shared.snapshot());
        platform.update_playback_state(false);
        let _ = app.emit(events::EVT_RESOLVE_REQUEST, &track);
        let _ = app.emit(events::EVT_PLAYER_STATE, &shared.snapshot());
        return false;
    }

    match Session::open(&track) {
        Ok(mut s) => {
            // 新会话一定是非暂停态：上一次暂停挂起的设备必须在这里恢复，
            // 否则就是「进度条在走但没声音」。失败要显式报错，别假装开播成功。
            if let Err(e) = align_output(false, output) {
                tracing::error!("resume audio output on track start failed: {e}");
                *shared.error.write() = Some(format!("无法恢复音频输出：{e}"));
                shared.set_status(PlaybackStatus::Error);
                platform.update_playback_state(false);
                return false;
            }
            // 切歌：清掉上一首残留在 ring 里的样本
            output.shared.reset_transport();
            // 没有显式起点时，消费「上次播放进度」的记忆（仅启动恢复那首有效）
            let start_ms = if start_ms == 0 {
                shared.take_resume_for(&track.id).unwrap_or(0)
            } else {
                start_ms
            };
            if start_ms > 0 {
                let _ = s.seek(start_ms, output);
            }
            shared.duration_ms.store(
                {
                    let d = s.duration_ms();
                    if d > 0 {
                        d
                    } else {
                        // 流式 MP3 无 Content-Length 时估不出时长，用元数据兜底
                        track.duration_ms.max(0) as u64
                    }
                },
                Ordering::Relaxed,
            );
            shared.position_ms.store(start_ms, Ordering::Relaxed);
            shared.source_rate.store(s.source_rate(), Ordering::Relaxed);
            *shared.track.write() = Some(track);
            shared.set_status(PlaybackStatus::Playing);
            *shared.error.write() = None;
            *session = Some(s);

            platform.update_now_playing(&shared.snapshot());
            platform.update_playback_state(true);
            let _ = app.emit(events::EVT_PLAYER_STATE, &shared.snapshot());
            true
        }
        Err(e) => {
            tracing::error!("play failed: {e}");
            // 智能音质：直链打不开（门户页、坏源、过期的签名 CDN）时先降一档
            // 重试，别急着把错误弹给用户。只对「本次走的是流式直链」的在线曲目
            // 生效——缓存文件坏了跟码率无关，降档救不回来。
            if track.stream_url.is_some() && track.file_path.is_none() {
                if let Some(next) = crate::state::smart_next_level(app, &track) {
                    tracing::info!(
                        "retry {} at lower quality: {:?} -> {next}",
                        track.id,
                        track.quality_hint
                    );
                    let mut retry = track.clone();
                    retry.quality_hint = Some(next.to_string());
                    retry.stream_url = None;
                    *shared.error.write() = None;
                    shared.set_status(PlaybackStatus::Buffering);
                    platform.update_playback_state(false);
                    let _ = app.emit(events::EVT_PLAY_FAILED, &retry);
                    return false;
                }
            }
            // 本地文件丢失是用户能理解的场景（移动/删除/U盘拔出），
            // 裸 io 错误码只会让人困惑，翻译成带歌名的可读提示。
            let msg = match &e {
                AppError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
                    format!("《{}》的本地文件已不存在（可能被移动或删除）", track.title)
                }
                _ => e.to_string(),
            };
            *shared.error.write() = Some(msg);
            shared.set_status(PlaybackStatus::Error);
            platform.update_playback_state(false);
            false
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_command(
    cmd: EngineCommand,
    queue: &mut PlayQueue,
    session: &mut Option<Session>,
    output: &mut Option<AudioPipeline>,
    shared: &Arc<SharedState>,
    app: &AppHandle,
    platform: &Arc<dyn PlatformHost>,
) {
    match cmd {
        EngineCommand::PlayTrack { track, start_ms } => {
            let out = match ensure_output(output, shared) {
                Ok(o) => o,
                Err(e) => {
                    *shared.error.write() = Some(e.to_string());
                    shared.set_status(PlaybackStatus::Error);
                    return;
                }
            };
            // 队列同步。
            //
            // 不能在队列里找不到就什么都不做：前端「单曲播放」这条路
            // （`player_play_track`）进来时，队列往往还是上一批曲目，
            // cursor 会停在完全不相干的位置，结果是
            // 「队列面板高亮错曲目 + 下一首按旧队列跳」。
            //
            // 这里把契约收紧成一条：**只要在播，队列里就一定有它，且 cursor 指向它**。
            // 不在队列里就把队列重置成这一首（单曲播放的语义）。
            match queue.items().iter().position(|t| t.id == track.id) {
                Some(i) => {
                    queue.jump_to(i);
                }
                None => queue.set_items(vec![track.clone()], Some(0)),
            }
            broadcast_queue(queue, shared, app);
            start_track(track, session, out, shared, app, platform, start_ms);
        }

        EngineCommand::Pause => {
            if let (Some(s), Some(out)) = (session.as_mut(), output.as_mut()) {
                let _ = s.set_paused(true, out);
                shared.set_status(PlaybackStatus::Paused);
                platform.update_playback_state(false);
            }
        }
        EngineCommand::Resume => {
            if let (Some(s), Some(out)) = (session.as_mut(), output.as_mut()) {
                let _ = s.set_paused(false, out);
                shared.set_status(PlaybackStatus::Playing);
                platform.update_playback_state(true);
            } else if let Some(idx) = queue.current_index() {
                play_index(idx, queue, session, output, shared, app, platform);
            }
        }
        EngineCommand::Toggle => {
            if shared.status() == PlaybackStatus::Playing {
                if let (Some(s), Some(out)) = (session.as_mut(), output.as_mut()) {
                    let _ = s.set_paused(true, out);
                }
                shared.set_status(PlaybackStatus::Paused);
                platform.update_playback_state(false);
            } else if let (Some(s), Some(out)) = (session.as_mut(), output.as_mut()) {
                let _ = s.set_paused(false, out);
                shared.set_status(PlaybackStatus::Playing);
                platform.update_playback_state(true);
            } else if let Some(idx) = queue.current_index() {
                play_index(idx, queue, session, output, shared, app, platform);
            }
        }
        EngineCommand::Stop => {
            *session = None;
            shared.source_rate.store(0, Ordering::Relaxed);
            if let Some(out) = output.as_ref() {
                out.shared.reset_transport();
            }
            shared.set_status(PlaybackStatus::Stopped);
            shared.position_ms.store(0, Ordering::Relaxed);
            platform.update_playback_state(false);
        }
        EngineCommand::Seek(ms) => {
            if let (Some(s), Some(out)) = (session.as_mut(), output.as_mut()) {
                if let Err(e) = s.seek(ms, out) {
                    tracing::warn!("seek failed: {e}");
                } else {
                    shared.position_ms.store(ms, Ordering::Relaxed);
                }
            } else {
                // 还没开播（典型：启动恢复态）——没有可 seek 的解码器，
                // 更新显示位置；若是恢复的那首，记忆位置一并改写。
                shared.position_ms.store(ms, Ordering::Relaxed);
                let tid = shared.track.read().as_ref().map(|t| t.id.clone());
                if let Some(id) = tid {
                    if shared.peek_resume_for(&id).is_some() {
                        shared.set_resume(id, ms);
                    }
                }
                shared.bump();
            }
        }
        EngineCommand::SetVolume(v) => {
            shared.set_volume(v);
            if let Some(out) = output.as_ref() {
                out.shared.set_volume(v);
            }
        }
        EngineCommand::SetSpeed(v) => shared.set_speed(v),
        EngineCommand::SetPitch(v) => shared.set_pitch(v),
        EngineCommand::SetGain(v) => shared.set_gain(v),
        EngineCommand::SetRepeat(mode) => {
            queue.set_repeat(mode);
            // 归一化可能顺带关掉 shuffle，两个 atomic 都必须跟着 queue 走，
            // 否则前端仍显示「单曲循环」而引擎已经在随机播放。
            let (shuffle, repeat) = queue.mode();
            shared.repeat.store(repeat as u8, Ordering::Relaxed);
            shared.shuffle.store(shuffle, Ordering::Relaxed);
            broadcast_queue(queue, shared, app);
        }
        EngineCommand::SetShuffle(on) => {
            queue.set_shuffle(on);
            // 归一化可能顺带把 repeat=One 复位为 Off
            let (shuffle, repeat) = queue.mode();
            shared.repeat.store(repeat as u8, Ordering::Relaxed);
            shared.shuffle.store(shuffle, Ordering::Relaxed);
            broadcast_queue(queue, shared, app);
        }
        EngineCommand::Duck(on) => {
            shared.set_duck(on);
            if let Some(out) = output.as_mut() {
                out.shared.set_duck(on);
            }
        }
        EngineCommand::SetQueue { tracks, index } => {
            *session = None;
            queue.set_items(tracks, index);
            broadcast_queue(queue, shared, app);
            match index {
                Some(idx) => {
                    play_index(idx, queue, session, output, shared, app, platform);
                }
                None => {
                    *shared.track.write() = None;
                    shared.source_rate.store(0, Ordering::Relaxed);
                    shared.set_status(PlaybackStatus::Stopped);
                }
            }
        }
        EngineCommand::QueueAdd { tracks, play_next } => {
            let had_current = queue.current_index().is_some();
            queue.push(tracks, play_next);
            broadcast_queue(queue, shared, app);
            if !had_current {
                if let Some(idx) = queue.current_index() {
                    play_index(idx, queue, session, output, shared, app, platform);
                }
            }
        }
        EngineCommand::QueueRemove(i) => {
            queue.remove(i);
            broadcast_queue(queue, shared, app);
        }
        EngineCommand::QueueMove { from, to } => {
            queue.move_item(from, to);
            broadcast_queue(queue, shared, app);
        }
        EngineCommand::QueueClear => {
            queue.clear();
            *session = None;
            *shared.track.write() = None;
            shared.source_rate.store(0, Ordering::Relaxed);
            shared.set_status(PlaybackStatus::Stopped);
            shared.position_ms.store(0, Ordering::Relaxed);
            broadcast_queue(queue, shared, app);
            platform.update_playback_state(false);
        }
        EngineCommand::UpdateTrackPath { track_id, path } => {
            // 流式落盘完成：只回填内存（DB 已由 finalize 线程更新），
            // 不广播——file_path 不影响前端展示。
            queue.update_file_path(&track_id, &path);
            let mut cur = shared.track.write();
            if cur.as_ref().is_some_and(|t| t.id == track_id) {
                if let Some(t) = cur.as_mut() {
                    t.file_path = Some(path);
                }
            }
        }
        EngineCommand::Next => {
            if let Some(idx) = queue.next(false) {
                // cursor 已经挪了，必须广播，否则前端队列面板停在上一行、
                // `player_queue` 返回的 current 与实际播放的曲目对不上。
                broadcast_queue(queue, shared, app);
                play_index(idx, queue, session, output, shared, app, platform);
            }
        }
        EngineCommand::Prev => {
            if let Some(idx) = queue.prev() {
                broadcast_queue(queue, shared, app);
                play_index(idx, queue, session, output, shared, app, platform);
            }
        }
        EngineCommand::AudioDiag(reply) => {
            let mut d = crate::models::AudioDiag {
                volume: shared.volume(),
                ducked: shared.is_ducked(),
                ..Default::default()
            };
            if let Some(out) = output.as_ref() {
                d.device = out.device_name.clone();
                d.sample_rate = out.config.sample_rate;
                d.channels = out.config.channels;
                d.played_frames = out.shared.played_frames.load(Ordering::Relaxed);
                d.underruns = out.shared.underruns.load(Ordering::Relaxed);
                d.buffered_samples = out.capacity.saturating_sub(out.producer.slots());
                d.device_error = out.shared.device_error.lock().clone();
            }
            let _ = reply.send(d);
        }
        EngineCommand::ResetOutput => {
            let pos = shared.position_ms();
            let was_playing = shared.status() == PlaybackStatus::Playing;
            // drop 旧的 cpal::Stream，下一次 ensure_output 会重新取系统默认设备
            *output = None;
            if was_playing {
                if let Some(idx) = queue.current_index() {
                    match ensure_output(output, shared) {
                        Ok(out) => {
                            // 从刚才的位置续播；新 pipeline 的 played_frames 从 0 开始，
                            // start_track 会重新建立锚点，不会让进度条倒退。
                            start_index(idx, queue, session, out, shared, app, platform, pos);
                        }
                        Err(e) => {
                            shared.set_error(format!("重新连接音频设备失败：{e}"));
                            shared.set_status(PlaybackStatus::Error);
                        }
                    }
                }
            }
            shared.bump();
        }
        EngineCommand::Shutdown => {}
    }
}

fn broadcast_queue(queue: &PlayQueue, shared: &SharedState, app: &AppHandle) {
    let snap = queue.snapshot();
    *shared.queue.write() = snap.clone();
    shared.revision.fetch_add(1, Ordering::AcqRel);
    let _ = app.emit(events::EVT_QUEUE, &snap);
    let _ = app.emit(events::EVT_PLAYER_STATE, &shared.snapshot());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归：ring 装不下的样本必须攒进 pending，而不是被 break 丢掉。
    /// 触发场景是 FLAC 4096 帧包 + 44.1k→48k 重采样后超出 slots 预估。
    #[test]
    fn overflow_is_parked_not_dropped() {
        let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(8);
        let mut pending = Vec::new();
        let input: Vec<f32> = (0..10).map(|i| i as f32).collect();

        push_or_park(&mut producer, &input, &mut pending);

        assert_eq!(pending, vec![8.0, 9.0], "溢出样本被丢掉了");
        let mut ring = Vec::new();
        while let Ok(v) = consumer.pop() {
            ring.push(v);
        }
        assert_eq!(ring, vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
    }

    /// ring 有余量时不该产生 pending。
    #[test]
    fn fits_without_parking() {
        let (mut producer, _consumer) = rtrb::RingBuffer::<f32>::new(16);
        let mut pending = Vec::new();
        push_or_park(&mut producer, &[1.0, 2.0, 3.0], &mut pending);
        assert!(pending.is_empty());
    }

    use std::sync::atomic::{AtomicUsize, Ordering as MemOrdering};

    struct FakeOutput {
        suspended: AtomicBool,
        suspend_calls: Arc<AtomicUsize>,
        resume_calls: Arc<AtomicUsize>,
    }

    impl crate::audio::sink::AudioOutput for FakeOutput {
        fn suspend(&self) -> AppResult<()> {
            self.suspended.store(true, MemOrdering::SeqCst);
            self.suspend_calls.fetch_add(1, MemOrdering::SeqCst);
            Ok(())
        }
        fn resume(&self) -> AppResult<()> {
            self.suspended.store(false, MemOrdering::SeqCst);
            self.resume_calls.fetch_add(1, MemOrdering::SeqCst);
            Ok(())
        }
        fn is_suspended(&self) -> bool {
            self.suspended.load(MemOrdering::SeqCst)
        }
    }

    fn fake_pipeline(suspended: bool) -> (AudioPipeline, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let (producer, _consumer) = rtrb::RingBuffer::<f32>::new(64);
        let suspend_calls = Arc::new(AtomicUsize::new(0));
        let resume_calls = Arc::new(AtomicUsize::new(0));
        let pipeline = AudioPipeline {
            output: Box::new(FakeOutput {
                suspended: AtomicBool::new(suspended),
                suspend_calls: suspend_calls.clone(),
                resume_calls: resume_calls.clone(),
            }),
            config: crate::audio::sink::OutputConfig {
                sample_rate: 48_000,
                channels: 2,
            },
            producer,
            shared: Arc::new(crate::audio::sink::OutputShared::new(0.85)),
            capacity: 64,
            device_name: "fake".into(),
        };
        (pipeline, suspend_calls, resume_calls)
    }

    /// 回归「有进度没声音」：上一首暂停挂起了设备，切歌后新 Session 是
    /// paused=false，开播 / 直接点播放都必须把设备拉回运行态，且不能重复动 stream。
    #[test]
    fn align_output_wakes_suspended_device() {
        let (p, suspends, resumes) = fake_pipeline(true);
        align_output(false, &p).unwrap();
        assert_eq!(resumes.load(MemOrdering::SeqCst), 1);
        assert_eq!(suspends.load(MemOrdering::SeqCst), 0);
        assert!(!p.output.is_suspended());
        // 幂等：设备已在跑，再调一次不该重复 play()
        align_output(false, &p).unwrap();
        assert_eq!(resumes.load(MemOrdering::SeqCst), 1);
    }

    /// 正常暂停 → 播放：只在状态真的变化时才碰设备，避免每 8ms 反复 pause/play。
    #[test]
    fn align_output_pause_resume_is_idempotent() {
        let (p, suspends, resumes) = fake_pipeline(false);
        align_output(false, &p).unwrap();
        assert_eq!(suspends.load(MemOrdering::SeqCst), 0);
        assert_eq!(resumes.load(MemOrdering::SeqCst), 0);
        align_output(true, &p).unwrap();
        align_output(true, &p).unwrap();
        assert_eq!(suspends.load(MemOrdering::SeqCst), 1);
        align_output(false, &p).unwrap();
        assert_eq!(resumes.load(MemOrdering::SeqCst), 1);
    }
}
