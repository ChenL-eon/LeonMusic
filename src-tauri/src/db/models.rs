//! 数据访问层（CRUD）。
//!
//! 所有 SQL 都集中在这里，上层只操作结构体。

use rusqlite::{params, Connection, OptionalExtension, Row};

use super::{now_ms, Db};
use crate::error::{AppError, AppResult};
use crate::models::{Playlist, RepeatMode, Track, TrackSource};

fn row_to_track(r: &Row<'_>) -> rusqlite::Result<Track> {
    let kind: String = r.get("source_kind")?;
    let plugin_id: Option<String> = r.get("plugin_id")?;
    let source = match kind.as_str() {
        "plugin" => TrackSource::Plugin {
            plugin_id: plugin_id.unwrap_or_default(),
        },
        "stream" => TrackSource::Stream,
        _ => TrackSource::Local,
    };
    Ok(Track {
        id: r.get("id")?,
        source,
        title: r.get("title")?,
        artist: r.get("artist")?,
        album: r.get("album")?,
        duration_ms: r.get("duration_ms")?,
        cover_path: r.get("cover_path")?,
        cover_url: r.get("cover_url")?,
        file_path: r.get("file_path")?,
        stream_url: None,
        plugin_track_id: r.get("plugin_track_id")?,
        format: r.get("format")?,
        bitrate: r.get::<_, Option<i64>>("bitrate")?.map(|v| v as u32),
        sample_rate: r.get::<_, Option<i64>>("sample_rate")?.map(|v| v as u32),
        channels: r.get::<_, Option<i64>>("channels")?.map(|v| v as u16),
        added_at: r.get("added_at")?,
    })
}

const TRACK_COLS: &str = "id, source_kind, plugin_id, title, artist, album, duration_ms,
     cover_path, cover_url, file_path, plugin_track_id, format, bitrate, sample_rate,
     channels, added_at";

impl Db {
    pub fn upsert_track(&self, t: &Track) -> AppResult<()> {
        let (kind, plugin_id) = match &t.source {
            TrackSource::Local => ("local", None),
            TrackSource::Plugin { plugin_id } => ("plugin", Some(plugin_id.clone())),
            TrackSource::Stream => ("stream", None),
        };
        let now = now_ms();
        self.with(|c| {
            c.execute(
                "INSERT INTO tracks (id, source_kind, plugin_id, title, artist, album,
                    duration_ms, cover_path, cover_url, file_path, plugin_track_id, format,
                    bitrate, sample_rate, channels, added_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
                 ON CONFLICT(id) DO UPDATE SET
                    title=excluded.title, artist=excluded.artist, album=excluded.album,
                    duration_ms=excluded.duration_ms, cover_path=excluded.cover_path,
                    cover_url=excluded.cover_url, file_path=excluded.file_path,
                    format=excluded.format, bitrate=excluded.bitrate,
                    sample_rate=excluded.sample_rate, channels=excluded.channels,
                    updated_at=excluded.updated_at",
                params![
                    t.id,
                    kind,
                    plugin_id,
                    t.title,
                    t.artist,
                    t.album,
                    t.duration_ms,
                    t.cover_path,
                    t.cover_url,
                    t.file_path,
                    t.plugin_track_id,
                    t.format,
                    t.bitrate.map(|v| v as i64),
                    t.sample_rate.map(|v| v as i64),
                    t.channels.map(|v| v as i64),
                    t.added_at,
                    now
                ],
            )?;
            Ok(())
        })
    }

