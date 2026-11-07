//! Android 输出实现。
//!
//! # 为什么还是 cpal
//!
//! cpal 在 `target_os = "android"` 下走 [Oboe] 后端，而 Oboe 在 API 26+
//! 优先使用 **AAudio**（NDK 原生音频 API），低版本回落到 OpenSL ES。
//! 也就是说这条路径本身就是 NDK 音频 API，而不是 Java `AudioTrack`
//! （Java 层会受 WebView/Activity 生命周期影响，这是必须避免的）。
//!
//! 差异化参数放在这里：
//! * 采样率锁定 48kHz —— Android 音频混音器原生 48k，避免系统层二次重采样
//! * 更大的缓冲（80ms）—— 移动设备调度抖动大，欠载比延迟更致命
//! * 低延迟模式关闭 —— 音乐播放不需要 AAudio 的 MMAP 低延迟路径
//!
//! # 如果要换成裸 AAudio
//!
//! 实现 `AudioOutput` + 自己开一个 `AAudioStreamBuilder` 即可，
//! 上层 `AudioPipeline` 的 `producer` / `OutputShared` 协议不变，
//! 只需要把 [`build`] 换成新的构造函数。这个 trait 就是为此预留的。

use super::sink::AudioPipeline;
use crate::error::AppResult;

/// Android 音频回调缓冲：80ms @48k ≈ 3840 帧。
const ANDROID_BUFFER_MS: u32 = 80;

pub fn build(_preferred_rate: u32, _preferred_channels: u16) -> AppResult<AudioPipeline> {
    // Android 上强制 48k / 双声道：系统混音器与 Oboe 都以此为准
    super::sink_cpal::build_with(48_000, 2, ANDROID_BUFFER_MS)
}
