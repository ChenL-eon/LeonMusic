//! 元数据与封面读取（lofty）。
//!
//! 封面落盘策略：原图直接写进 `covers/<sha1(path)>.<ext>`，
//! 前端用 `asset://localhost/...` 加载。不做缩放——`<img>` 的 `object-fit`
//! 足够，避免引入 image 处理带来的 CPU 开销与质量损失。

use std::path::Path;

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::{Accessor, ItemKey};
use lofty::probe::Probe;

use crate::error::{AppError, AppResult};
use crate::models::{Track, TrackSource};

pub struct ScannedTrack {
    pub track: Track,
    pub embedded_lyrics: Option<String>,
}

fn sha1_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    hex::encode(h.finalize())
}

pub fn track_id_for_path(path: &Path) -> String {
    format!("local:{}", sha1_hex(&path.to_string_lossy()))
}

/// 从文件名猜 "艺术家 - 标题"（很多无损资源没有 tag）。
/// 剥掉文件名开头的音轨号：`01 `、`1.`、`03 - `、`12_`。
///
/// 判据刻意保守，避免误伤以数字开头的歌名（`7 Rings`、`1984`）：
/// - 数字 1~3 位；
/// - 且满足其一：带前导零（`01`），或后面紧跟显式分隔符（`.` `-` `_` `)`）。
///
/// 单纯的 `7 Rings.mp3` 不剥——宁可留着序号，也不能把歌名切坏。
fn strip_track_number(s: &str) -> &str {
    let digits = s
        .char_indices()
        .take_while(|(_, c)| c.is_ascii_digit())
        .count();
    if digits == 0 || digits > 3 {
        return s;
    }
    let head = &s[..digits];
    let rest = &s[digits..];
    let sep = rest.chars().next();
    let zero_padded = head.len() > 1 && head.starts_with('0');
    let explicit_sep = matches!(sep, Some('.') | Some('-') | Some('_') | Some(')'));
    if !zero_padded && !explicit_sep {
        return s;
    }
    let trimmed = rest.trim_start_matches([' ', '.', '_', '-', ')']);
    if trimmed.is_empty() {
        s
    } else {
        trimmed
    }
}

fn guess_from_filename(path: &Path) -> (String, String) {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "未知曲目".into());
    let stem = strip_track_number(&stem);
    if let Some((a, t)) = stem.split_once(" - ") {
        let (artist, title) = (a.trim(), t.trim());
        // "01 - 标题" 剥完序号后只剩 "标题"，此时 artist 其实是标题
        if artist.is_empty() {
            (title.to_string(), String::new())
        } else {
            (title.to_string(), artist.to_string())
        }
    } else {
        (stem.to_string(), String::new())
    }
}

pub fn read_track(path: &Path) -> AppResult<ScannedTrack> {
    let id = track_id_for_path(path);
    let (fallback_title, fallback_artist) = guess_from_filename(path);

    let mut track = Track {
        id,
        source: TrackSource::Local,
        title: fallback_title,
        artist: fallback_artist,
        album: String::new(),
        duration_ms: 0,
        cover_path: None,
        cover_url: None,
        file_path: Some(path.to_string_lossy().into_owned()),
        stream_url: None,
        quality_hint: None,
        plugin_track_id: None,
        format: path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase()),
        bitrate: None,
        sample_rate: None,
        channels: None,
        added_at: chrono::Utc::now().timestamp_millis(),
    };

    let mut lyrics = None;

    // 元数据失败不该导致整首歌被跳过（有些文件 tag 损坏但音频正常）
    match Probe::open(path).and_then(|p| p.read()) {
        Ok(tagged) => {
            let props = tagged.properties();
            track.duration_ms = props.duration().as_millis() as i64;
            track.bitrate = props.audio_bitrate();
            track.sample_rate = props.sample_rate();
            track.channels = props.channels().map(|c| c as u16);

            if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
                if let Some(t) = tag.title() {
                    if !t.trim().is_empty() {
                        track.title = t.trim().to_string();
                    }
                }
                if let Some(a) = tag.artist() {
                    if !a.trim().is_empty() {
                        track.artist = a.trim().to_string();
                    }
                }
                if let Some(al) = tag.album() {
                    track.album = al.trim().to_string();
                }
                if let Some(text) = tag.get_string(&ItemKey::Lyrics) {
                    if !text.trim().is_empty() {
                        lyrics = Some(text.to_string());
                    }
                }

                if let Some(pic) = tag
                    .pictures()
                    .iter()
                    .find(|p| p.pic_type() == lofty::picture::PictureType::CoverFront)
                    .or_else(|| tag.pictures().first())
                {
                    if let Some(p) = save_cover(&track.id, pic) {
                        track.cover_path = Some(p);
                    }
                }
            }
        }
        Err(e) => {
            tracing::debug!("tag read failed for {}: {e}", path.display());
        }
    }

    Ok(ScannedTrack {
        track,
        embedded_lyrics: lyrics,
    })
}

