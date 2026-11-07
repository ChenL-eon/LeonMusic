//! 插件命令。

use std::collections::HashMap;

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::error::{AppError, AppResult};
use crate::models::{PluginInfo, Track};
use crate::state::AppState;

#[tauri::command]
pub fn plugin_list(app: AppHandle) -> Vec<PluginInfo> {
    app.state::<AppState>().plugins.list()
}

#[tauri::command]
pub fn plugin_set_enabled(app: AppHandle, id: String, enabled: bool) -> AppResult<()> {
    app.state::<AppState>().plugins.set_enabled(&id, enabled)
}

#[tauri::command]
pub fn plugin_set_config(
    app: AppHandle,
    id: String,
    config: HashMap<String, String>,
) -> AppResult<()> {
    app.state::<AppState>().plugins.set_config(&id, config)
}

#[tauri::command]
pub async fn plugin_search(
    app: AppHandle,
    plugin_id: String,
    keyword: String,
    page: Option<u32>,
    limit: Option<u32>,
) -> AppResult<Vec<Track>> {
    let plugins = app.state::<AppState>().plugins.clone();
    if keyword.trim().is_empty() {
        return Ok(Vec::new());
    }
    plugins
        .search(&plugin_id, keyword.trim(), page.unwrap_or(1), limit.unwrap_or(30))
        .await
}

#[tauri::command]
pub async fn plugin_resolve_stream(
    app: AppHandle,
    plugin_id: String,
    track: Track,
) -> AppResult<String> {
    let plugins = app.state::<AppState>().plugins.clone();
    // 这个命令只给调试/外部调用用，保持旧的字符串返回；
    // 真正播放走 ensure_playable，那里会把规格写进播放快照。
    Ok(plugins.stream_url(&plugin_id, &track).await?.url)
}

#[tauri::command]
pub async fn plugin_lyric(
    app: AppHandle,
    plugin_id: String,
    track: Track,
) -> AppResult<String> {
    let plugins = app.state::<AppState>().plugins.clone();
    plugins.lyric(&plugin_id, &track).await
}

/// 安装/覆盖一个插件（前端「导入插件」按钮直接把 js 与 manifest 传进来）。
#[tauri::command]
pub fn plugin_install(app: AppHandle, manifest_json: String, script: String) -> AppResult<String> {
    let root = crate::paths::plugins_dir()?;
    let plugins = app.state::<AppState>().plugins.clone();
    let id = plugins.install(&root, &manifest_json, &script)?;
    plugins.reload(&[root]);
    Ok(id)
}

#[tauri::command]
pub fn plugin_reload(app: AppHandle) -> AppResult<()> {
    let root = crate::paths::plugins_dir()?;
    app.state::<AppState>().plugins.reload(&[root]);
    Ok(())
}

