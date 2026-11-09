//! SQLite 持久化。
//!
//! 用 `parking_lot::Mutex<Connection>` 串行化访问：音乐播放器的写频率极低
//! （扫描 / 收藏 / 历史），单连接 + WAL 完全够用，也避免了连接池带来的
//! 生命周期复杂度。读多写少的场景下 WAL 允许读写并发。

mod models;

use std::path::Path;

use parking_lot::Mutex;
use rusqlite::Connection;

use crate::error::AppResult;

pub use models::*;

const SCHEMA: &str = include_str!("../../migrations/001_init.sql");

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> AppResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        ensure_column(&conn, "playlists", "cover_url", "ALTER TABLE playlists ADD COLUMN cover_url TEXT")?;
        ensure_column(&conn, "playlists", "remote_key", "ALTER TABLE playlists ADD COLUMN remote_key TEXT")?;
        // 旧版导入的 QQ 歌单封面是 300×300，播放页 / 高 DPI 屏上发糊。
        // 同 albummid 的 800×800 实测可用，直接把已入库的 URL 升一档：
        // 不用重新联网拉歌单，也不会产生第二份同名歌单。
        match upgrade_qq_cover_urls(&conn) {
            Ok(0) => {}
            Ok(n) => tracing::info!("upgraded {n} legacy QQ cover URLs to 800x800"),
            Err(e) => tracing::warn!("upgrade QQ cover URLs failed: {e}"),
        }
        // 旧版导入是「先写曲目、再建歌单」，中途失败（断网 / 关窗 / 接口报错）
        // 会留下既进不了曲库（曲库只列本地文件）、也不属于任何歌单的插件曲目：
        // 它们在界面上永远不会出现，只会白占数据库。这里顺手清掉；
        // 有播放历史的保留，否则「最近播放」会丢条目。
        match purge_orphan_plugin_tracks(&conn) {
            Ok(0) => {}
            Ok(n) => tracing::info!("purged {n} orphan plugin tracks"),
            Err(e) => tracing::warn!("purge orphan plugin tracks failed: {e}"),
        }
        conn.execute_batch("PRAGMA temp_store = MEMORY;")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> AppResult<T>) -> AppResult<T> {
        let conn = self.conn.lock();
        f(&conn)
    }

    pub fn tx<T>(&self, f: impl FnOnce(&rusqlite::Transaction<'_>) -> AppResult<T>) -> AppResult<T> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 轻量列迁移：`SCHEMA` 里的 CREATE TABLE IF NOT EXISTS 管不了老库，
/// 新增列时在这里做一次幂等补齐（pragma_table_info 需要 SQLite 3.16+，rusqlite 自带的新版满足）。
fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    ddl: &str,
) -> AppResult<()> {
    let exists: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
        [column],
        |r| r.get(0),
    )?;
    if exists == 0 {
        conn.execute_batch(ddl)?;
    }
    Ok(())
}

/// 建库时插入的内置歌单。
pub fn ensure_system_playlists(db: &Db) -> AppResult<()> {
    db.with(|c| {
        let count: i64 = c.query_row(
            "SELECT COUNT(*) FROM playlists WHERE system = 1",
            [],
            |r| r.get(0),
        )?;
        if count == 0 {
            let now = now_ms();
            c.execute(
                "INSERT INTO playlists (id, name, system, created_at, updated_at)
                 VALUES ('favorites', '我喜欢的音乐', 1, ?1, ?2)",
                rusqlite::params![now, now],
            )?;
        }
        Ok(())
    })
}

/// 把旧版 300×300 的 QQ 封面 URL 升到 800×800，返回实际更新的行数。
/// `WHERE` 只匹配旧尺寸，替换后不再命中，重复启动不会反复写盘。
fn upgrade_qq_cover_urls(conn: &Connection) -> AppResult<usize> {
    Ok(conn.execute(
        "UPDATE tracks
         SET cover_url = REPLACE(cover_url, 'T002R300x300M000', 'T002R800x800M000')
         WHERE cover_url LIKE '%T002R300x300M000%'",
        [],
    )?)
}

/// 删除「插件来源 + 不属于任何歌单 + 没有播放历史」的曲目，返回行数。
///
/// 曲库页只查 `source_kind = 'local'`，歌单页只查自己那张表，所以这类行
/// 在任何界面上都不可见（`recent_tracks` 会 JOIN tracks，故有历史的不动）。
/// 判据本身幂等：正常导入的插件曲目一定挂在歌单里，不会误删。
fn purge_orphan_plugin_tracks(conn: &Connection) -> AppResult<usize> {
    Ok(conn.execute(
        "DELETE FROM tracks
         WHERE source_kind = 'plugin'
           AND id NOT IN (SELECT track_id FROM playlist_tracks)
           AND id NOT IN (SELECT track_id FROM play_history)",
        [],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qq_cover_upgrade_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE tracks (id TEXT PRIMARY KEY, cover_url TEXT);")
            .unwrap();
        conn.execute(
            "INSERT INTO tracks VALUES ('qq:1', 'https://y.gtimg.cn/music/photo_new/T002R300x300M000abc.jpg')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO tracks VALUES ('local:1', NULL)", []).unwrap();
        conn.execute(
            "INSERT INTO tracks VALUES ('qq:2', 'https://y.gtimg.cn/music/photo_new/T002R800x800M000def.jpg')",
            [],
        )
        .unwrap();

        assert_eq!(upgrade_qq_cover_urls(&conn).unwrap(), 1);
        let url: String = conn
            .query_row("SELECT cover_url FROM tracks WHERE id = 'qq:1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            url,
            "https://y.gtimg.cn/music/photo_new/T002R800x800M000abc.jpg"
        );
        // 已是 800 的、以及 NULL 都不会再被改动
        assert_eq!(upgrade_qq_cover_urls(&conn).unwrap(), 0);
    }

    #[test]
    fn purge_keeps_tracks_that_are_reachable() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE tracks (id TEXT PRIMARY KEY, source_kind TEXT NOT NULL);
             CREATE TABLE playlist_tracks (playlist_id TEXT NOT NULL, track_id TEXT NOT NULL);
             CREATE TABLE play_history (id INTEGER PRIMARY KEY AUTOINCREMENT, track_id TEXT NOT NULL);",
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO tracks VALUES ('local:1', 'local');
             INSERT INTO tracks VALUES ('netease:qq:inplaylist', 'plugin');
             INSERT INTO tracks VALUES ('netease:qq:played', 'plugin');
             INSERT INTO tracks VALUES ('netease:qq:orphan', 'plugin');
             INSERT INTO playlist_tracks VALUES ('p1', 'netease:qq:inplaylist');
             INSERT INTO play_history (track_id) VALUES ('netease:qq:played');",
        )
        .unwrap();

        assert_eq!(purge_orphan_plugin_tracks(&conn).unwrap(), 1);
        let left: Vec<String> = conn
            .prepare("SELECT id FROM tracks ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            left,
            vec![
                "local:1",
                "netease:qq:inplaylist",
                "netease:qq:played",
            ]
        );
        // 幂等：再跑一次没有可删的
        assert_eq!(purge_orphan_plugin_tracks(&conn).unwrap(), 0);
    }
}
