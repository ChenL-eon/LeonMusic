//! HTTP 客户端封装。
//!
//! 统一 UA / 超时 / 重试，避免插件各自实现导致行为不一致。

use std::time::Duration;

use crate::error::{AppError, AppResult};

// 不带自定义后缀：网易云风控会把明显的第三方客户端标识当成异常设备，
// 登录链路尤其敏感。普通浏览器 UA 对搜索/取链没有任何影响。
pub const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
     Chrome/122.0 Safari/537.36";

/// 网易系域名是否该**强制直连**。
///
/// 登录/风控接口对出口 IP 非常敏感：reqwest 会读取 `HTTP_PROXY` /
/// `HTTPS_PROXY` 环境变量，从带代理环境变量的终端启动 App（或以后接上
/// 系统代理支持）时，登录请求就会经代理节点出境，被服务端判
/// 「网络环境存在安全风险」（登录 code 8810）。网易域名一律绕开代理
/// 走直连，代理只留给确实需要它的第三方解析源。
pub fn is_direct_host(url: &str) -> bool {
    // 网易系：登录风控对代理出口极敏感；
    // 咪咕系（migu.cn 全域）：海外出口会被 440001「应版权方要求，该内容
    // 暂无法在当前地区提供服务」拒绝，搜索 / 取链 / CDN 下载统一强制直连；
    // 酷我系（kuwo.cn 全域）：搜索 / BD 取链 / bd-er、kw-bj 等 CDN 都对
    // 海外出口不稳定，且直链 token 短命，走代理会额外增加超时风险。
    const DIRECT_SUFFIXES: [&str; 6] = [
        "163.com",
        "126.net",
        "163cn.tv",
        "netease.cn",
        "migu.cn",
        "kuwo.cn",
    ];
    let lower = url.to_ascii_lowercase();
    // 只看 host 段：去掉 scheme，取第一个 '/' 之前、':' 之前的部分
    let host = lower
        .split_once("://")
        .map_or(lower.as_str(), |(_, rest)| rest)
        .split(['/', '?'])
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    DIRECT_SUFFIXES
        .iter()
        .any(|s| host.ends_with(s) || host == s.trim_start_matches('.'))
}

#[derive(Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
    direct: reqwest::Client,
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new()
    }
}

/// 流式版后 Rust 侧暂无调用方（下载走了 `BlockingHttp` 的流式会话）；
/// 保留给未来的异步下载 / 导出场景。
#[allow(dead_code)]
impl HttpClient {
    pub fn new() -> Self {
        let build = |no_proxy: bool| -> reqwest::Client {
            let mut b = reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(Duration::from_secs(20))
                .connect_timeout(Duration::from_secs(8))
                .pool_max_idle_per_host(8);
            if no_proxy {
                b = b.no_proxy();
            }
            b.build().unwrap_or_else(|_| reqwest::Client::new())
        };
        Self {
            inner: build(false),
            direct: build(true),
        }
    }

    pub fn raw(&self) -> &reqwest::Client {
        &self.inner
    }

    /// 按目标地址选客户端：网易系强制直连，其余走默认（尊重代理环境）。
    pub fn client_for(&self, url: &str) -> &reqwest::Client {
        if is_direct_host(url) {
            &self.direct
        } else {
            &self.inner
        }
    }

