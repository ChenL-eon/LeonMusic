//! Tauri IPC 命令层。
//!
//! 这一层**只做三件事**：参数校验、拼装数据、转发给引擎。
//! 任何播放逻辑都不允许写在这里，否则前端就能通过命令绕过状态机。

mod app;
mod library;
mod net;
mod player;
mod playlist;
mod plugin;
mod settings;
mod update;

pub use app::*;
pub use library::*;
pub use net::*;
pub use player::*;
pub use playlist::*;
pub use plugin::*;
pub use settings::*;
pub use update::*;
