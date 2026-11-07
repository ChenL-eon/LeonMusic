//! 网络层：音源 API 调用 + 流媒体缓存。
//!
//! 做三件事：
//! 1. 给插件宿主提供阻塞 HTTP（插件在独立线程里同步执行）
//! 2. 把远程音频落到本地缓存（整曲落盘，重播秒开）
//! 3. 流式直链（`stream.rs`）：边下边播，不等整曲下载完成

mod cache;
pub mod client;
pub mod stream;

pub use cache::MediaCache;
pub use client::{BlockingHttp, HttpClient};
