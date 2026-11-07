//! Windows / 桌面输出实现：cpal → WASAPI（共享模式）。
//!
//! cpal 在 Windows 上使用 WASAPI；这里不主动请求独占模式，
//! 避免抢占其他应用的声音（音乐播放器的正确姿势）。
//!
//! # 为什么要「逐个候选尝试」
//!
//! WASAPI 设备会用 `supported_output_configs()` 暴露一大串区间，
//! 而且**不保证默认那一个是可用的**。曾经踩过的坑：按采样率排序后
//! 选中了第一个区间，它的 `sample_format` 是 `U8`，于是整个播放链路
//! 在打开设备时就失败——用户看到的现象只是「点了播放没反应」。
//!
//! 所以这里的策略是：
//! 1. 把设备所有区间按「声道数 → 采样格式 → 采样率」打分排序（F32 优先）；
//! 2. 依次尝试真正 `build_output_stream`，失败就换下一个；
//! 3. 全部失败才报错，并带上最后一次的真实原因。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleFormat, StreamConfig, SupportedStreamConfig};

use super::sink::{render, ring_capacity, AudioOutput, AudioPipeline, OutputConfig, OutputShared};
use crate::error::{AppError, AppResult};

/// 桌面入口。Android 走 `sink_android::build`（同一个 `build_with`，只是参数不同），
/// 所以这里在 Android 目标上不参与调用。
#[cfg_attr(target_os = "android", allow(dead_code))]
pub fn build(preferred_rate: u32, preferred_channels: u16) -> AppResult<AudioPipeline> {
    // 桌面端留 30ms 缓冲，兼顾延迟与抗抖动
    build_with(preferred_rate, preferred_channels, 30)
}

/// `buffer_ms` 是目标回调缓冲时长，Android 会用更大的值。
pub fn build_with(
    preferred_rate: u32,
    preferred_channels: u16,
    buffer_ms: u32,
) -> AppResult<AudioPipeline> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| AppError::audio("no default output device"))?;

    // name() 在 cpal 0.17 已弃用，description() 还能带出厂商信息
    let name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "unknown".into());
    let candidates = candidate_configs(&device, preferred_rate, preferred_channels);

    let mut last_err: Option<AppError> = None;
    for supported in candidates {
        let fmt = supported.sample_format();
        let rate = supported.sample_rate();
        let ch = supported.channels();
        match try_open(&device, supported, buffer_ms) {
            Ok(mut pipeline) => {
                pipeline.device_name = name.clone();
                tracing::info!(
                    "audio output: {name} | {rate} Hz | {ch} ch | {fmt:?} | {} ms buffer",
                    buffer_ms
                );
                return Ok(pipeline);
            }
            Err(e) => {
                tracing::warn!("output config {rate}Hz/{ch}ch/{fmt:?} unusable: {e}");
                last_err = Some(e);
            }
        }
    }

    Err(last_err.unwrap_or_else(|| {
        AppError::audio(format!("no usable output config on device {name}"))
    }))
}

fn try_open(
    device: &cpal::Device,
    supported: SupportedStreamConfig,
    buffer_ms: u32,
) -> AppResult<AudioPipeline> {
    let sample_format = supported.sample_format();
    let mut config: StreamConfig = supported.config();

    // 固定缓冲可以显著降低偶发 underrun；设备不支持时退回默认
    let frames = (config.sample_rate as u64 * buffer_ms as u64 / 1000) as u32;
    config.buffer_size = BufferSize::Fixed(frames.max(128));

    let channels = config.channels as usize;
    let out_cfg = OutputConfig {
        sample_rate: config.sample_rate,
        channels: config.channels,
    };

    let shared = Arc::new(OutputShared::new(0.85));
    let capacity = ring_capacity(out_cfg);
    let (producer, consumer) = rtrb::RingBuffer::<f32>::new(capacity);

    let stream = match sample_format {
        SampleFormat::F32 => build_stream::<f32>(device, &config, consumer, shared.clone(), channels)?,
        SampleFormat::I16 => build_stream::<i16>(device, &config, consumer, shared.clone(), channels)?,
        SampleFormat::U16 => build_stream::<u16>(device, &config, consumer, shared.clone(), channels)?,
        SampleFormat::I32 => build_stream::<i32>(device, &config, consumer, shared.clone(), channels)?,
        SampleFormat::U32 => build_stream::<u32>(device, &config, consumer, shared.clone(), channels)?,
        SampleFormat::I8 => build_stream::<i8>(device, &config, consumer, shared.clone(), channels)?,
        SampleFormat::U8 => build_stream::<u8>(device, &config, consumer, shared.clone(), channels)?,
        other => {
            return Err(AppError::Unsupported(format!(
                "output sample format {other:?} not supported"
            )))
        }
    };

    stream.play().map_err(AppError::audio)?;

    Ok(AudioPipeline {
        output: Box::new(CpalOutput {
            stream,
            suspended: AtomicBool::new(false),
        }),
        config: out_cfg,
        producer,
        shared,
        capacity,
        device_name: String::new(),
    })
}

