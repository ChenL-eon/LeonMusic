//! 跨模块共享的数据模型。
//!
//! 这里是「前端可见的契约」：字段名统一 camelCase 序列化，
//! 任何结构变更都要同步 `src/types/index.ts`。

use serde::{Deserialize, Serialize};

/// 曲目来源。本地文件与插件音源在同一个库里共存。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TrackSource {
    Local,
    Plugin { plugin_id: String },
    Stream,
}

impl TrackSource {
    pub fn plugin_id(&self) -> Option<&str> {
        match self {
            Self::Plugin { plugin_id } => Some(plugin_id),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    /// 形如 `local:<sha1(path)>` / `<plugin>:<trackId>`，全局唯一。
    pub id: String,
    pub source: TrackSource,
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub duration_ms: i64,
    /// 已解析成本地文件路径的封面（由 Rust 生成缩略图后给出）。
    #[serde(default)]
    pub cover_path: Option<String>,
    /// 远程封面（插件音源）。
    #[serde(default)]
    pub cover_url: Option<String>,
    /// 本地文件路径；在线曲目播放前会被填成缓存文件路径。
    #[serde(default)]
    pub file_path: Option<String>,
    /// 运行时字段：解析成功后交给引擎的流式直链，边下边播。
    /// 不入库、不发给前端（`serde(skip)`）；落盘完成后由 file_path 接管。
    #[serde(skip)]
    pub stream_url: Option<String>,
    /// 运行时字段：本次解析实际请求的音质档位。
    ///
    /// 只由「智能音质」降级重试填写，用户设置里存的是插件配置的 `level`。
    /// 要跟着曲目一起穿过 resolve / play 事件，所以不能 `skip`——引擎开播
    /// 失败时正是靠它知道下一次该再降哪一档。不入库。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_hint: Option<String>,
    /// 插件音源的原始 id。
    #[serde(default)]
    pub plugin_track_id: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub bitrate: Option<u32>,
    #[serde(default)]
    pub sample_rate: Option<u32>,
    #[serde(default)]
    pub channels: Option<u16>,
    #[serde(default)]
    pub added_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum RepeatMode {
    Off,
    #[default]
    All,
    One,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PlaybackStatus {
    #[default]
    Idle,
    Buffering,
    Playing,
    Paused,
    Stopped,
    Error,
}

/// 插件实际拿到的音频规格。官方接口会静默降级（请求 lossless 却回 320k MP3），
/// 所以「用户选了什么」和「实际播的是什么」必须分开记录。
/// `track_id` 是宿主填的，用来让快照在切歌后自动丢弃上一首的规格。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamQuality {
    /// official / resolved / trial / plugin
    #[serde(default)]
    pub source: String,
    /// 服务端回传的码率（bps）；解析服务有时只给 kbps，插件会换算
    #[serde(default)]
    pub bitrate: Option<u32>,
    /// mp3 / flac / aac ...
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub trial: bool,
    /// 用户在设置里选的档位
    #[serde(default)]
    pub requested: Option<String>,
    /// 实际规格低于所选档位（逐档降级或服务端静默降级）
    #[serde(default)]
    pub downgraded: bool,
    #[serde(default)]
    pub track_id: String,
}

/// 音质档位，由低到高。设置页的「在线音质」与「智能音质」降级共用这一份顺序。
pub const QUALITY_LADDER: [&str; 6] = ["standard", "higher", "exhigh", "lossless", "hires", "jymaster"];

/// 比 `current` 低一档的档位；已在最低档（或字符串不认识）时返回 `None`。
///
/// 降级重试只往低走：请求无损拿到的是 320k、或直链根本打不开时，
/// 再往上试既没权限也没意义，只会多绕几次网络。
pub fn lower_quality(current: &str) -> Option<&'static str> {
    let i = QUALITY_LADDER.iter().position(|q| *q == current)?;
    if i == 0 {
        None
    } else {
        Some(QUALITY_LADDER[i - 1])
    }
}

/// 引擎每次状态变更时推送的完整快照。
/// 前端只持有一份镜像，永远不自己推断播放状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerSnapshot {
    pub status: PlaybackStatus,
    pub track: Option<Track>,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub volume: f32,
    pub speed: f32,
    pub pitch_semitones: f32,
    pub gain_db: f32,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    pub queue_index: Option<usize>,
    pub queue_len: usize,
    pub output_sample_rate: u32,
    /// 当前曲目的**源文件**采样率（解码时探测，0 = 没有活动会话）。
    /// 与 output_sample_rate 分开记录：引擎固定 48 kHz 输出，44.1k 的
    /// MP3/FLAC 会被重采样，UI 需要两个值都可见才能区分「文件规格」
    /// 和「输出规格」。
    #[serde(default)]
    pub source_sample_rate: u32,
    #[serde(default)]
    pub error: Option<String>,
    /// 在线曲目的真实音质；本地文件、无规格信息的插件为 null。
    #[serde(default)]
    pub quality: Option<StreamQuality>,
    /// 单调递增，前端可用来丢弃乱序事件。
    pub revision: u64,
}

impl Default for PlayerSnapshot {
    fn default() -> Self {
        Self {
            status: PlaybackStatus::Idle,
            track: None,
            position_ms: 0,
            duration_ms: 0,
            volume: 0.85,
            speed: 1.0,
            pitch_semitones: 0.0,
            gain_db: 0.0,
            repeat: RepeatMode::All,
            shuffle: false,
            queue_index: None,
            queue_len: 0,
            output_sample_rate: 0,
            source_sample_rate: 0,
            error: None,
            quality: None,
            revision: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSnapshot {
    pub items: Vec<Track>,
    pub current: Option<usize>,
    pub repeat: RepeatMode,
    pub shuffle: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub cover_path: Option<String>,
    /// 云端歌单导入时记下的网络封面；本地歌单通常只有 cover_path。
    #[serde(default)]
    pub cover_url: Option<String>,
    /// 云端来源（`pluginId:cloudId`），用于「已导入过」检测。
    #[serde(default)]
    pub remote_key: Option<String>,
    pub track_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
    /// 「我喜欢的音乐」是内置歌单，不可删除。
    #[serde(default)]
    pub system: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricWord {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricLine {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// 逐字歌词；普通 LRC 为空数组。
    #[serde(default)]
    pub words: Vec<LyricWord>,
    /// 翻译（同时间轴的第二份 LRC），由前端按行合并展示。
    #[serde(default)]
    pub translation: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LyricKind {
    Plain,
    WordByWord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lyrics {
    pub kind: LyricKind,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub offset_ms: i64,
    pub lines: Vec<LyricLine>,
}

impl Lyrics {
    pub fn empty() -> Self {
        Self {
            kind: LyricKind::Plain,
            title: None,
            artist: None,
            album: None,
            offset_ms: 0,
            lines: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    pub scanned: usize,
    pub added: usize,
    pub updated: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    /// 协议版本，当前只支持 1。
    pub api: u32,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default = "default_main")]
    pub main: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub config: Vec<PluginConfigField>,
}

fn default_main() -> String {
    "index.js".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginConfigField {
    pub key: String,
    pub label: String,
    #[serde(default = "default_field_type")]
    pub field_type: String,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub placeholder: Option<String>,
}

fn default_field_type() -> String {
    "string".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub manifest: PluginManifest,
    pub enabled: bool,
    pub installed: bool,
    /// 内置插件不允许卸载。
    pub builtin: bool,
    #[serde(default)]
    pub config: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlatformKind {
    Windows,
    Android,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformInfo {
    pub kind: PlatformKind,
    pub os_version: String,
    pub arch: String,
    pub supports_lyric_window: bool,
    pub supports_tray: bool,
    pub supports_global_shortcut: bool,
    pub supports_media_session: bool,
    pub cache_dir: String,
}

/// 音频输出自检数据。
///
/// 「进度条在走但没声音」有几种完全不同的成因：应用内静音、输出设备指向了
/// 没插音箱的那一个、设备回调报错、ring buffer 空转。只靠 `PlayerSnapshot`
/// 区分不出来（位置是由 `played_frames` 推导的，补零时照样会涨），所以单独
/// 给设置页一份可判读的诊断。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDiag {
    pub volume: f32,
    pub ducked: bool,
    pub device: String,
    pub sample_rate: u32,
    pub channels: u16,
    /// 设备回调真正消费的帧数；为 0 说明音频流根本没跑起来
    pub played_frames: u64,
    /// ring buffer 欠载次数（补零）
    pub underruns: u64,
    /// 已写入但还没被设备取走的样本数
    pub buffered_samples: usize,
    pub device_error: Option<String>,
    pub hint: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_ladder_steps_down_and_stops_at_standard() {
        // 降级链只往低走，最低档必须返回 None —— 否则引擎会一直重试
        assert_eq!(lower_quality("lossless"), Some("exhigh"));
        assert_eq!(lower_quality("exhigh"), Some("higher"));
        assert_eq!(lower_quality("higher"), Some("standard"));
        assert_eq!(lower_quality("standard"), None);
        assert_eq!(lower_quality("unknown"), None);
    }

    #[test]
    fn track_quality_hint_is_optional_on_the_wire() {
        let t = Track {
            id: "netease:1".into(),
            source: TrackSource::Plugin {
                plugin_id: "netease".into(),
            },
            title: "t".into(),
            artist: String::new(),
            album: String::new(),
            duration_ms: 0,
            cover_path: None,
            cover_url: None,
            file_path: None,
            stream_url: Some("https://example.com/a.flac".into()),
            quality_hint: None,
            plugin_track_id: Some("1".into()),
            format: None,
            bitrate: None,
            sample_rate: None,
            channels: None,
            added_at: 0,
        };
        let json = serde_json::to_string(&t).unwrap();
        // 直链不出进程、降级提示未设置时不占字段
        assert!(!json.contains("streamUrl"));
        assert!(!json.contains("qualityHint"));
        let back: Track = serde_json::from_str(&json).unwrap();
        assert!(back.quality_hint.is_none());
    }
}