#[tauri::command]
pub fn plugin_open_dir(app: AppHandle) -> AppResult<String> {
    let dir = crate::paths::plugins_dir()?;
    let _ = &app;
    Ok(dir.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------------
// 扫码登录
// ---------------------------------------------------------------------------

/// 二维码内容 + 渲染好的 SVG。前端不需要引二维码库，直接丢进 DOM 就行。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginQr {
    /// 会话 key，轮询时原样带回来
    pub key: String,
    /// 二维码里编码的内容（调试/复制用）
    pub url: String,
    /// 建码时的浏览器会话 Cookie，轮询必须原样带回
    pub session: String,
    pub qr_svg: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginState {
    /// waiting / scanned / success / expired / blocked（风控或异常码）
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub logged_in: bool,
    pub user_id: i64,
    pub nickname: String,
    pub avatar_url: String,
    /// 0 无 / 10 音乐包 / 11 黑胶 VIP
    pub vip_type: i64,
    pub vip_label: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptchaSent {
    /// sent / failed
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// 发码时的浏览器会话指纹，登录时原样带回
    pub session: String,
}

/// 把登录 URL 渲染成 SVG 二维码。
///
/// 强制黑白：扫码靠的是对比度，跟着主题变深色反而扫不出来。
fn render_qr_svg(text: &str) -> Result<String, String> {
    use qrcode::render::svg;

    let code = qrcode::QrCode::new(text.as_bytes()).map_err(|e| format!("生成二维码失败：{e}"))?;
    let svg = code
        .render::<svg::Color>()
        .min_dimensions(220, 220)
        .quiet_zone(true)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build();
    // qrcode 会带一段 `<?xml version="1.0" standalone="yes"?>` 前缀。
    // 塞进 HTML 时它会被浏览器当成一条注释，虽不致命但没必要留。
    Ok(match svg.find("<svg") {
        Some(i) => svg[i..].to_string(),
        None => svg,
    })
}

#[tauri::command]
pub async fn plugin_login_qr(app: AppHandle, plugin_id: String) -> AppResult<LoginQr> {
    let plugins = app.state::<AppState>().plugins.clone();
    let (key, url, session) = plugins.login_qr(&plugin_id).await?;
    let qr_svg = render_qr_svg(&url).map_err(AppError::plugin)?;
    Ok(LoginQr {
        key,
        url,
        session,
        qr_svg,
    })
}

#[tauri::command]
pub async fn plugin_login_poll(
    app: AppHandle,
    plugin_id: String,
    key: String,
    session: Option<String>,
) -> AppResult<LoginState> {
    let plugins = app.state::<AppState>().plugins.clone();
    let session = session.unwrap_or_default();
    let value = plugins
        .login_poll(&plugin_id, &key, &session)
        .await?;
    Ok(LoginState {
        state: value
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("waiting")
            .to_string(),
        message: value
            .get("message")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    })
}

/// 发送短信验证码（验证码登录第一步）。session 由前端保存、登录时带回。
#[tauri::command]
pub async fn plugin_captcha_send(
    app: AppHandle,
    plugin_id: String,
    phone: String,
    session: Option<String>,
) -> AppResult<CaptchaSent> {
    let plugins = app.state::<AppState>().plugins.clone();
    let value = plugins
        .captcha_send(&plugin_id, &phone, "86", &session.unwrap_or_default())
        .await?;
    Ok(CaptchaSent {
        state: value
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("failed")
            .to_string(),
        message: value
            .get("message")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        session: value
            .get("session")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

/// 短信验证码登录：成功时宿主已把 cookie 并进插件配置（与扫码同一落库路径）。
#[tauri::command]
pub async fn plugin_login_cellphone(
    app: AppHandle,
    plugin_id: String,
    phone: String,
    captcha: String,
    session: Option<String>,
) -> AppResult<LoginState> {
    let plugins = app.state::<AppState>().plugins.clone();
    let value = plugins
        .login_cellphone(
            &plugin_id,
            &phone,
            &captcha,
            "86",
            &session.unwrap_or_default(),
        )
        .await?;
    Ok(LoginState {
        state: value
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("failed")
            .to_string(),
        message: value
            .get("message")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    })
}

#[tauri::command]
pub async fn plugin_login_info(app: AppHandle, plugin_id: String) -> AppResult<Account> {
    let plugins = app.state::<AppState>().plugins.clone();
    let value = plugins.login_info(&plugin_id).await?;
    Ok(Account {
        logged_in: value
            .get("loggedIn")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        user_id: value.get("userId").and_then(|v| v.as_i64()).unwrap_or(0),
        nickname: value
            .get("nickname")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        avatar_url: value
            .get("avatarUrl")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        vip_type: value.get("vipType").and_then(|v| v.as_i64()).unwrap_or(0),
        vip_label: value
            .get("vipLabel")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

// ---------------------------------------------------------------------------
// 推荐流 / 账号歌单（网易云插件 2.6.0）
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn plugin_recommend_feed(
    app: AppHandle,
    plugin_id: String,
    limit: Option<u32>,
) -> AppResult<crate::plugin::host::RecommendFeed> {
    let plugins = app.state::<AppState>().plugins.clone();
    plugins
        .recommend_feed(&plugin_id, limit.unwrap_or(12))
        .await
}

#[tauri::command]
pub async fn plugin_user_playlists(
    app: AppHandle,
    plugin_id: String,
) -> AppResult<crate::plugin::protocol::UserPlaylists> {
    let plugins = app.state::<AppState>().plugins.clone();
    plugins.user_playlists(&plugin_id).await
}

#[tauri::command]
pub async fn plugin_cloud_playlist(
    app: AppHandle,
    plugin_id: String,
    id: String,
) -> AppResult<crate::plugin::host::CloudPlaylist> {
    let plugins = app.state::<AppState>().plugins.clone();
    plugins.cloud_playlist(&plugin_id, id.trim()).await
}

#[tauri::command]
pub fn plugin_logout(app: AppHandle, plugin_id: String) -> AppResult<()> {
    app.state::<AppState>().plugins.logout(&plugin_id)
}

/// 听歌打卡：前端在切歌/播到曲尾时调用，把真实播放同步给音源平台。
/// 未登录或打卡关闭时插件返回 `ok:false`，前端据此本会话不再尝试。
#[tauri::command]
pub async fn plugin_scrobble(
    app: AppHandle,
    plugin_id: String,
    track_id: String,
    listened_ms: i64,
) -> AppResult<serde_json::Value> {
    let plugins = app.state::<AppState>().plugins.clone();
    plugins.scrobble(&plugin_id, &track_id, listened_ms).await
}

#[cfg(test)]
mod tests {
    use super::render_qr_svg;

    /// 二维码渲染是登录链路的最后一环：就算接口全通，SVG 生成不出来
    /// 用户也只能看到一个空白框。这里只验结构，肉眼确认靠真机。
    #[test]
    fn renders_login_qr_svg() {
        let svg = render_qr_svg("https://music.163.com/login?codekey=00000000-0000-0000-0000-000000000000")
            .expect("渲染失败");
        assert!(svg.starts_with("<svg"), "不是 SVG: {}", svg.chars().take(40).collect::<String>());
        assert!(svg.contains("viewBox"), "缺少 viewBox，缩放会糊");
        assert!(svg.contains("<path"), "没有绘制模块，说明矩阵是空的");
        // 白底黑块：暗色主题下如果跟着反色，相机扫不出来
        assert!(svg.contains("#000000"));
        assert!(svg.contains("#ffffff"));
    }
}
