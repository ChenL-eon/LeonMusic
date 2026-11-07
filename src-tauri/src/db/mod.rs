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