    /// 抓取纯文本（网页 / LRC 歌词）。插件走的是同步的 [`BlockingHttp`]，
    /// 这个异步版留给主流程用（例如「从 URL 导入歌词」）。
    #[allow(dead_code)]
    pub async fn get_text(&self, url: &str) -> AppResult<String> {
        let resp = self
            .client_for(url)
            .get(url)
            .send()
            .await
            .map_err(|e| AppError::net(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            return Err(AppError::net(format!("GET {url}: HTTP {}", resp.status())));
        }
        resp.text()
            .await
            .map_err(|e| AppError::net(format!("read body: {e}")))
    }

    /// 带 Range 的探测请求：拿到 Content-Length 与是否支持分片。
    ///
    /// 目前流媒体缓存走「整曲落盘」（symphonia 需要 Seek），
    /// 这条留着给后续的边下边播用，也方便排查某些 CDN 不支持 Range 的问题。
    #[allow(dead_code)]
    pub async fn probe(&self, url: &str) -> AppResult<(u64, bool)> {
        let resp = self
            .client_for(url)
            .head(url)
            .send()
            .await
            .map_err(|e| AppError::net(format!("HEAD {url}: {e}")))?;
        let len = resp
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let ranges = resp
            .headers()
            .get(reqwest::header::ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.contains("bytes"))
            .unwrap_or(false);
        Ok((len, ranges))
    }
}

/// 给插件用的阻塞版本（插件在 spawn_blocking 线程里同步执行）。
pub struct BlockingHttp {
    inner: reqwest::blocking::Client,
    direct: reqwest::blocking::Client,
    /// 流式下载专用：不带总超时（整曲下载可达几十秒），
    /// 只有连接与空闲超时，防止连接挂死时下载线程永远卡住。
    stream_inner: reqwest::blocking::Client,
    stream_direct: reqwest::blocking::Client,
}

impl Default for BlockingHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockingHttp {
    /// 进程级共享实例。
    ///
    /// 插件解析和流式下载以前各自 `new()` 一次，每次调用结束就丢掉连接池，
    /// 于是每首歌都要重新做 TLS 握手（实测冷连接首请求 208ms，复用 25ms）。
    /// 共享一份后，第二个请求起直接走已建立的连接；插件里那发 Range 探测
    /// 还能顺手把播放要用的 CDN 连接预热。
    pub fn shared() -> &'static BlockingHttp {
        static SHARED: std::sync::OnceLock<BlockingHttp> = std::sync::OnceLock::new();
        SHARED.get_or_init(BlockingHttp::new)
    }

    pub fn new() -> Self {
        let build = |no_proxy: bool| -> reqwest::blocking::Client {
            let mut b = reqwest::blocking::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(Duration::from_secs(20))
                .connect_timeout(Duration::from_secs(8));
            if no_proxy {
                b = b.no_proxy();
            }
            b.build().unwrap_or_else(|_| reqwest::blocking::Client::new())
        };
        let build_stream = |no_proxy: bool| -> reqwest::blocking::Client {
            // 不设 client 级总超时：整曲下载可达几十秒。请求层（open_range）
            // 会带 per-request timeout；单连接挂死/断流由流式层的断点续传
            // 与 cancel 检查兜底（reqwest blocking builder 没有 read_timeout）。
            let mut b = reqwest::blocking::Client::builder()
                .user_agent(USER_AGENT)
                .connect_timeout(Duration::from_secs(8));
            if no_proxy {
                b = b.no_proxy();
            }
            b.build().unwrap_or_else(|_| reqwest::blocking::Client::new())
        };
        Self {
            inner: build(false),
            direct: build(true),
            stream_inner: build_stream(false),
            stream_direct: build_stream(true),
        }
    }

    #[allow(dead_code)]
    pub fn raw(&self) -> &reqwest::blocking::Client {
        &self.inner
    }

    /// 按目标地址选客户端：网易系强制直连（登录风控对代理出口极敏感），
    /// 其余保持默认行为。
    pub fn client_for(&self, url: &str) -> &reqwest::blocking::Client {
        if is_direct_host(url) {
            &self.direct
        } else {
            &self.inner
        }
    }

    /// 流式下载专用（无总超时）。策略与 [`Self::client_for`] 一致。
    pub fn stream_client_for(&self, url: &str) -> &reqwest::blocking::Client {
        if is_direct_host(url) {
            &self.stream_direct
        } else {
            &self.stream_inner
        }
    }

    #[allow(dead_code)]
    pub fn get_text(&self, url: &str, headers: &[(String, String)]) -> AppResult<String> {
        let mut req = self.client_for(url).get(url);
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req
            .send()
            .map_err(|e| AppError::net(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            return Err(AppError::net(format!("HTTP {} for {url}", resp.status())));
        }
        resp.text()
            .map_err(|e| AppError::net(format!("read body: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::is_direct_host;

    #[test]
    fn direct_hosts() {
        for u in [
            "https://music.163.com/weapi/login/cellphone",
            "https://clientlog.music.163.com/eapi/feedback/weblog",
            "https://p1.music.126.net/abc.jpg",
            "https://163cn.tv/abc",
            "https://music.163.com:443/x",
            "http://search.kuwo.cn/r.s?all=test",
            "http://bd-api.kuwo.cn/api/play/music/v2/audioUrl?musicId=1",
            "http://bd-er.kuwo.cn/abc/resource/trackmedia/F000.flac",
        ] {
            assert!(is_direct_host(u), "{u} 应直连");
        }
        for u in [
            "https://api.example.com/x",
            "https://music.gdmusic.top/resolve",
            "http://localhost:8899",
        ] {
            assert!(!is_direct_host(u), "{u} 不应直连");
        }
    }
}
