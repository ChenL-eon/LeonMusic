//! 解码层：把「文件 / 网络缓存文件」解成统一的交错 f32 PCM。
//!
//! * MP3 / FLAC / OGG-Vorbis / WAV / M4A(AAC) / ALAC → symphonia
//! * OGG-Opus (`.opus`) → opus-pure（symphonia 0.5 不提供 Opus 解码）
//!
//! 上层（engine）只看到 `DecodeSource`，不关心容器与编码格式。
//!
//! `allow(dead_code)` 的原因：这里有一批**诊断与探测**接口
//! （`sniff_container` / `format_name` / `channels`），播放主链路用不到，
//! 但排查「为什么这首放不出来」时必须靠它们，删掉反而要重写。

#![allow(dead_code)]

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

use crate::error::{AppError, AppResult};

/// 一次解码输出的 PCM 块。
pub struct DecodedChunk {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: usize,
}

pub enum DecodeSource {
    Symphonia(SymphoniaSource),
    Opus(OpusSource),
}

impl DecodeSource {
    pub fn open(path: &Path) -> AppResult<Self> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        if ext == "opus" {
            return Ok(Self::Opus(OpusSource::open(path)?));
        }
        Ok(Self::Symphonia(SymphoniaSource::open(path)?))
    }

    /// 流式打开（HTTP 边下边播）。Opus 在线流不走这条（容器罕见，
    /// probe 失败会报可读错误）。
    pub fn open_stream(stream: crate::net::stream::HttpStream, url: &str) -> AppResult<Self> {
        Ok(Self::Symphonia(SymphoniaSource::open_stream(stream, url)?))
    }

    pub fn next_chunk(&mut self, max_frames: usize) -> AppResult<Option<DecodedChunk>> {
        match self {
            Self::Symphonia(s) => s.next_chunk(max_frames),
            Self::Opus(s) => s.next_chunk(max_frames),
        }
    }

    pub fn seek(&mut self, position_ms: u64) -> AppResult<()> {
        match self {
            Self::Symphonia(s) => s.seek(position_ms),
            Self::Opus(s) => s.seek(position_ms),
        }
    }

    pub fn position_ms(&self) -> u64 {
        match self {
            Self::Symphonia(s) => s.position_ms(),
            Self::Opus(s) => s.position_ms(),
        }
    }

    pub fn duration_ms(&self) -> u64 {
        match self {
            Self::Symphonia(s) => s.duration_ms,
            Self::Opus(s) => s.duration_ms,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        match self {
            Self::Symphonia(s) => s.sample_rate,
            Self::Opus(s) => s.sample_rate,
        }
    }

    pub fn channels(&self) -> usize {
        match self {
            Self::Symphonia(s) => s.channels,
            Self::Opus(s) => s.channels,
        }
    }

    pub fn format_name(&self) -> &'static str {
        match self {
            Self::Symphonia(s) => s.format_name,
            Self::Opus(_) => "opus",
        }
    }
}

// ---------------------------------------------------------------------------
// symphonia
// ---------------------------------------------------------------------------

pub struct SymphoniaSource {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    sample_rate: u32,
    channels: usize,
    duration_ms: u64,
    format_name: &'static str,
    /// 已输出的帧数（不含 seek 之前的丢弃）
    frames_out: u64,
    /// seek 基点在文件中的毫秒位置
    base_ms: u64,
    eof: bool,
    sample_buf: Option<SampleBuffer<f32>>,
    /// 上一个解码包里超出 `max_frames` 的样本。
    ///
    /// FLAC 固定 4096 帧/包，是 CHUNK_FRAMES(2048) 的两倍：不缓存的话
    /// `next_chunk` 会真的返回 4096 帧，调用方按 2048 帧预估的 ring 空间
    /// 装不下，剩下的被静默丢掉 —— 听感就是持续断续加颤音。
    pending: Vec<f32>,
}

/// probe 失败时的取证信息：流式音源的开头字节与响应 Content-Type。
/// 本地文件走 `Default`（两个字段都空），note() 返回空串不污染错误。
#[derive(Default)]
struct SourceDiag {
    /// 是否来自 HTTP 流（本地文件为 false，错误消息不带来源说明）
    remote: bool,
    head: Vec<u8>,
    content_type: Option<String>,
}