fn save_cover(track_id: &str, pic: &lofty::picture::Picture) -> Option<String> {
    let dir = crate::paths::covers_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;

    let ext = pic
        .mime_type()
        .map(|m| match m.as_str() {
            "image/png" => "png",
            "image/webp" => "webp",
            "image/gif" => "gif",
            _ => "jpg",
        })
        .unwrap_or("jpg");

    let key = track_id.replace(':', "_");
    let path = dir.join(format!("{key}.{ext}"));
    if path.exists() {
        return Some(path.to_string_lossy().into_owned());
    }
    std::fs::write(&path, pic.data()).ok()?;
    Some(path.to_string_lossy().into_owned())
}

/// 查找同名 `.lrc` 文件（与音频同目录）。
pub fn find_sidecar_lrc(audio: &Path) -> Option<std::path::PathBuf> {
    let lrc = audio.with_extension("lrc");
    if lrc.exists() {
        return Some(lrc);
    }
    // 有些资源用大写扩展名
    let upper = audio.with_extension("LRC");
    if upper.exists() {
        return Some(upper);
    }
    None
}

pub fn read_text_file(path: &Path) -> AppResult<String> {
    let bytes = std::fs::read(path)?;
    // LRC 可能是 GBK，优先 UTF-8，失败则按有损 UTF-8 处理
    match String::from_utf8(bytes.clone()) {
        Ok(s) => Ok(s),
        Err(_) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
    }
}

/// 扫描阶段的扩展名过滤。权威实现只有一份：`audio::decoder`。
/// （两处各写一份的时代已经结束了——`mp4` 就曾经只在一个地方支持）
pub fn supported_ext(ext: &str) -> bool {
    crate::audio::decoder::is_supported_extension(ext)
}

#[allow(dead_code)]
fn _unused(_e: AppError) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn guess(name: &str) -> (String, String) {
        guess_from_filename(Path::new(name))
    }

    #[test]
    fn strips_leading_track_number() {
        // 有标签时不会走到这里，但纯文件名曲库（下载/抓轨）非常常见
        assert_eq!(guess("01 Aurora - Sine Wave.wav").1, "Aurora");
        assert_eq!(guess("01 Aurora - Sine Wave.wav").0, "Sine Wave");
        assert_eq!(guess("3. Night City - Deep Pulse.flac").1, "Night City");
        assert_eq!(guess("03 - 测试歌手 - 中文歌名.wav").1, "测试歌手");
        assert_eq!(guess("12_某乐队 - 某歌.mp3").1, "某乐队");
    }

    #[test]
    fn keeps_numeric_titles_intact() {
        // 以数字开头但不是序号：不能剥
        assert_eq!(guess("1984.flac").0, "1984");
        assert_eq!(guess("7 Rings - Ariana Grande.mp3").1, "7 Rings");
        // 超过 3 位的数字不当作序号
        assert_eq!(guess("20240101 - live.mp3").1, "20240101");
    }

    #[test]
    fn falls_back_to_title_only() {
        let (title, artist) = guess("Just A Song.ogg");
        assert_eq!(title, "Just A Song");
        assert!(artist.is_empty());
    }
}
