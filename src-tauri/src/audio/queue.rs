//! 播放队列。
//!
//! 队列是**唯一**的播放顺序来源，`order` 保存实际播放次序：
//! * 顺序播放：`order == [0,1,2,...]`
//! * 随机播放：`order` 是打乱后的下标序列表，切歌只移动 `cursor`
//!
//! 这样切换随机模式不会打断当前曲目，也不需要重排 `items`。

use rand::seq::SliceRandom;

pub use crate::models::QueueSnapshot;
use crate::models::{RepeatMode, Track};

pub struct PlayQueue {
    items: Vec<Track>,
    order: Vec<usize>,
    /// 指向 order 的下标
    cursor: Option<usize>,
    repeat: RepeatMode,
    shuffle: bool,
}

impl Default for PlayQueue {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            order: Vec::new(),
            cursor: None,
            repeat: RepeatMode::All,
            shuffle: false,
        }
    }
}

impl PlayQueue {
    pub fn items(&self) -> &[Track] {
        &self.items
    }

    /// 流式落盘完成后回填缓存路径，重播同一首时直接命中本地文件。
    pub fn update_file_path(&mut self, track_id: &str, path: &str) {
        for t in &mut self.items {
            if t.id == track_id {
                t.file_path = Some(path.to_string());
            }
        }
    }

    /// 当前曲目在 `items` 中的下标。
    pub fn current_index(&self) -> Option<usize> {
        self.cursor.and_then(|c| self.order.get(c).copied())
    }

    pub fn current(&self) -> Option<&Track> {
        self.current_index().and_then(|i| self.items.get(i))
    }

    pub fn snapshot(&self) -> QueueSnapshot {
        QueueSnapshot {
            items: self.items.clone(),
            current: self.current_index(),
            repeat: self.repeat,
            shuffle: self.shuffle,
        }
    }

    /// 整体替换队列，`index` 指定要播放的位置（`items` 下标）。
    pub fn set_items(&mut self, items: Vec<Track>, index: Option<usize>) {
        self.items = items;
        self.rebuild_order();
        self.cursor = index.and_then(|i| {
            if self.items.is_empty() {
                None
            } else {
                let i = i.min(self.items.len() - 1);
                self.order.iter().position(|&x| x == i)
            }
        });
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.order.clear();
        self.cursor = None;
    }

    pub fn push(&mut self, tracks: Vec<Track>, play_next: bool) {
        if tracks.is_empty() {
            return;
        }
        let insert_at = if play_next {
            self.current_index().map(|i| i + 1).unwrap_or(self.items.len())
        } else {
            self.items.len()
        };
        for (offset, t) in tracks.into_iter().enumerate() {
            self.items.insert(insert_at + offset, t);
        }
        let current_track = self.current().cloned();
        self.rebuild_order();
        self.restore_cursor_by_track(current_track);
    }

    pub fn remove(&mut self, index: usize) {
        if index >= self.items.len() {
            return;
        }
        let was_current = self.current_index() == Some(index);
        self.items.remove(index);

        // 修正 order 中的下标
        let mut new_order: Vec<usize> = Vec::with_capacity(self.order.len());
        for &i in &self.order {
            if i == index {
                continue;
            }
            new_order.push(if i > index { i - 1 } else { i });
        }
        self.order = new_order;

        if self.items.is_empty() {
            self.cursor = None;
            return;
        }
        if was_current {
            // 删掉的是当前曲目：cursor 指向同一位置（下一首），越界则回 0
            let c = self.cursor.unwrap_or(0);
            self.cursor = Some(c.min(self.order.len().saturating_sub(1)));
        } else if let Some(c) = self.cursor {
            self.cursor = Some(c.min(self.order.len().saturating_sub(1)));
        }
    }

    pub fn move_item(&mut self, from: usize, to: usize) {
        if from >= self.items.len() || to >= self.items.len() || from == to {
            return;
        }
        let current_track = self.current().cloned();
        let t = self.items.remove(from);
        self.items.insert(to, t);
        self.rebuild_order();
        self.restore_cursor_by_track(current_track);
    }

