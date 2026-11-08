//! 音频子系统。
//!
//! ```text
//! decoder (symphonia / opus-pure)
//!    │  f32 interleaved, 源采样率
//!    ▼
//! dsp (WSOLA 变速变调 + Catmull-Rom 重采样 + 增益)
//!    │  48kHz stereo f32
//!    ▼
//! sink (cpal: WASAPI / Oboe→AAudio)
//! ```

pub mod decoder;
pub mod dsp;
pub mod engine;
pub mod queue;
pub mod resume;
pub mod sink;

// Android 用 cpal 的 Oboe 后端，只是构造参数不同（见 sink_android 的注释）。
// 所以 sink_cpal 是两个平台共用的实现，必须都编译；只有 sink_android 是 Android 专属。
#[cfg(target_os = "android")]
pub mod sink_android;
pub mod sink_cpal;
