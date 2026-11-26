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
        self.set_favorite(track_id, !is_fav)
    }

    /// 把收藏写成目标状态（幂等）：已在目标状态时不做任何写入，直接返回。
    ///
    /// 前端红心按钮要同时维护「本地收藏」和「云端红心」两套状态，二者可能
    /// 本来就不同步（例如在网易云 App 里红心过、本地没入库）。盲 toggle 会
    /// 把用户想去的方向翻反，所以这里一律按目标状态落库。
    pub fn set_favorite(&self, track_id: &str, favorite: bool) -> AppResult<bool> {
        let is_fav = self.db.is_favorite(track_id)?;
        if is_fav != favorite {
            if favorite {
                self.db
                    .add_to_playlist("favorites", &[track_id.to_string()])?;
            } else {
                self.db
                    .remove_from_playlist("favorites", &[track_id.to_string()])?;
            }
        }
        Ok(favorite)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TrackSource;

    fn track(id: &str, source: TrackSource) -> Track {
        Track {
            id: id.into(),
            source,
            title: "测试曲目".into(),
            artist: String::new(),
            album: String::new(),
            duration_ms: 0,
            cover_path: None,
            cover_url: None,
            file_path: None,
            stream_url: None,
            quality_hint: None,
            plugin_track_id: None,
            format: None,
            bitrate: None,
            sample_rate: None,
            channels: None,
            vip: false,
            added_at: 0,
        }
    }

    fn temp_library(tag: &str) -> (std::path::PathBuf, Library) {
        let dir = std::env::temp_dir().join(format!(
            "leonmusic-lib-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::Db::open(&dir.join("t.db")).unwrap();
        (dir, Library::new(std::sync::Arc::new(db)))
    }

    /// LM-07：按目标状态收藏，重复写同一个状态必须保持幂等（不能反向翻转）。
    #[test]
    fn set_favorite_is_idempotent_in_both_directions() {
        let (dir, lib) = temp_library("set-fav");
        lib.db
            .upsert_track(&track("netease:42", TrackSource::Plugin { plugin_id: "netease".into() }))
            .unwrap();

        assert!(lib.set_favorite("netease:42", true).unwrap());
        assert!(lib.set_favorite("netease:42", true).unwrap());
        assert_eq!(lib.db.favorite_ids().unwrap(), vec!["netease:42".to_string()]);

        assert!(!lib.set_favorite("netease:42", false).unwrap());
        assert!(!lib.set_favorite("netease:42", false).unwrap());
        assert!(lib.db.favorite_ids().unwrap().is_empty());
        assert!(!lib.db.is_favorite("netease:42").unwrap());

        let _ = std::fs::remove_dir_all(dir);
    }

    /// 云端说「已红心」而本地收藏也在时，旧代码的 `toggleFavorite` 会把本地
    /// 收藏翻掉（本地取消、云端仍红心）。目标状态写法则保持两边一致。
    #[test]
    fn set_favorite_keeps_local_in_sync_with_cloud_true() {
        let (dir, lib) = temp_library("cloud-true");
        lib.db
            .upsert_track(&track("netease:7", TrackSource::Plugin { plugin_id: "netease".into() }))
            .unwrap();
        lib.db
            .add_to_playlist("favorites", &["netease:7".to_string()])
            .unwrap();
        assert!(lib.db.is_favorite("netease:7").unwrap());

        // 云端返回 liked=true → 本地也写 true，收藏应保留
        assert!(lib.set_favorite("netease:7", true).unwrap());
        assert!(lib.db.is_favorite("netease:7").unwrap());

        let _ = std::fs::remove_dir_all(dir);
    }

    /// toggle 仍然按当前状态取反（列表页「点一下就收藏」的语义）。
    #[test]
    fn toggle_favorite_still_flips_current_state() {
        let (dir, lib) = temp_library("toggle");
        lib.db
            .upsert_track(&track("local:1", TrackSource::Local))
            .unwrap();

        assert!(lib.toggle_favorite("local:1").unwrap());
        assert!(lib.db.is_favorite("local:1").unwrap());
        assert!(!lib.toggle_favorite("local:1").unwrap());
        assert!(!lib.db.is_favorite("local:1").unwrap());

        let _ = std::fs::remove_dir_all(dir);
    }
}