    /// 流式播放落盘完成后回填缓存路径（不动其它列）。
    pub fn update_track_file_path(&self, track_id: &str, path: &str) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE tracks SET file_path = ?2, updated_at = ?3 WHERE id = ?1",
                params![track_id, path, now_ms()],
            )?;
            Ok(())
        })
    }

    /// 批量 upsert（云端歌单导入用）。单个事务，返回写入条数。
    pub fn upsert_tracks(&self, tracks: &[Track]) -> AppResult<usize> {
        if tracks.is_empty() {
            return Ok(0);
        }
        let mut n = 0usize;
        self.tx(|tx| {
            let now = now_ms();
            for t in tracks {
                let (kind, plugin_id) = match &t.source {
                    TrackSource::Local => ("local".to_string(), None),
                    TrackSource::Plugin { plugin_id } => {
                        ("plugin".to_string(), Some(plugin_id.clone()))
                    }
                    TrackSource::Stream => ("stream".to_string(), None),
                };
                tx.execute(
                    "INSERT INTO tracks (id, source_kind, plugin_id, title, artist, album,
                        duration_ms, cover_path, cover_url, file_path, plugin_track_id, format,
                        bitrate, sample_rate, channels, added_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
                     ON CONFLICT(id) DO UPDATE SET
                        title=excluded.title, artist=excluded.artist, album=excluded.album,
                        duration_ms=excluded.duration_ms, cover_path=excluded.cover_path,
                        cover_url=excluded.cover_url, file_path=excluded.file_path,
                        format=excluded.format, bitrate=excluded.bitrate,
                        sample_rate=excluded.sample_rate, channels=excluded.channels,
                        updated_at=excluded.updated_at",
                    params![
                        t.id,
                        kind,
                        plugin_id,
                        t.title,
                        t.artist,
                        t.album,
                        t.duration_ms,
                        t.cover_path,
                        t.cover_url,
                        t.file_path,
                        t.plugin_track_id,
                        t.format,
                        t.bitrate.map(|v| v as i64),
                        t.sample_rate.map(|v| v as i64),
                        t.channels.map(|v| v as i64),
                        t.added_at,
                        now
                    ],
                )?;
                n += 1;
            }
            Ok(())
        })?;
        Ok(n)
    }

    pub fn get_track(&self, id: &str) -> AppResult<Option<Track>> {
        self.with(|c| {
            let sql = format!("SELECT {TRACK_COLS} FROM tracks WHERE id = ?1");
            Ok(c.query_row(&sql, params![id], row_to_track).optional()?)
        })
    }

    /// 清掉指向已不存在文件的 `file_path`（仅插件源曲目）。
    ///
    /// 缓存文件可能被外部删除：LRU 淘汰、用户手动清理 media 目录、
    /// 或旧版本缓存命名升级后的残留（如 sha1 无扩展名 → sha256+ext）。
    /// 记录不清掉的话，点播会直接「io: 系统找不到指定的文件」，
    /// 且因为 file_path 非空永远不触发重新解析，成了一条死记录。
    /// 本地源曲目不在此列：文件丢了也没有在线兜底，保留原路径让
    /// 引擎给出「文件已丢失」的针对性提示。
    pub fn clear_missing_cache_paths(&self) -> AppResult<usize> {
        let stale: Vec<String> = self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, file_path FROM tracks \
                 WHERE plugin_id IS NOT NULL AND plugin_id != '' \
                 AND file_path IS NOT NULL AND file_path != ''",
            )?;
            let it = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(it
                .into_iter()
                .filter(|(_, p)| !std::path::Path::new(p).exists())
                .map(|(id, _)| id)
                .collect())
        })?;
        if stale.is_empty() {
            return Ok(0);
        }
        let n = self.with(|c| {
            let mut total = 0;
            for id in &stale {
                total += c.execute(
                    "UPDATE tracks SET file_path = NULL WHERE id = ?1",
                    params![id],
                )?;
            }
            Ok(total)
        })?;
        tracing::info!("cleared {n} stale track file paths");
        Ok(n)
    }

    pub fn tracks_by_ids(&self, ids: &[String]) -> AppResult<Vec<Track>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.with(|c| {
            let mut out = Vec::with_capacity(ids.len());
            let mut stmt = c.prepare(&format!("SELECT {TRACK_COLS} FROM tracks WHERE id = ?1"))?;
            for id in ids {
                if let Some(t) = stmt.query_row(params![id], row_to_track).optional()? {
                    out.push(t);
                }
            }
            Ok(out)
        })
    }

    pub fn list_tracks(
        &self,
        query: Option<&str>,
        source: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> AppResult<Vec<Track>> {
        self.with(|c| {
            let mut sql = format!("SELECT {TRACK_COLS} FROM tracks WHERE 1=1");
            let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

            if let Some(q) = query.filter(|q| !q.trim().is_empty()) {
                sql.push_str(" AND (title LIKE ?1 OR artist LIKE ?2 OR album LIKE ?3)");
                let like = format!("%{}%", q.trim());
                args.push(Box::new(like.clone()));
                args.push(Box::new(like.clone()));
                args.push(Box::new(like));
            }
            if let Some(s) = source {
                if s == "local" {
                    sql.push_str(" AND source_kind = 'local'");
                } else if s != "all" {
                    sql.push_str(" AND plugin_id = ?4");
                    args.push(Box::new(s.to_string()));
                }
            }
            sql.push_str(" ORDER BY artist, album, title LIMIT ? LIMIT_OFFSET");
            // 简化：不用动态占位符，直接内联整数（非用户输入，安全）
            sql = sql.replace("LIMIT ? LIMIT_OFFSET", &format!("LIMIT {limit} OFFSET {offset}"));

            let mut stmt = c.prepare(&sql)?;
            let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
            let rows = stmt.query_map(refs.as_slice(), row_to_track)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
    }

    pub fn count_tracks(&self, query: Option<&str>) -> AppResult<i64> {
        self.with(|c| {
            let n = match query.filter(|q| !q.trim().is_empty()) {
                Some(q) => {
                    let like = format!("%{}%", q.trim());
                    c.query_row(
                        "SELECT COUNT(*) FROM tracks WHERE title LIKE ?1 OR artist LIKE ?2 OR album LIKE ?3",
                        params![like, like, like],
                        |r| r.get(0),
                    )?
                }
                None => c.query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))?,
            };
            Ok(n)
        })
    }

    pub fn delete_track(&self, id: &str) -> AppResult<()> {
        self.with(|c| {
            c.execute("DELETE FROM tracks WHERE id = ?1", params![id])?;
            c.execute("DELETE FROM playlist_tracks WHERE track_id = ?1", params![id])?;
            Ok(())
        })
    }

    /// 清空本地曲目。给「重新扫描」与「移除媒体库目录」用；
    /// 注意 `playlist_tracks` 是 ON DELETE CASCADE，歌单里的本地曲目会一起消失。
    #[allow(dead_code)]
    pub fn clear_local_tracks(&self) -> AppResult<()> {
        self.with(|c| {
            c.execute("DELETE FROM tracks WHERE source_kind = 'local'", [])?;
            Ok(())
        })
    }

    // ---- playlists ----

    /// 歌单列表。封面优先用歌单自己记的（云端导入写入 cover_url），
    /// 都没有时回退到歌单内第一首曲目的封面 —— 手动建的本地歌单也有图可看。
    pub fn list_playlists(&self) -> AppResult<Vec<Playlist>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT p.id, p.name,
                        COALESCE(p.cover_path, (
                            SELECT t.cover_path FROM playlist_tracks pt
                            JOIN tracks t ON t.id = pt.track_id
                            WHERE pt.playlist_id = p.id ORDER BY pt.position LIMIT 1)) AS cover_path,
                        COALESCE(p.cover_url, (
                            SELECT t.cover_url FROM playlist_tracks pt
                            JOIN tracks t ON t.id = pt.track_id
                            WHERE pt.playlist_id = p.id ORDER BY pt.position LIMIT 1)) AS cover_url,
                        p.remote_key,
                        p.system, p.created_at, p.updated_at,
                        (SELECT COUNT(*) FROM playlist_tracks pt WHERE pt.playlist_id = p.id) AS cnt
                 FROM playlists p ORDER BY p.system DESC, p.updated_at DESC",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(Playlist {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    cover_path: r.get(2)?,
                    cover_url: r.get(3)?,
                    remote_key: r.get(4)?,
                    system: r.get::<_, i64>(5)? != 0,
                    created_at: r.get(6)?,
                    updated_at: r.get(7)?,
                    track_count: r.get(8)?,
                })
            })?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
    }

    pub fn create_playlist(&self, name: &str) -> AppResult<Playlist> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_ms();
        self.with(|c| {
            c.execute(
                "INSERT INTO playlists (id, name, system, created_at, updated_at)
                 VALUES (?1, ?2, 0, ?3, ?3)",
                params![id, name, now],
            )?;
            Ok(())
        })?;
        Ok(Playlist {
            id,
            name: name.to_string(),
            cover_path: None,
            cover_url: None,
            remote_key: None,
            track_count: 0,
            created_at: now,
            updated_at: now,
            system: false,
        })
    }

    /// 记录歌单封面（云端导入时写入网络封面 URL；传 None 清除）。
    pub fn set_playlist_cover_url(&self, id: &str, cover_url: Option<&str>) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE playlists SET cover_url = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, cover_url, now_ms()],
            )?;
            Ok(())
        })
    }

    /// 记录歌单的云端来源（`pluginId:cloudId`），用于「已导入过」检测；传 None 清除。
    pub fn set_playlist_remote_key(&self, id: &str, remote_key: Option<&str>) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE playlists SET remote_key = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, remote_key, now_ms()],
            )?;
            Ok(())
        })
    }

    pub fn rename_playlist(&self, id: &str, name: &str) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE playlists SET name = ?2, updated_at = ?3 WHERE id = ?1 AND system = 0",
                params![id, name, now_ms()],
            )?;
            Ok(())
        })
    }

    pub fn delete_playlist(&self, id: &str) -> AppResult<()> {
        self.with(|c| {
            c.execute("DELETE FROM playlists WHERE id = ?1 AND system = 0", params![id])?;
            c.execute("DELETE FROM playlist_tracks WHERE playlist_id = ?1", params![id])?;
            Ok(())
        })
    }

    pub fn playlist_tracks(&self, playlist_id: &str) -> AppResult<Vec<Track>> {
        self.with(|c| {
            let sql = format!(
                "SELECT {} FROM tracks t JOIN playlist_tracks pt ON pt.track_id = t.id
                 WHERE pt.playlist_id = ?1 ORDER BY pt.position",
                TRACK_COLS
                    .split(", ")
                    .map(|col| format!("t.{col}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map(params![playlist_id], row_to_track)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
    }

    pub fn add_to_playlist(&self, playlist_id: &str, track_ids: &[String]) -> AppResult<()> {
        let now = now_ms();
        self.tx(|tx| {
            let next: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(position)+1, 0) FROM playlist_tracks WHERE playlist_id = ?1",
                    params![playlist_id],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            for (i, id) in track_ids.iter().enumerate() {
                tx.execute(
                    "INSERT OR IGNORE INTO playlist_tracks (playlist_id, track_id, position, added_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![playlist_id, id, next + i as i64, now],
                )?;
            }
            tx.execute(
                "UPDATE playlists SET updated_at = ?2 WHERE id = ?1",
                params![playlist_id, now],
            )?;
            Ok(())
        })
    }

    pub fn remove_from_playlist(&self, playlist_id: &str, track_ids: &[String]) -> AppResult<()> {
        self.tx(|tx| {
            for id in track_ids {
                tx.execute(
                    "DELETE FROM playlist_tracks WHERE playlist_id = ?1 AND track_id = ?2",
                    params![playlist_id, id],
                )?;
            }
            Ok(())
        })
    }

    pub fn is_favorite(&self, track_id: &str) -> AppResult<bool> {
        self.with(|c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM playlist_tracks WHERE playlist_id = 'favorites' AND track_id = ?1",
                params![track_id],
                |r| r.get(0),
            )?;
            Ok(n > 0)
        })
    }

    pub fn favorite_ids(&self) -> AppResult<Vec<String>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT track_id FROM playlist_tracks WHERE playlist_id = 'favorites' ORDER BY position",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
    }

    // ---- history ----

    pub fn record_play(&self, track_id: &str, ms_played: i64) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO play_history (track_id, played_at, ms_played) VALUES (?1, ?2, ?3)",
                params![track_id, now_ms(), ms_played],
            )?;
            // 只保留最近 5000 条
            c.execute(
                "DELETE FROM play_history WHERE id NOT IN
                 (SELECT id FROM play_history ORDER BY played_at DESC LIMIT 5000)",
                [],
            )?;
            Ok(())
        })
    }

    pub fn recent_tracks(&self, limit: i64) -> AppResult<Vec<Track>> {
        self.with(|c| {
            let sql = format!(
                "SELECT {} FROM tracks t
                 WHERE t.id IN (SELECT track_id FROM play_history ORDER BY played_at DESC LIMIT {limit})
                 GROUP BY t.id
                 ORDER BY MAX(t.added_at) DESC",
                TRACK_COLS
                    .split(", ")
                    .map(|col| format!("t.{col}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map([], row_to_track)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
    }

    // ---- settings ----

    pub fn get_setting(&self, key: &str) -> AppResult<Option<String>> {
        self.with(|c| {
            Ok(c.query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?)
        })
    }

    pub fn set_setting(&self, key: &str, value: &str) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
            Ok(())
        })
    }

    // ---- plugin 配置 ----

    pub fn plugin_row(&self, id: &str) -> AppResult<Option<(bool, String)>> {
        self.with(|c| {
            Ok(c.query_row(
                "SELECT enabled, config_json FROM plugins WHERE id = ?1",
                params![id],
                |r| Ok((r.get::<_, i64>(0)? != 0, r.get::<_, String>(1)?)),
            )
            .optional()?)
        })
    }

    pub fn set_plugin_enabled(&self, id: &str, enabled: bool) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO plugins (id, enabled, config_json, installed_at)
                 VALUES (?1, ?2, '{}', ?3)
                 ON CONFLICT(id) DO UPDATE SET enabled = excluded.enabled",
                params![id, enabled as i64, now_ms()],
            )?;
            Ok(())
        })
    }

    pub fn set_plugin_config(&self, id: &str, json: &str) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO plugins (id, enabled, config_json, installed_at)
                 VALUES (?1, 1, ?2, ?3)
                 ON CONFLICT(id) DO UPDATE SET config_json = excluded.config_json",
                params![id, json, now_ms()],
            )?;
            Ok(())
        })
    }

    // ---- 歌词缓存 ----

    pub fn save_lyrics(&self, track_id: &str, content: &str, kind: &str) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO lyrics_cache (track_id, content, kind, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(track_id) DO UPDATE SET content=excluded.content, kind=excluded.kind,
                    updated_at=excluded.updated_at",
                params![track_id, content, kind, now_ms()],
            )?;
            Ok(())
        })
    }

    pub fn load_lyrics(&self, track_id: &str) -> AppResult<Option<String>> {
        self.with(|c| {
            Ok(c.query_row(
                "SELECT content FROM lyrics_cache WHERE track_id = ?1",
                params![track_id],
                |r| r.get(0),
            )
            .optional()?)
        })
    }

    // ---- 缓存索引 ----

    pub fn put_cache(&self, key: &str, url: &str, path: &str, size: i64) -> AppResult<()> {
        let now = now_ms();
        self.with(|c| {
            c.execute(
                "INSERT INTO cache_entries (cache_key, url, file_path, size_bytes, created_at, last_access)
                 VALUES (?1,?2,?3,?4,?5,?5)
                 ON CONFLICT(cache_key) DO UPDATE SET last_access = excluded.last_access,
                    size_bytes = excluded.size_bytes, file_path = excluded.file_path",
                params![key, url, path, size, now],
            )?;
            Ok(())
        })
    }

    pub fn get_cache(&self, key: &str) -> AppResult<Option<(String, i64)>> {
        self.with(|c| {
            Ok(c.query_row(
                "SELECT file_path, size_bytes FROM cache_entries WHERE cache_key = ?1",
                params![key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
        })
    }

    pub fn touch_cache(&self, key: &str) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE cache_entries SET last_access = ?2 WHERE cache_key = ?1",
                params![key, now_ms()],
            )?;
            Ok(())
        })
    }

    pub fn cache_entries_oldest(&self, limit: usize) -> AppResult<Vec<(String, String, i64)>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT cache_key, file_path, size_bytes FROM cache_entries
                 ORDER BY last_access ASC LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
    }

    pub fn drop_cache(&self, key: &str) -> AppResult<()> {
        self.with(|c| {
            c.execute("DELETE FROM cache_entries WHERE cache_key = ?1", params![key])?;
            Ok(())
        })
    }

    pub fn cache_total_size(&self) -> AppResult<i64> {
        self.with(|c| {
            let n: i64 = c.query_row(
                "SELECT COALESCE(SUM(size_bytes), 0) FROM cache_entries",
                [],
                |r| r.get(0),
            )?;
            Ok(n)
        })
    }
}