/// 采样格式偏好：浮点优先，其次有符号整数，U8 这种最差的排最后。
fn format_rank(f: SampleFormat) -> u8 {
    match f {
        SampleFormat::F32 => 0,
        SampleFormat::I32 => 1,
        SampleFormat::I16 => 2,
        SampleFormat::U32 => 3,
        SampleFormat::U16 => 4,
        SampleFormat::I8 => 5,
        SampleFormat::U8 => 6,
        _ => 7,
    }
}

/// 在区间内挑一个离目标最近的采样率。
fn pick_rate(c: &cpal::SupportedStreamConfigRange, preferred: u32) -> u32 {
    let lo = c.min_sample_rate();
    let hi = c.max_sample_rate();
    if (lo..=hi).contains(&preferred) {
        return preferred;
    }
    // 常见采样率里挑一个设备支持的
    for r in [48_000u32, 44_100, 96_000, 32_000] {
        if (lo..=hi).contains(&r) {
            return r;
        }
    }
    hi
}

/// 把设备暴露的所有区间按偏好排序；最后补一个设备默认配置兜底。
fn candidate_configs(
    device: &cpal::Device,
    preferred_rate: u32,
    preferred_channels: u16,
) -> Vec<SupportedStreamConfig> {
    let mut out: Vec<(u8, u8, u8, std::cmp::Reverse<u16>, SupportedStreamConfig)> = Vec::new();

    if let Ok(ranges) = device.supported_output_configs() {
        for c in ranges {
            // 声道：优先能覆盖目标声道的区间（桌面是 2）
            let ch_rank = u8::from(c.channels() < preferred_channels);
            let rate = pick_rate(&c, preferred_rate);
            let rate_rank = match rate {
                r if r == preferred_rate => 0,
                48_000 => 1,
                44_100 => 2,
                _ => 3,
            };
            out.push((
                ch_rank,
                format_rank(c.sample_format()),
                rate_rank,
                std::cmp::Reverse(c.channels()),
                c.with_sample_rate(rate),
            ));
        }
    }

    out.sort_by_key(|(ch, fmt, rate, chans, _)| (*ch, *fmt, *rate, *chans));
    let mut list: Vec<SupportedStreamConfig> = out.into_iter().map(|x| x.4).collect();

    // 设备默认配置作为最后兜底（有些驱动只在这个接口上报真实能力）
    if let Ok(default) = device.default_output_config() {
        let dup = list.iter().any(|c| {
            c.sample_format() == default.sample_format()
                && c.sample_rate() == default.sample_rate()
                && c.channels() == default.channels()
        });
        if !dup {
            list.push(default);
        }
    }

    list
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut consumer: rtrb::Consumer<f32>,
    shared: Arc<OutputShared>,
    channels: usize,
) -> AppResult<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let err_shared = shared.clone();
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _info: &cpal::OutputCallbackInfo| {
                render(data, &mut consumer, &shared, channels);
            },
            move |err| {
                tracing::error!("audio output error: {err}");
                *err_shared.device_error.lock() = Some(err.to_string());
            },
            None,
        )
        .map_err(|e| AppError::audio(format!("build_output_stream: {e}")))
}

struct CpalOutput {
    stream: cpal::Stream,
    suspended: AtomicBool,
}

impl AudioOutput for CpalOutput {
    fn suspend(&self) -> AppResult<()> {
        self.stream.pause().map_err(AppError::audio)?;
        self.suspended.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn resume(&self) -> AppResult<()> {
        self.stream.play().map_err(AppError::audio)?;
        self.suspended.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn is_suspended(&self) -> bool {
        self.suspended.load(Ordering::SeqCst)
    }
}
