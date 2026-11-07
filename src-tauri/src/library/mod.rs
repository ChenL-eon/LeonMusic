//! 媒体库：扫描、歌词装配、收藏。

mod metadata;
mod lyrics;
mod scanner;

pub use lyrics::{merge_translation, parse_lrc};
pub use metadata::read_track;
pub use scanner::scan_paths;

use std::sync::Arc;

use crate::db::Db;
use crate::error::AppResult;
use crate::models::{Lyrics, Track};

pub struct Library {
    pub db: Arc<Db>,
}

impl Library {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }

    /// 取歌词：缓存 → 同名 .lrc → 内嵌 tag（扫描时已写入缓存）。
    pub fn lyrics_for(&self, track: &Track) -> AppResult<Lyrics> {
        let duration = track.duration_ms.max(0) as u64;

        if let Some(cached) = self.db.load_lyrics(&track.id)? {
            return Ok(parse_lrc(&cached, duration));
        }

        let Some(path) = track.file_path.as_ref().map(std::path::PathBuf::from) else {
            return Ok(Lyrics::empty());
        };

        // 同名 lrc
        if let Some(lrc_path) = metadata::find_sidecar_lrc(&path) {
            if let Ok(text) = metadata::read_text_file(&lrc_path) {
                let _ = self.db.save_lyrics(&track.id, &text, "plain");
                return Ok(parse_lrc(&text, duration));
            }
        }

        // 翻译：`song.zh.lrc` / `song.zh-CN.lrc`
        let mut main = Lyrics::empty();
        if let Some(text) = self.find_translation(&path) {
            let trans = parse_lrc(&text, duration);
            merge_translation(&mut main, &trans);
        }

        Ok(main)
    }

    fn find_translation(&self, audio: &std::path::Path) -> Option<String> {
        let stem = audio.file_stem()?.to_string_lossy().to_string();
        let dir = audio.parent()?;
        for suffix in [".zh.lrc", ".zh-CN.lrc", ".trans.lrc", ".cn.lrc"] {
            let p = dir.join(format!("{stem}{suffix}"));
            if p.exists() {
                if let Ok(t) = metadata::read_text_file(&p) {
                    return Some(t);
                }
            }
        }
        None
    }

    pub fn toggle_favorite(&self, track_id: &str) -> AppResult<bool> {
        let is_fav = self.db.is_favorite(track_id)?;
        if is_fav {
            self.db
                .remove_from_playlist("favorites", &[track_id.to_string()])?;
            Ok(false)
        } else {
            self.db
                .add_to_playlist("favorites", &[track_id.to_string()])?;
            Ok(true)
        }
    }
}