    pub fn set_repeat(&mut self, mode: RepeatMode) {
        self.repeat = mode;
    }

    pub fn set_shuffle(&mut self, on: bool) {
        if self.shuffle == on {
            return;
        }
        self.shuffle = on;
        let current_track = self.current().cloned();
        self.rebuild_order();
        self.restore_cursor_by_track(current_track);
    }

    /// 前进一首。`auto` 表示由「播放结束」触发（受单曲循环影响）。
    /// 返回 `None` 表示队列到头了。
    pub fn next(&mut self, auto: bool) -> Option<usize> {
        if self.order.is_empty() {
            return None;
        }
        if auto && self.repeat == RepeatMode::One {
            return self.current_index();
        }
        let c = self.cursor?;
        if c + 1 < self.order.len() {
            self.cursor = Some(c + 1);
            self.current_index()
        } else if self.repeat == RepeatMode::All {
            self.cursor = Some(0);
            self.current_index()
        } else {
            None
        }
    }

    /// 上一首。
    pub fn prev(&mut self) -> Option<usize> {
        if self.order.is_empty() {
            return None;
        }
        let c = self.cursor?;
        if c > 0 {
            self.cursor = Some(c - 1);
            self.current_index()
        } else if self.repeat == RepeatMode::All {
            self.cursor = Some(self.order.len() - 1);
            self.current_index()
        } else {
            self.current_index()
        }
    }

    /// 手动选中队列中的某一项（`items` 下标）。
    pub fn jump_to(&mut self, item_index: usize) -> Option<usize> {
        if item_index >= self.items.len() {
            return None;
        }
        let pos = self.order.iter().position(|&i| i == item_index)?;
        self.cursor = Some(pos);
        self.current_index()
    }

    fn rebuild_order(&mut self) {
        self.order = (0..self.items.len()).collect();
        if self.shuffle {
            let mut rng = rand::rng();
            self.order.shuffle(&mut rng);
            // 当前曲目保持在队首，避免开启随机时突然跳走
            if let Some(c) = self.cursor {
                if let Some(&cur) = self.order.get(c) {
                    if let Some(pos) = self.order.iter().position(|&x| x == cur) {
                        self.order.swap(0, pos);
                    }
                }
            }
        }
    }

    fn restore_cursor_by_track(&mut self, track: Option<Track>) {
        if let Some(t) = track {
            if let Some(pos) = self.order.iter().position(|&i| self.items[i].id == t.id) {
                self.cursor = Some(pos);
                return;
            }
        }
        if self.order.is_empty() {
            self.cursor = None;
        } else {
            let c = self.cursor.unwrap_or(0);
            self.cursor = Some(c.min(self.order.len() - 1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str) -> Track {
        Track {
            id: id.into(),
            source: crate::models::TrackSource::Local,
            title: id.into(),
            artist: String::new(),
            album: String::new(),
            duration_ms: 0,
            cover_path: None,
            cover_url: None,
            file_path: None,
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
    fn next_respects_repeat_all() {
        let mut q = PlayQueue::default();
        q.set_items(vec![track("a"), track("b")], Some(0));
        assert_eq!(q.next(true), Some(1));
        assert_eq!(q.next(true), Some(0));
    }

    #[test]
    fn next_stops_when_repeat_off() {
        let mut q = PlayQueue::default();
        q.set_items(vec![track("a"), track("b")], Some(1));
        q.set_repeat(RepeatMode::Off);
        assert_eq!(q.next(true), None);
    }

    #[test]
    fn remove_adjusts_cursor() {
        let mut q = PlayQueue::default();
        q.set_items(vec![track("a"), track("b"), track("c")], Some(0));
        q.remove(0);
        assert_eq!(q.current().map(|t| t.id.clone()), Some("b".into()));
    }
}
