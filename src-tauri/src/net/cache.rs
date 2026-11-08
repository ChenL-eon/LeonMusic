//! 流媒体缓存。
//!
//! 双轨策略：
//! * `ensure`：整曲落盘（`<sha256(url)>.<ext>`），下载完成才返回——本地导出
//!   等需要「必须拿到完整文件」的场景用。
//! * `begin_stream`：流式会话（`stream.rs`），边下边播立即出声，下载完成
//!   后一样落盘进缓存。播放主链路用它。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::StreamExt;

use crate::db::Db;
use crate::error::{AppError, AppResult};

use super::client::{BlockingHttp, HttpClient};
use super::stream::{self, PendingDownload};

pub struct MediaCache {
    db: Arc<Db>,
    dir: PathBuf,
}

impl MediaCache {
    pub fn new(db: Arc<Db>) -> AppResult<Self> {
        let dir = crate::paths::cache_dir()?.join("media");
        std::fs::create_dir_all(&dir)?;
        Ok(Self { db, dir })
    }

    fn key_for(url: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(url.as_bytes());
        hex::encode(h.finalize())
    }

    fn guess_ext(url: &str) -> &'static str {
        let lower = url.to_ascii_lowercase();
        let path = lower.split('?').next().unwrap_or(&lower);
        if path.ends_with(".flac") {
            "flac"
        } else if path.ends_with(".mp3") {
            "mp3"
        } else if path.ends_with(".m4a") || path.ends_with(".mp4") {
            "m4a"
        } else if path.ends_with(".m4s") {
            // B 站 DASH 音轨是 fMP4 分段（symphonia isomp4 按内容探测，
            // 存 .m4a 让 Hint 与真实容器一致，避免按 .mp3 落盘误导排查）
            "m4a"
        } else if path.ends_with(".ogg") || path.ends_with(".opus") {
            "opus"
        } else {
            "mp3"
        }
    }

    /// 按域注入防盗链 Referer。B 站 CDN（upgcxcode / bilivideo）没有
    /// Referer 直接 403，酷我 / 网易云的直链都不校验，其余域不动。
    fn referer_for(url: &str) -> Option<&'static str> {
        let lower = url.to_ascii_lowercase();
        if lower.contains("bilivideo.")
            || lower.contains("bilibili.com")
            || lower.contains("hdslb.com")
        {
            Some("https://www.bilibili.com/")
        } else {
            None
        }
    }

    /// 返回可直接交给解码器的本地路径。`on_progress(downloaded, total)` 可为 None。
    ///
    /// 流式版后播放主链路走 `begin_stream`；`ensure` 保留给「必须拿到
    /// 完整文件」的场景（本地导出 / 批量预热缓存）。
    #[allow(dead_code)]
    pub async fn ensure(
        &self,
        url: &str,
        http: &HttpClient,
        on_progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
    ) -> AppResult<PathBuf> {
        let key = Self::key_for(url);

        if let Some((path, size)) = self.db.get_cache(&key)? {
            let p = PathBuf::from(&path);
            if p.exists() && std::fs::metadata(&p).map(|m| m.len() as i64).unwrap_or(0) == size {
                let _ = self.db.touch_cache(&key);
                return Ok(p);
            }
            // 文件被外部删了或大小不符，重新下
            let _ = self.db.drop_cache(&key);
        }

        let ext = Self::guess_ext(url);
        let final_path = self.dir.join(format!("{key}.{ext}"));
        let tmp_path = self.dir.join(format!("{key}.part"));

        let mut req = http.client_for(url).get(url);
        if let Some(referer) = Self::referer_for(url) {
            req = req.header("Referer", referer);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| AppError::net(format!("download {url}: {e}")))?;
        if !resp.status().is_success() {
            return Err(AppError::net(format!("HTTP {} for {url}", resp.status())));
        }
        let total = resp.content_length().unwrap_or(0);

        {
            use std::io::Write;
            let mut file = std::fs::File::create(&tmp_path)?;
            let mut downloaded: u64 = 0;
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| AppError::net(format!("stream: {e}")))?;
                file.write_all(&chunk)?;
                downloaded += chunk.len() as u64;
                if let Some(cb) = &on_progress {
                    cb(downloaded, total);
                }
            }
            file.flush()?;
        }

        std::fs::rename(&tmp_path, &final_path)?;
        let size = std::fs::metadata(&final_path)?.len() as i64;
        self.db
            .put_cache(&key, url, &final_path.to_string_lossy(), size)?;
        Ok(final_path)
    }

    /// 流式起播：启动「边下边播 + 落盘」会话，立即返回，不等下载。
    ///
    /// 下载线程 EOF 时自行写缓存库；`PendingDownload.done` 由调用方接收
    /// （`Ok(path)` 后回填曲目 `file_path`）。切歌取消时线程静默退出。
    pub fn begin_stream(
        &self,
        url: &str,
        blocking: &BlockingHttp,
    ) -> AppResult<PendingDownload> {
        let key = Self::key_for(url);
        let url_owned = url.to_string();
        let db = self.db.clone();
        let final_path = self.dir.join(format!("{key}.{}", Self::guess_ext(url)));
        let finish_path = final_path.clone();
        let tmp_path = self.dir.join(format!("{key}.part"));

        let finish = Box::new(move |res: AppResult<()>| {
            if res.is_ok() {
                let size = std::fs::metadata(&finish_path)
                    .map(|m| m.len() as i64)
                    .unwrap_or(0);
                let _ = db.put_cache(&key, &url_owned, &finish_path.to_string_lossy(), size);
            }
        });

        stream::spawn_download(
            url.to_string(),
            blocking.stream_client_for(url).clone(),
            Self::referer_for(url),
            tmp_path,
            final_path,
            finish,
        )
    }

    /// 该路径是否是本缓存目录的产物。清理坏缓存前必须先过这一关——
    /// 用户自己的本地音乐绝不能因为「解码失败」被删。
    pub fn owns(&self, path: &Path) -> bool {
        path.parent().map(|p| p == self.dir).unwrap_or(false)
    }

    /// 丢弃一个坏缓存：删文件 + 删库记录（文件名就是 URL 的 sha256，即 key）。
    pub fn discard(&self, path: &Path) {
        let key = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let _ = std::fs::remove_file(path);
        if !key.is_empty() {
            let _ = self.db.drop_cache(&key);
        }
    }

    /// 查询某 URL 是否已有完整本地缓存（存在性校验通过）。
    pub fn cached_path(&self, url: &str) -> AppResult<Option<PathBuf>> {
        let key = Self::key_for(url);
        if let Some((path, size)) = self.db.get_cache(&key)? {
            let p = PathBuf::from(&path);
            if p.exists() && std::fs::metadata(&p).map(|m| m.len() as i64).unwrap_or(0) == size {
                let _ = self.db.touch_cache(&key);
                return Ok(Some(p));
            }
        }
        Ok(None)
    }

    /// 按 LRU 淘汰，直到总大小低于 `limit_mb`。
    pub fn enforce_limit(&self, limit_mb: u64) -> AppResult<()> {
        let limit = (limit_mb as i64) * 1024 * 1024;
        let mut total = self.db.cache_total_size()?;
        if total <= limit {
            return Ok(());
        }
        for (key, path, size) in self.db.cache_entries_oldest(64)? {
            if total <= limit {
                break;
            }
            let _ = std::fs::remove_file(Path::new(&path));
            let _ = self.db.drop_cache(&key);
            total -= size;
        }
        Ok(())
    }

    pub fn clear_all(&self) -> AppResult<()> {
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for e in entries.flatten() {
                let _ = std::fs::remove_file(e.path());
            }
        }
        Ok(())
    }
}
