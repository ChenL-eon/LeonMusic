//! 音频输出抽象。
//!
//! 平台差异只在「如何开一个输出流」这一层，DSP / 解码 / 状态机完全共用。
//!
//! ```text
//!   ┌──────────────┐   ring buffer   ┌──────────────────┐
//!   │ 解码线程      │ ──────────────▶ │ 音频回调线程      │
//!   │ decode+DSP   │  rtrb (SPSC)    │ pop + 音量 → 设备 │
//!   └──────────────┘                 └──────────────────┘
//! ```
//!
//! 回调线程**不做任何分配、不加锁、不碰文件**，只做内存搬运与乘法，
//! 满足实时音频约束。

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use rtrb::{Consumer, Producer};

use crate::error::AppResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputConfig {
    pub sample_rate: u32,
    pub channels: u16,
}

/// 平台输出流的统一接口。
///
/// 具体实现持有操作系统的 stream 句柄（cpal::Stream / AAudio builder），
/// 一旦 drop 就停止发声。
pub trait AudioOutput {
    /// 暂停设备侧播放（省电，避免空转回调）。队列与进度保持不变。
    fn suspend(&self) -> AppResult<()>;
    fn resume(&self) -> AppResult<()>;
    /// 给上层判断「设备是否已经挂起」。切歌 / 恢复播放时按它对齐状态，
    /// 否则会出现「状态在播、设备却没跑」的静音。
    fn is_suspended(&self) -> bool;
}

/// 引擎与输出之间共享的实时参数与统计。
pub struct OutputShared {
    /// 音量的 f32 位模式，回调线程直接读，避免锁
    pub volume_bits: AtomicU32,
    /// 已真正送进设备的帧数（用于计算播放位置）
    pub played_frames: AtomicU64,
    /// 请求清空 ring buffer（seek / 切歌）
    pub flush: AtomicBool,
    /// 欠载次数（ring 空导致补零）
    pub underruns: AtomicU64,
    /// 临时压低音量（导航播报 / 通知提示音），不改用户的 volume 设定
    pub duck: AtomicBool,
    /// 回调里检测到的设备错误
    pub device_error: parking_lot::Mutex<Option<String>>,
}

/// duck 时保留的音量比例。0.2 ≈ -14 dB，能听清但明显让位。
const DUCK_GAIN: f32 = 0.2;

impl OutputShared {
    pub fn new(volume: f32) -> Self {
        Self {
            volume_bits: AtomicU32::new(volume.to_bits()),
            played_frames: AtomicU64::new(0),
            flush: AtomicBool::new(false),
            underruns: AtomicU64::new(0),
            duck: AtomicBool::new(false),
            device_error: parking_lot::Mutex::new(None),
        }
    }

    #[inline]
    pub fn is_ducked(&self) -> bool {
        self.duck.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn set_duck(&self, on: bool) {
        self.duck.store(on, Ordering::Relaxed);
    }

    #[inline]
    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume_bits.load(Ordering::Relaxed))
    }

    #[inline]
    pub fn set_volume(&self, v: f32) {
        self.volume_bits.store(v.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
    }

    pub fn reset_transport(&self) {
        self.played_frames.store(0, Ordering::SeqCst);
        self.flush.store(true, Ordering::SeqCst);
    }
}

pub struct AudioPipeline {
    pub output: Box<dyn AudioOutput>,
    pub config: OutputConfig,
    pub producer: Producer<f32>,
    pub shared: Arc<OutputShared>,
    /// ring buffer 总容量（样本数）。用于判断「缓冲已排空 / 已填满」。
    pub capacity: usize,
    /// 实际打开的输出设备名。诊断「声音去了别的设备」时给设置页看。
    pub device_name: String,
}

/// 建议的 ring buffer 容量：2 秒音频。
pub fn ring_capacity(config: OutputConfig) -> usize {
    (config.sample_rate as usize * config.channels as usize * 2).next_power_of_two()
}

#[cfg(not(target_os = "android"))]
pub fn build_pipeline(preferred_rate: u32, preferred_channels: u16) -> AppResult<AudioPipeline> {
    super::sink_cpal::build(preferred_rate, preferred_channels)
}

#[cfg(target_os = "android")]
pub fn build_pipeline(preferred_rate: u32, preferred_channels: u16) -> AppResult<AudioPipeline> {
    super::sink_android::build(preferred_rate, preferred_channels)
}

/// 回调线程的公共渲染逻辑，各平台实现共用。
///
/// 从 ring buffer 逐帧取出 f32、应用平滑后的音量、转换成设备样本格式。
/// 全程无分配、无锁。
#[inline]
pub(crate) fn render<T>(data: &mut [T], consumer: &mut Consumer<f32>, shared: &OutputShared, channels: usize)
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    // seek / 切歌：把残留样本丢掉，保证新位置从干净状态开始
    if shared.flush.swap(false, Ordering::AcqRel) {
        while consumer.pop().is_ok() {}
    }

    let channels = channels.max(1);
    let frames = data.len() / channels;
    let target = shared.volume();
    // duck 也走同样的单极点平滑，避免导航播报进出时「咔」一声
    let duck_target = if shared.is_ducked() { DUCK_GAIN } else { 1.0 };
    let mut current = target;
    let mut duck_now = duck_target;
    let smooth = 0.004f32;
    let mut misses = 0u64;

    for f in 0..frames {
        // 每帧推进一步音量平滑，避免突变爆音
        current += (target - current) * smooth;
        duck_now += (duck_target - duck_now) * smooth;
        let base = f * channels;
        for c in 0..channels {
            let sample = match consumer.pop() {
                Ok(s) => s,
                Err(_) => {
                    misses += 1;
                    0.0
                }
            };
            data[base + c] = T::from_sample(sample * current * duck_now);
        }
    }

    if misses > 0 {
        shared.underruns.fetch_add(misses, Ordering::Relaxed);
    }
    shared
        .played_frames
        .fetch_add(frames as u64, Ordering::Relaxed);
}
