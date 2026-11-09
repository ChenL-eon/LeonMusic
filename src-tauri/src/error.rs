//! 统一错误类型。
//!
//! 约定：所有 `#[tauri::command]` 返回 `AppResult<T>`，序列化后前端拿到
//! `{ code, message }`，前端不允许根据 message 做逻辑分支，只看 code。

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("audio: {0}")]
    Audio(String),

    #[error("decode: {0}")]
    Decode(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("db: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("net: {0}")]
    Net(String),

    #[error("plugin: {0}")]
    Plugin(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid argument: {0}")]
    Invalid(String),

    #[error("permission denied: {0}")]
    Permission(String),

    #[error("unsupported: {0}")]
    Unsupported(String),

    /// 用户主动取消（目前只有「应用内更新」的下载会走到）。
    /// 单独立一个变体：前端要把它当成正常收尾而不是错误弹窗。
    #[error("cancelled: {0}")]
    Cancelled(String),

    #[error("tauri: {0}")]
    Tauri(#[from] tauri::Error),
}

impl AppError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Audio(_) => "AUDIO",
            Self::Decode(_) => "DECODE",
            Self::Io(_) => "IO",
            Self::Db(_) => "DB",
            Self::Net(_) => "NET",
            Self::Plugin(_) => "PLUGIN",
            Self::NotFound(_) => "NOT_FOUND",
            Self::Invalid(_) => "INVALID",
            Self::Permission(_) => "PERMISSION",
            Self::Unsupported(_) => "UNSUPPORTED",
            Self::Cancelled(_) => "CANCELLED",
            Self::Tauri(_) => "TAURI",
        }
    }

    pub fn audio(e: impl std::fmt::Display) -> Self {
        Self::Audio(e.to_string())
    }
    pub fn decode(e: impl std::fmt::Display) -> Self {
        Self::Decode(e.to_string())
    }
    pub fn net(e: impl std::fmt::Display) -> Self {
        Self::Net(e.to_string())
    }
    pub fn plugin(e: impl std::fmt::Display) -> Self {
        Self::Plugin(e.to_string())
    }
    pub fn invalid(e: impl std::fmt::Display) -> Self {
        Self::Invalid(e.to_string())
    }
    pub fn cancelled(e: impl std::fmt::Display) -> Self {
        Self::Cancelled(e.to_string())
    }

    /// 去掉 `kind: ` 前缀后的原始描述。
    ///
    /// 给「需要自己拼上下文」的调用方用：直接 `format!("{e}")` 会得到
    /// `plugin: xxx`，再包一层就变成 `plugin: emby: plugin: xxx`。
    pub fn detail(&self) -> String {
        match self {
            Self::Audio(s)
            | Self::Decode(s)
            | Self::Net(s)
            | Self::Plugin(s)
            | Self::NotFound(s)
            | Self::Invalid(s)
            | Self::Permission(s)
            | Self::Unsupported(s)
            | Self::Cancelled(s) => s.clone(),
            Self::Io(e) => e.to_string(),
            Self::Db(e) => e.to_string(),
            Self::Tauri(e) => e.to_string(),
        }
    }
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("AppError", 2)?;
        s.serialize_field("code", self.code())?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

pub type AppResult<T> = Result<T, AppError>;
