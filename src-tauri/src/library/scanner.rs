//! 目录扫描。
//!
//! 单线程递归 + 逐文件入库。对 1 万首级别的库大约几秒到十几秒，
//! 期间通过 `leon://library/scan-progress` 推送进度，UI 不阻塞。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use tauri::{AppHandle, Emitter};

use crate::db::Db;
use crate::error::AppResult;
use crate::events;
use crate::models::ScanSummary;

use super::metadata::{self, read_track};

const MAX_DEPTH: usize = 12;

pub fn scan_paths(
    db: &Db,
    paths: Vec<PathBuf>,
    app: &AppHandle,
) -> AppResult<ScanSummary> {
    let mut summary = ScanSummary {
        scanned: 0,
        added: 0,
        updated: 0,
        failed: 0,
        errors: Vec::new(),
    };

    // 先收集文件，便于显示总进度
    let mut files: Vec<PathBuf> = Vec::new();
    for p in &paths {
        if p.is_dir() {
            walk(p, 0, &mut files);
        } else if p.is_file() && is_audio(p) {
            files.push(p.clone());
        }
    }

    let total = files.len();
    let done = AtomicUsize::new(0);

    for file in files {
        summary.scanned += 1;
        match read_track(&file) {
            Ok(scanned) => {
                let existed = db.get_track(&scanned.track.id)?.is_some();
                db.upsert_track(&scanned.track)?;
                if existed {
                    summary.updated += 1;
                } else {
                    summary.added += 1;
                }
                if let Some(lrc) = scanned.embedded_lyrics {
                    let _ = db.save_lyrics(&scanned.track.id, &lrc, "plain");
                }
            }
            Err(e) => {
                summary.failed += 1;
                if summary.errors.len() < 20 {
                    summary.errors.push(format!("{}: {e}", file.display()));
                }
            }
        }

        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        if n % 10 == 0 || n == total {
            let _ = app.emit(
                events::EVT_SCAN_PROGRESS,
                serde_json::json!({ "done": n, "total": total }),
            );
        }
    }

    let _ = app.emit(events::EVT_LIBRARY_CHANGED, ());
    Ok(summary)
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            let name = path.file_name().map(|n| n.to_string_lossy().to_string());
            // 跳过明显的系统/隐藏目录
            if let Some(n) = name {
                if n.starts_with('.') || n == "node_modules" || n == "$RECYCLE.BIN" {
                    continue;
                }
            }
            walk(&path, depth + 1, out);
        } else if ft.is_file() && is_audio(&path) {
            out.push(path);
        }
    }
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(metadata::supported_ext)
        .unwrap_or(false)
}