impl SourceDiag {
    /// 开头像网页 / XML / JSON：这类响应绝不是音频，直接给结论，
    /// 不再让 symphonia 报「no suitable format reader found」。
    fn non_audio_kind(&self) -> Option<&'static str> {
        let text = String::from_utf8_lossy(&self.head);
        let trimmed = text.trim_start_matches('\u{feff}').trim_start();
        let head = trimmed.get(..16).unwrap_or(trimmed).to_ascii_lowercase();
        if head.starts_with("<!doctype")
            || head.starts_with("<html")
            || head.starts_with("<head")
            || head.starts_with("<?xml")
        {
            Some("网页/XML")
        } else if head.starts_with('{') || head.starts_with('[') {
            Some("JSON")
        } else {
            None
        }
    }

    /// 响应正文开头的可读片段（换行压成空格）。
    fn snippet(&self) -> String {
        let text = String::from_utf8_lossy(&self.head);
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        flat.chars().take(56).collect()
    }

    /// 附在 probe 错误后面的技术细节。
    fn note(&self) -> String {
        let ct = self.content_type.as_deref();
        if self.remote {
            if self.head.is_empty() {
                return format!(
                    "（来源没有返回任何数据，Content-Type: {}）",
                    ct.unwrap_or("无")
                );
            }
            return format!(
                "（Content-Type: {}，开头字节 {}）",
                ct.unwrap_or("无"),
                hex_head(&self.head)
            );
        }
        // 本地文件：加密格式（ncm / mflac）、改过后缀、空文件都落在这里
        if self.head.is_empty() {
            return "（文件是空的：0 字节）".to_string();
        }
        format!("（文件开头字节 {}）", hex_head(&self.head))
    }
}