// ---------------------------------------------------------------------------
// 设置结构
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub theme: String,
    pub accent: String,
    pub volume: f32,
    pub speed: f32,
    pub pitch: f32,
    pub gain_db: f32,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    pub library_paths: Vec<String>,
    pub lyric_font_size: u32,
    pub lyric_align: String,
    pub lyric_show_translation: bool,
    pub close_to_tray: bool,
    /// 播放页背景来源：`cover` = 当前曲目封面（默认），`off` = 关闭。
    pub now_bg_source: String,
    /// 播放页封面背景的模糊半径（px），0 为不模糊。
    pub now_bg_blur: u32,
    /// 桌面歌词：锁定后窗口鼠标穿透，只能从设置页解除。
    pub dl_locked: bool,
    /// 桌面歌词：排版方向，`horizontal` | `vertical`。
    pub dl_direction: String,
    /// 桌面歌词：对齐方式，`left` | `center` | `right`。
    pub dl_align: String,
    /// 桌面歌词：双行歌词（上一句 + 下一句交替），开启时隐藏翻译。
    pub dl_dual_line: bool,
    /// 桌面歌词：字号（px），工具条 A-/A+ 与设置页共用。
    pub dl_font_size: u32,
    /// 桌面歌词：颜色模式，`theme` = 主题渐变卡拉OK，`custom` = 自定义纯色。
    pub dl_color_mode: String,
    /// 桌面歌词：歌词颜色（自定义模式生效，#RRGGBB[AA]）。
    pub dl_color_lyric: String,
    /// 桌面歌词：翻译/旁行颜色（自定义模式生效）。
    pub dl_color_trans: String,
    /// 桌面歌词：窗口是否置顶。
    pub dl_always_on_top: bool,
    pub cache_limit_mb: u64,
    pub crossfade_ms: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            accent: "#7c5cff".into(),
            volume: 0.85,
            speed: 1.0,
            pitch: 0.0,
            gain_db: 0.0,
            repeat: RepeatMode::All,
            shuffle: false,
            library_paths: Vec::new(),
            lyric_font_size: 34,
            lyric_align: "center".into(),
            lyric_show_translation: true,
            close_to_tray: true,
            now_bg_source: "cover".into(),
            now_bg_blur: 60,
            dl_locked: false,
            dl_direction: "horizontal".into(),
            dl_align: "center".into(),
            dl_dual_line: true,
            dl_font_size: 34,
            dl_color_mode: "theme".into(),
            dl_color_lyric: "#ffffff".into(),
            dl_color_trans: "#ffffff".into(),
            dl_always_on_top: true,
            cache_limit_mb: 2048,
            crossfade_ms: 0,
        }
    }
}

