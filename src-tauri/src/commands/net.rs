//! 网络辅助命令：不属于任何插件的外部 HTTP 小工具。

use std::time::Duration;

use crate::error::{AppError, AppResult};

/// 解析分享短链：跟随重定向，返回最终 URL。
///
/// 音乐平台的短链（163cn.tv 等）只有跳转后才能拿到带歌单 id 的完整地址。
/// 这是宿主命令而不是插件入口，因为「把链接变成歌单 id」是纯 HTTP 行为，
/// 与具体音源插件无关。
#[tauri::command]
pub async fn resolve_redirect(url: String) -> AppResult<String> {
    let url = url.trim().to_string();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(AppError::invalid("not an http(s) url"));
    }
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(6));
    // 163cn.tv 等网易短链直连（与插件 HTTP 同一策略）；其余尊重代理环境
    if crate::net::client::is_direct_host(&url) {
        builder = builder.no_proxy();
    }
    let client = builder
        .build()
        .map_err(|e| AppError::net(e.to_string()))?;
    let resp = client
        .get(&url)
        .header(
            reqwest::header::USER_AGENT,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36",
        )
        .send()
        .await
        .map_err(|e| AppError::net(e.to_string()))?;
    Ok(resp.url().to_string())
}
