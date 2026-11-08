//! 播放进度记忆：退出后下次启动恢复「上次听到哪」。
//!
//! 存储是数据目录下的 `playback-state.json`：整条队列、当前曲目位置、
//! 循环与随机模式。**保存走独立线程**——ticker 的 120ms 节拍不能被
//! 磁盘 IO 拖住。触发时机（见 engine 的 ticker）：
//!
//! * 队列 / 当前曲 / 模式变化 → 立即保存
//! * 播放中每 5 秒 → 刷一次进度
//! * 队列清空 → 删除状态文件
//!
//! 恢复只把队列与模式就位、当前曲挂到快照（status 仍是 Idle），
//! **不自动播放**——用户点播放才真正加载解码器，并在 `start_track`
//! 里消费记忆位置 seek 过去。

use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::engine::SharedState;
use super::queue::QueueSnapshot;
use crate::error::AppResult;
use crate::paths;

/// 落盘结构。`queue` 直接复用 `QueueSnapshot`（Track 全量 + repeat/shuffle），
/// 恢复时 `set_items(items, current)` 一步到位。
#[derive(Debug, Serialize, Deserialize)]
pub struct ResumeFile {
    /// 整条队列（Track 全量，恢复时直接 set_items）。
    pub queue: QueueSnapshot,
    /// 保存时的播放位置。
    pub position_ms: u64,
    pub saved_at: u64,
}

fn file_path() -> AppResult<PathBuf> {
    Ok(paths::data_dir()?.join("playback-state.json"))
}

/// 启动时读取。空队列 / 解析失败一律返回 `None`（等价于没有历史）。
pub fn load() -> Option<ResumeFile> {
    let text = std::fs::read_to_string(file_path().ok()?).ok()?;
    match serde_json::from_str::<ResumeFile>(&text) {
        Ok(f) if !f.queue.items.is_empty() => Some(f),
        _ => None,
    }
}

/// 异步保存。队列非空才写；失败只留日志（记忆功能不该影响播放）。
pub fn save_async(shared: &Arc<SharedState>) {
    let snap = shared.queue_snapshot();
    if snap.items.is_empty() {
        return;
    }
    let pos = shared.position_ms();
    let _ = thread::Builder::new()
        .name("leon-resume-save".into())
        .spawn(move || {
            let f = ResumeFile {
                queue: snap,
                position_ms: pos,
                saved_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            };
            match serde_json::to_string(&f) {
                Ok(json) => {
                    if let Err(e) = std::fs::write(file_path().unwrap_or_default(), json) {
                        tracing::warn!("save playback state failed: {e}");
                    }
                }
                Err(e) => tracing::warn!("serialize playback state failed: {e}"),
            }
        });
}

/// 队列被清空时删掉状态文件，避免下次启动恢复一条已经不存在的队列。
pub fn clear_async() {
    let _ = thread::Builder::new()
        .name("leon-resume-clear".into())
        .spawn(move || {
            if let Ok(p) = file_path() {
                let _ = std::fs::remove_file(p);
            }
        });
}