const SETTINGS_KEY: &str = "app.settings";

impl Db {
    pub fn load_settings(&self) -> AppResult<Settings> {
        match self.get_setting(SETTINGS_KEY)? {
            Some(json) => Ok(serde_json::from_str(&json).unwrap_or_default()),
            None => Ok(Settings::default()),
        }
    }

    pub fn save_settings(&self, s: &Settings) -> AppResult<()> {
        let json = serde_json::to_string(s)
            .map_err(|e| AppError::invalid(format!("serialize settings: {e}")))?;
        self.set_setting(SETTINGS_KEY, &json)
    }
}

/// 仅用于日志的 Connection 计数（避免 unused import 警告）
#[allow(dead_code)]
fn _c(_c: &Connection) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TrackSource;

    fn make_track(id: &str, source: TrackSource, file_path: Option<String>) -> Track {
        Track {
            id: id.into(),
            source,
            title: "测试曲目".into(),
            artist: String::new(),
            album: String::new(),
            duration_ms: 0,
            cover_path: None,
            cover_url: None,
            file_path,
            stream_url: None,
            plugin_track_id: None,
            format: None,
            bitrate: None,
            sample_rate: None,
            channels: None,
            added_at: 0,
        }
    }

    #[test]
    fn clear_missing_cache_paths_only_touches_stale_plugin_rows() {
        let dir = std::env::temp_dir().join(format!(
            "leonmusic-dbtest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db")).unwrap();

        // 一个真实存在的缓存文件 + 一个必然不存在的路径
        let live = dir.join("live.flac");
        std::fs::write(&live, b"x").unwrap();

        db.upsert_track(&make_track(
            "netease:stale",
            TrackSource::Plugin { plugin_id: "netease".into() },
            Some(dir.join("gone-file").to_string_lossy().into_owned()),
        ))
        .unwrap();
        db.upsert_track(&make_track(
            "netease:live",
            TrackSource::Plugin { plugin_id: "netease".into() },
            Some(live.to_string_lossy().into_owned()),
        ))
        .unwrap();
        db.upsert_track(&make_track(
            "local:lost",
            TrackSource::Local,
            Some(dir.join("lost-local").to_string_lossy().into_owned()),
        ))
        .unwrap();

        let n = db.clear_missing_cache_paths().unwrap();
        assert_eq!(n, 1, "只清失效的插件源记录");

        assert_eq!(
            db.get_track("netease:stale").unwrap().unwrap().file_path,
            None,
            "失效缓存路径被清空（点播会重新解析）"
        );
        assert!(
            db.get_track("netease:live").unwrap().unwrap().file_path.is_some(),
            "文件还在的记录不动"
        );
        assert!(
            db.get_track("local:lost").unwrap().unwrap().file_path.is_some(),
            "本地源文件丢了也不清（引擎给针对性提示）"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