fn hex_head(head: &[u8]) -> String {
    head.iter()
        .take(10)
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

impl SymphoniaSource {
    pub fn open(path: &Path) -> AppResult<Self> {
        let mut file = File::open(path)?;
        // 留一份开头字节：probe 失败时（加密格式 / 改过后缀 / 空文件）能直接
        // 看出文件真实长相，不用再让用户把文件发过来
        let mut head = vec![0u8; 96];
        let n = file.read(&mut head)?;
        head.truncate(n);
        let _ = file.seek(SeekFrom::Start(0));
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        Self::open_mss(mss, &ext, &SourceDiag { head, ..Default::default() })
    }

    /// 流式打开：HTTP 流（边下边播）+ URL 后缀作容器 Hint。
    pub fn open_stream(stream: crate::net::stream::HttpStream, url: &str) -> AppResult<Self> {
        // 先取开头字节与 Content-Type：链接失效时服务端常回 200 + 登录页/JSON，
        // symphonia 只会给一句「找不到格式」，看不出到底谁的问题。
        let diag = SourceDiag {
            remote: true,
            head: stream.sniff(96, std::time::Duration::from_millis(1200)),
            content_type: stream.content_type(),
        };
        if let Some(kind) = diag.non_audio_kind() {
            return Err(AppError::decode(format!(
                "音源返回的不是音频（{kind}，Content-Type: {}）：「{}」——链接可能已失效或需要登录",
                diag.content_type.as_deref().unwrap_or("无"),
                diag.snippet()
            )));
        }
        let mss = MediaSourceStream::new(Box::new(stream), Default::default());
        let lower = url.to_ascii_lowercase();
        let path = lower.split('?').next().unwrap_or("");
        let ext = path.rsplit('.').next().unwrap_or("");
        // B 站等 CDN 的 URL 常无扩展名（hash 路径），hint 留空让 probe 全试
        let ext = if ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
            ext
        } else {
            ""
        };
        Self::open_mss(mss, ext, &diag)
    }

    fn open_mss(mss: MediaSourceStream, ext: &str, diag: &SourceDiag) -> AppResult<Self> {
        let mut hint = Hint::new();
        if !ext.is_empty() {
            hint.with_extension(ext);
        }

        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions {
                    enable_gapless: true,
                    ..Default::default()
                },
                &MetadataOptions::default(),
            )
            .map_err(|e| AppError::decode(format!("probe failed: {e}{}", diag.note())))?;

        let format = probed.format;
        let track = format
            .default_track()
            .ok_or_else(|| AppError::decode("no default audio track"))?;

        let track_id = track.id;
        let codec_params = track.codec_params.clone();
        let sample_rate = codec_params.sample_rate.unwrap_or(44_100);
        let channels = codec_params.channels.map(|c| c.count()).unwrap_or(2);
        let duration_ms = codec_params
            .n_frames
            .map(|n| (n as f64 / sample_rate as f64 * 1000.0) as u64)
            .unwrap_or(0);

        let decoder = symphonia::default::get_codecs()
            .make(&codec_params, &DecoderOptions::default())
            .map_err(|e| AppError::decode(format!("codec unsupported: {e}")))?;

        let format_name: &'static str = match ext {
            "mp3" => "mp3",
            "flac" => "flac",
            "ogg" => "ogg",
            "wav" => "wav",
            "m4a" | "mp4" | "aac" => "aac",
            "alac" => "alac",
            "mka" | "mkv" => "mkv",
            _ => "audio",
        };

        Ok(Self {
            format,
            decoder,
            track_id,
            sample_rate,
            channels,
            duration_ms,
            format_name,
            frames_out: 0,
            base_ms: 0,
            eof: false,
            sample_buf: None,
            pending: Vec::new(),
        })
    }

    fn next_chunk(&mut self, max_frames: usize) -> AppResult<Option<DecodedChunk>> {
        if self.eof && self.pending.is_empty() {
            return Ok(None);
        }
        let want = max_frames * self.channels;
        let mut out: Vec<f32> = Vec::with_capacity(want);

        // 上一包解出来但没交出去的部分先补上，保证每次交付 ≤ max_frames
        if !self.pending.is_empty() {
            let take = want.min(self.pending.len());
            out.extend(self.pending.drain(..take));
        }

        while out.len() < want && !self.eof {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymError::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    self.eof = true;
                    break;
                }
                Err(SymError::ResetRequired) => {
                    // 流参数变化，重新构建解码器
                    self.eof = true;
                    break;
                }
                Err(e) => return Err(AppError::decode(e)),
            };

            if packet.track_id() != self.track_id {
                continue;
            }

            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    let spec = *decoded.spec();
                    let ch = spec.channels.count();
                    if self.sample_rate != spec.rate || self.channels != ch {
                        self.sample_rate = spec.rate;
                        self.channels = ch;
                    }
                    let capacity = decoded.capacity() as u64;
                    let frames = decoded.frames() as u64;
                    // 分字段借用：sample_buf 与 pending 同时可变借，不能走 self
                    let buf = self
                        .sample_buf
                        .get_or_insert_with(|| SampleBuffer::<f32>::new(capacity, spec));
                    if buf.capacity() < decoded.frames() {
                        *buf = SampleBuffer::<f32>::new(capacity, spec);
                    }
                    buf.copy_interleaved_ref(decoded);
                    let room = want - out.len();
                    {
                        let samples = buf.samples();
                        if samples.len() <= room {
                            out.extend_from_slice(samples);
                        } else {
                            out.extend_from_slice(&samples[..room]);
                            self.pending.extend_from_slice(&samples[room..]);
                        }
                    }
                    self.frames_out += frames;
                }
                // 单帧损坏不应中断整首播放
                Err(SymError::DecodeError(_)) => continue,
                Err(e) => return Err(AppError::decode(e)),
            }
        }

        if out.is_empty() {
            return Ok(None);
        }
        Ok(Some(DecodedChunk {
            samples: out,
            sample_rate: self.sample_rate,
            channels: self.channels,
        }))
    }

    fn seek(&mut self, position_ms: u64) -> AppResult<()> {
        let time = Time::from(Duration::from_millis(position_ms));
        self.format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: Some(self.track_id),
                },
            )
            .map_err(|e| AppError::decode(format!("seek failed: {e}")))?;
        self.decoder.reset();
        self.frames_out = 0;
        self.base_ms = position_ms;
        self.eof = false;
        // seek 前的残留样本属于旧位置，必须丢掉
        self.pending.clear();
        Ok(())
    }

    fn position_ms(&self) -> u64 {
        self.base_ms + (self.frames_out as f64 / self.sample_rate as f64 * 1000.0) as u64
    }
}

