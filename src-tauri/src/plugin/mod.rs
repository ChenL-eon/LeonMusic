//! 音源插件体系。
//!
//! * [`protocol`]：manifest 与 JSON 协议
//! * [`host`]：目录扫描、启用/配置、调用调度
//! * [`js_runtime`]：boa 沙箱
//!
//! # 预留 Rust 原生插件
//!
//! 目前只有 JS 插件。若要接需要系统库的音源（如本地 Navidrome 的
//! Subsonic 签名、或带 DRM 的服务），在 `host` 里加一个 `Native` 变体：
//!
//! ```ignore
//! trait NativeSource: Send + Sync {
//!     fn search(&self, kw: &str, page: u32, limit: u32) -> AppResult<Vec<RemoteTrack>>;
//!     fn stream_url(&self, track: &Track) -> AppResult<String>;
//! }
//! ```
//!
//! `PluginManager::call` 按 `LoadedPlugin::kind` 分派即可，上层协议不变。

pub mod host;
mod js_runtime;
pub mod protocol;

pub use host::PluginManager;
