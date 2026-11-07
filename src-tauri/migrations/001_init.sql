-- LeonMusic schema v1
-- 约定：所有时间戳是 Unix 毫秒；id 是字符串（local:<sha1> / <plugin>:<id>）

CREATE TABLE IF NOT EXISTS tracks (
    id              TEXT PRIMARY KEY,
    source_kind     TEXT NOT NULL,              -- local | plugin | stream
    plugin_id       TEXT,
    title           TEXT NOT NULL,
    artist          TEXT NOT NULL DEFAULT '',
    album           TEXT NOT NULL DEFAULT '',
    duration_ms     INTEGER NOT NULL DEFAULT 0,
    cover_path      TEXT,
    cover_url       TEXT,
    file_path       TEXT,
    plugin_track_id TEXT,
    format          TEXT,
    bitrate         INTEGER,
    sample_rate     INTEGER,
    channels        INTEGER,
    added_at        INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_tracks_title  ON tracks(title);
CREATE INDEX IF NOT EXISTS idx_tracks_artist ON tracks(artist);
CREATE INDEX IF NOT EXISTS idx_tracks_source ON tracks(source_kind, plugin_id);

CREATE TABLE IF NOT EXISTS playlists (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    cover_path TEXT,
    cover_url  TEXT,
    remote_key TEXT,
    system     INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS playlist_tracks (
    playlist_id TEXT NOT NULL,
    track_id    TEXT NOT NULL,
    position    INTEGER NOT NULL,
    added_at    INTEGER NOT NULL,
    PRIMARY KEY (playlist_id, track_id)
);

CREATE INDEX IF NOT EXISTS idx_playlist_tracks_pos ON playlist_tracks(playlist_id, position);

CREATE TABLE IF NOT EXISTS play_history (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    track_id  TEXT NOT NULL,
    played_at INTEGER NOT NULL,
    ms_played INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_history_time ON play_history(played_at DESC);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS plugins (
    id           TEXT PRIMARY KEY,
    enabled      INTEGER NOT NULL DEFAULT 1,
    config_json  TEXT NOT NULL DEFAULT '{}',
    installed_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS cache_entries (
    cache_key   TEXT PRIMARY KEY,
    url         TEXT NOT NULL,
    file_path   TEXT NOT NULL,
    size_bytes  INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL,
    last_access INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_cache_access ON cache_entries(last_access);

CREATE TABLE IF NOT EXISTS lyrics_cache (
    track_id   TEXT PRIMARY KEY,
    content    TEXT NOT NULL,
    kind       TEXT NOT NULL DEFAULT 'plain',
    updated_at INTEGER NOT NULL
);