// ---------------------------------------------------------------------------
// Ogg Opus
// ---------------------------------------------------------------------------

pub struct OpusSource {
    reader: opus_pure::OggOpusReader<BufReader<File>>,
    decoder: opus_pure::OpusDecoder,
    sample_rate: u32,
    channels: usize,
    duration_ms: u64,
    /// Opus 解码固定输出 48kHz；pre-skip 需要丢弃
    pre_skip: u32,
    skipped: u32,
    frames_out: u64,
    base_ms: u64,
    path: std::path::PathBuf,
    eof: bool,
    pcm: Vec<f32>,
    packet: opus_pure::OggPacket,
}

impl OpusSource {
    pub fn open(path: &Path) -> AppResult<Self> {
        let file = File::open(path)?;
        let reader = opus_pure::OggOpusReader::new(BufReader::new(file))
            .map_err(|e| AppError::decode(format!("ogg opus open: {e}")))?;

        let head = reader.head();
        let channels = head.channel_count.max(1) as usize;
        let pre_skip = head.pre_skip as u32;
        // Opus 内部总是 48kHz；这里直接按 48k 解码，交给上层重采样
        let sample_rate = 48_000u32;

        let decoder = head
            .decoder(sample_rate as i32)
            .map_err(|e| AppError::decode(format!("opus decoder: {e}")))?;

        // 时长：Opus granule = 48kHz 样本数。没有索引时通过文件大小估算不可靠，
        // 这里先置 0，由上层用 lofty 的元数据补齐（见 library::metadata）。
        Ok(Self {
            reader,
            decoder,
            sample_rate,
            channels,
            duration_ms: 0,
            pre_skip,
            skipped: 0,
            frames_out: 0,
            base_ms: 0,
            path: path.to_path_buf(),
            eof: false,
            pcm: vec![0.0; 5760 * channels],
            packet: opus_pure::OggPacket::default(),
        })
    }

    fn next_chunk(&mut self, max_frames: usize) -> AppResult<Option<DecodedChunk>> {
        if self.eof {
            return Ok(None);
        }
        let mut out: Vec<f32> = Vec::with_capacity(max_frames * self.channels);

        while out.len() / self.channels < max_frames {
            let has = self
                .reader
                .read_packet_into(&mut self.packet)
                .map_err(|e| AppError::decode(format!("ogg read: {e}")))?;
            if !has {
                self.eof = true;
                break;
            }
            if self.packet.data.is_empty() {
                continue;
            }

            let decoded = self
                .decoder
                .decode(&self.packet.data, 5760, &mut self.pcm)
                .map_err(|e| AppError::decode(format!("opus decode: {e}")))?;
            if decoded == 0 {
                continue;
            }

            let mut start = 0usize;
            if self.skipped < self.pre_skip {
                let remain = (self.pre_skip - self.skipped) as usize;
                start = remain.min(decoded);
                self.skipped += start as u32;
            }
            if start >= decoded {
                continue;
            }

            out.extend_from_slice(&self.pcm[start * self.channels..decoded * self.channels]);
            self.frames_out += (decoded - start) as u64;
        }

        if out.is_empty() {
            return Ok(None);
        }
        Ok(Some(DecodedChunk {
            samples: out,
            sample_rate: self.sample_rate,
            channels: self.channels,
        }))
    }

    /// Ogg Opus 没有随机访问索引，这里退化为「回到开头 + 解码丢弃」。
    /// 对常见 3~8 分钟曲目，seek 延迟在几十毫秒量级，可接受。
    fn seek(&mut self, position_ms: u64) -> AppResult<()> {
        self.reader
            .rewind()
            .map_err(|e| AppError::decode(format!("ogg rewind: {e}")))?;
        self.decoder
            .reset_state()
            .map_err(|e| AppError::decode(format!("opus reset: {e}")))?;
        self.eof = false;
        self.skipped = 0;
        self.frames_out = 0;
        self.base_ms = 0;

        let mut remaining = position_ms.saturating_sub(self.pre_skip as u64 / 48);
        while remaining > 0 {
            let has = self
                .reader
                .read_packet_into(&mut self.packet)
                .map_err(|e| AppError::decode(format!("ogg read: {e}")))?;
            if !has {
                break;
            }
            if self.packet.data.is_empty() {
                continue;
            }
            let decoded = self
                .decoder
                .decode(&self.packet.data, 5760, &mut self.pcm)
                .map_err(|e| AppError::decode(format!("opus decode: {e}")))?;
            if decoded == 0 {
                continue;
            }
            let ms = (decoded as u64 * 1000) / 48_000;
            self.frames_out += decoded as u64;
            if ms >= remaining {
                // 多解出来的部分留在 base 里，位置计算会略超前，误差 < 一帧
                let overshoot = ms - remaining;
                self.frames_out = self.frames_out.saturating_sub(overshoot * 48 / 1000);
                break;
            }
            remaining -= ms;
        }
        self.frames_out = 0;
        self.base_ms = position_ms;
        Ok(())
    }

