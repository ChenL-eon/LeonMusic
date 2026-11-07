//! 应用数据目录。
//!
//! 必须通过 Tauri 的 path resolver 初始化：Android 上 `dirs` crate 拿不到
//! 正确的 sandbox 路径，而 `app_data_dir()` 会返回
//! `/data/user/0/com.leonmusic.player/files`。

use std::path::PathBuf;
use std::sync::OnceLock;

use tauri::{AppHandle, Manager};

use crate::error::{AppError, AppResult};

#[derive(Debug, Clone)]
pub struct Paths {
    pub data: PathBuf,
    pub cache: PathBuf,
    pub covers: PathBuf,
    pub plugins: PathBuf,
    pub db: PathBuf,
}

static PATHS: OnceLock<Paths> = OnceLock::new();

pub fn init(app: &AppHandle) -> AppResult<()> {
    let base = app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::invalid(format!("app_data_dir: {e}")))?;
    let cache = app
        .path()
        .app_cache_dir()
        .unwrap_or_else(|_| base.join("cache"));

    let covers = base.join("covers");
    let plugins = base.join("plugins");
    for d in [&base, &cache, &covers, &plugins] {
        std::fs::create_dir_all(d)?;
    }

    let paths = Paths {
        db: base.join("leonmusic.db"),
        data: base,
        cache,
        covers,
        plugins,
    };
    let _ = PATHS.set(paths);
    Ok(())
}

fn get() -> AppResult<&'static Paths> {
    PATHS
        .get()
        .ok_or_else(|| AppError::invalid("paths not initialized"))
}

pub fn data_dir() -> AppResult<PathBuf> {
    Ok(get()?.data.clone())
}

pub fn cache_dir() -> AppResult<PathBuf> {
    Ok(get()?.cache.clone())
}

pub fn covers_dir() -> AppResult<PathBuf> {
    Ok(get()?.covers.clone())
}

pub fn plugins_dir() -> AppResult<PathBuf> {
    Ok(get()?.plugins.clone())
}

pub fn db_path() -> AppResult<PathBuf> {
    Ok(get()?.db.clone())
}

/// 内置插件目录：打包时随二进制一起分发。
pub fn bundled_plugins_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .resource_dir()
        .map(|p| p.join("plugins"))
        .unwrap_or_else(|_| PathBuf::from("plugins"))
}