    fn position_ms(&self) -> u64 {
        self.base_ms + (self.frames_out as f64 / 48_000.0 * 1000.0) as u64
    }

    /// 补上时长（由扫描阶段从 lofty 读到的结果回填）。
    pub fn with_duration(mut self, duration_ms: u64) -> Self {
        if self.duration_ms == 0 {
            self.duration_ms = duration_ms;
        }
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// 探测文件容器是否是我们支持的格式（扫描阶段用，不做完整解码）。
pub fn is_supported_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "mp3" | "flac" | "ogg" | "oga" | "opus" | "wav" | "m4a" | "mp4" | "aac" | "alac" | "mka"
    )
}

/// 读取文件头部若干字节，用于 sniff 真实容器（防止扩展名撒谎）。
pub fn sniff_container(path: &Path) -> AppResult<String> {
    let mut f = File::open(path)?;
    let mut head = [0u8; 12];
    let n = f.read(&mut head)?;
    let _ = f.seek(SeekFrom::Start(0));
    if n < 4 {
        return Ok("unknown".into());
    }
    let s = &head[..n];
    if s.starts_with(b"ID3") {
        return Ok("mp3".into());
    }
    if s.starts_with(b"fLaC") {
        return Ok("flac".into());
    }
    if s.starts_with(b"OggS") {
        // Ogg 里可能是 Vorbis 或 Opus，检查前 64 字节
        let mut buf = [0u8; 64];
        let mut f = File::open(path)?;
        let n = f.read(&mut buf)?;
        if buf[..n].windows(5).any(|w| w == b"OpusH") {
            return Ok("opus".into());
        }
        return Ok("ogg".into());
    }
    if s.starts_with(b"RIFF") {
        return Ok("wav".into());
    }
    if &s[4..8] == b"ftyp" {
        return Ok("m4a".into());
    }
    Ok("unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本地加密缓存（网易云 ncm 开头是 `CTENFDAM`）或改过后缀的文件：
    /// 错误里必须带文件真实开头，否则只有一句 `no suitable format reader found`。
    #[test]
    fn local_non_audio_file_reports_head_bytes() {
        let p = std::env::temp_dir().join(format!("lm-bad-{}.mp3", std::process::id()));
        std::fs::write(&p, b"CTENFDAM\x00\x01\x02not a real mp3").unwrap();
        let err = match DecodeSource::open(&p) {
            Ok(_) => panic!("encrypted header must not probe successfully"),
            Err(e) => e.detail().to_string(),
        };
        let _ = std::fs::remove_file(&p);
        println!("[local-bad] {err}");
        assert!(err.contains("probe failed"), "{err}");
        assert!(
            err.contains("文件开头字节 43 54 45 4e 46 44 41 4d"),
            "{err}"
        );
    }

    /// 0 字节文件要明确说「空」，别让用户以为是格式问题
    #[test]
    fn local_empty_file_is_reported_as_empty() {
        let p = std::env::temp_dir().join(format!("lm-empty-{}.flac", std::process::id()));
        std::fs::write(&p, b"").unwrap();
        let err = match DecodeSource::open(&p) {
            Ok(_) => panic!("empty file must not probe successfully"),
            Err(e) => e.detail().to_string(),
        };
        let _ = std::fs::remove_file(&p);
        println!("[local-empty] {err}");
        assert!(err.contains("0 字节"), "{err}");
    }
}
