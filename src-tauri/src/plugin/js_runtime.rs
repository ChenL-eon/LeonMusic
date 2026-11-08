//! JS 沙箱运行时（boa_engine，纯 Rust，无 C 依赖，Android 交叉编译友好）。
//!
//! 每次插件调用都会新建一个 `Context`：无跨调用的状态污染，
//! 也不需要处理 `Context` 的 `Send` 问题（boa 的 Context 不是 Send）。

use std::cell::RefCell;
use std::collections::HashMap;

use boa_engine::object::ObjectInitializer;
use boa_engine::property::Attribute;
use boa_engine::{js_string, Context, JsNativeError, JsResult, JsValue, NativeFunction, Source};

use crate::error::{AppError, AppResult};
use crate::net::client::BlockingHttp;

/// 单次插件执行允许的最大循环迭代数，防止 `while(true)`。
const LOOP_LIMIT: u64 = 20_000_000;
/// 单次执行的递归/栈深度上限
const RECURSION_LIMIT: usize = 400;

struct HostState {
    config: HashMap<String, String>,
    http: BlockingHttp,
}

thread_local! {
    static HOST: RefCell<Option<HostState>> = const { RefCell::new(None) };
}

/// 在沙箱里执行 `script`，然后调用全局函数 `func`，参数与返回值都是 JSON。
pub fn call_function(
    script: &str,
    func: &str,
    args: &[serde_json::Value],
    config: HashMap<String, String>,
) -> AppResult<serde_json::Value> {
    let mut ctx = Context::default();

    // 运行时限制：超限会抛异常而不是卡死
    {
        let limits = ctx.runtime_limits_mut();
        limits.set_loop_iteration_limit(LOOP_LIMIT);
        limits.set_recursion_limit(RECURSION_LIMIT);
    }

    install_host(&mut ctx)?;

    HOST.with(|h| {
        *h.borrow_mut() = Some(HostState {
            config: config.clone(),
            http: BlockingHttp::new(),
        });
    });

    let result = (|| -> AppResult<serde_json::Value> {
        ctx.eval(Source::from_bytes(script))
            .map_err(|e| AppError::plugin(format!("eval: {e}")))?;

        let global = ctx.global_object().clone();
        let f = global
            .get(js_string!(func), &mut ctx)
            .map_err(|e| AppError::plugin(format!("lookup {func}: {e}")))?;
        let callable = f
            .as_callable()
            .ok_or_else(|| AppError::plugin(format!("plugin does not export function `{func}`")))?;

        let mut js_args = Vec::with_capacity(args.len());
        for a in args {
            js_args.push(json_to_js(a, &mut ctx)?);
        }

        let ret = callable
            .call(&JsValue::undefined(), &js_args, &mut ctx)
            .map_err(|e| {
                // 完整栈帧进日志，给用户只看第一行
                tracing::warn!("plugin {func} threw: {e}");
                AppError::plugin(format!("{func}: {}", clean_js_error(&e)))
            })?;

        js_to_json(ret, &mut ctx)
    })();

    HOST.with(|h| *h.borrow_mut() = None);
    result
}

fn install_host(ctx: &mut Context) -> AppResult<()> {    let config = ObjectInitializer::new(ctx)
        .function(
            NativeFunction::from_fn_ptr(host_config_get),
            js_string!("get"),
            1,
        )
        .build();

    let http = ObjectInitializer::new(ctx)
        .function(NativeFunction::from_fn_ptr(host_http_get), js_string!("get"), 1)
        .function(
            NativeFunction::from_fn_ptr(host_http_post),
            js_string!("post"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(host_http_post_form),
            js_string!("postForm"),
            3,
        )
        .function(
            NativeFunction::from_fn_ptr(host_http_post_form_ex),
            js_string!("postFormEx"),
            3,
        )
        .function(
            NativeFunction::from_fn_ptr(host_http_get_ex),
            js_string!("getEx"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(host_http_get_raw),
            js_string!("getRaw"),
            2,
        )
        .build();

    // 沙箱里没有 Node 的 crypto。网易云的 weapi/eapi 都要 AES + RSA，
    // 所以把原语放在宿主侧——插件只负责拼协议，不碰算法实现。
    let crypto = ObjectInitializer::new(ctx)
        .function(
            NativeFunction::from_fn_ptr(host_aes_cbc_b64),
            js_string!("aesCbcB64"),
            3,
        )
        .function(
            NativeFunction::from_fn_ptr(host_aes_ecb_hex),
            js_string!("aesEcbHex"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(host_rsa_no_pad_hex),
            js_string!("rsaNoPadHex"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(host_random_b64),
            js_string!("randomB64"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(host_crypto_utf8_decode),
            js_string!("utf8Decode"),
            1,
        )
        .build();

    let leon = ObjectInitializer::new(ctx)
        .property(js_string!("config"), config, Attribute::all())
        .property(js_string!("http"), http, Attribute::all())
        .property(js_string!("crypto"), crypto, Attribute::all())
        .function(NativeFunction::from_fn_ptr(host_log), js_string!("log"), 1)
        .function(NativeFunction::from_fn_ptr(host_md5), js_string!("md5"), 1)
        .function(NativeFunction::from_fn_ptr(host_sha1), js_string!("sha1"), 1)
        .function(
            NativeFunction::from_fn_ptr(host_base64),
            js_string!("base64"),
            1,
        )
        .build();

    ctx.register_global_property(js_string!("leon"), leon, Attribute::all())
        .map_err(|e| AppError::plugin(format!("register host api: {e}")))?;
    Ok(())
}

fn arg_str(args: &[JsValue], idx: usize, ctx: &mut Context) -> String {
    args.get(idx)
        .filter(|v| !v.is_undefined() && !v.is_null())
        .and_then(|v| v.to_string(ctx).ok())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default()
}

fn js_err(msg: impl std::fmt::Display) -> boa_engine::JsError {
    JsNativeError::error().with_message(msg.to_string()).into()
}

fn host_config_get(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let key = arg_str(args, 0, ctx);
    let value = HOST.with(|h| {
        h.borrow()
            .as_ref()
            .and_then(|s| s.config.get(&key).cloned())
            .unwrap_or_default()
    });
    Ok(JsValue::from(boa_engine::JsString::from(value)))
}

fn host_log(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let msg = arg_str(args, 0, ctx);
    tracing::info!(target: "plugin", "{msg}");
    Ok(JsValue::undefined())
}

fn host_http_get(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let url = arg_str(args, 0, ctx);
    if url.is_empty() {
        return Err(js_err("leon.http.get: url is required"));
    }
    // 第二参数（可选）：`{ timeoutMs?: number, headers?: Record<string,string> }`。
    // 解析链里 GD 这类会限流/宕机的后端要短超时快速失败，
    // 否则每首 VIP 歌都要先白等满默认的 20s 才轮到下一个后端。
    let opts = match args.get(1) {
        Some(v) if !v.is_undefined() && !v.is_null() => {
            js_to_json(v.clone(), ctx).unwrap_or(serde_json::Value::Null)
        }
        _ => serde_json::Value::Null,
    };
    let timeout_ms = opts
        .get("timeoutMs")
        .and_then(|v| v.as_u64())
        .filter(|&ms| (500..=30_000).contains(&ms));
    let req_headers = opts
        .get("headers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let result = HOST.with(|h| {
        let guard = h.borrow();
        let Some(state) = guard.as_ref() else {
            return Err("host not ready".to_string());
        };
        let mut req = state.http.client_for(&url).get(&url);
        if let Some(ms) = timeout_ms {
            req = req.timeout(std::time::Duration::from_millis(ms));
        }
        for (k, v) in &req_headers {
            if let Some(s) = v.as_str() {
                req = req.header(k.as_str(), s);
            }
        }
        let resp = req.send().map_err(|e| e.to_string())?;
        resp.text().map_err(|e| e.to_string())
    });
    match result {
        Ok(text) => Ok(JsValue::from(boa_engine::JsString::from(text))),
        Err(e) => Err(js_err(e)),
    }
}

fn host_http_post(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let url = arg_str(args, 0, ctx);
    let body = arg_str(args, 1, ctx);
    if url.is_empty() {
        return Err(js_err("leon.http.post: url is required"));
    }
    let result = HOST.with(|h| {
        let guard = h.borrow();
        let Some(state) = guard.as_ref() else {
            return Err("host not ready".to_string());
        };
        let resp = state
            .http
            .client_for(&url)
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .map_err(|e| e.to_string())?;
        resp.text().map_err(|e| e.to_string())
    });
    match result {
        Ok(text) => Ok(JsValue::from(boa_engine::JsString::from(text))),
        Err(e) => Err(js_err(e)),
    }
}

fn host_md5(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let s = arg_str(args, 0, ctx);
    let digest = md5::compute(s.as_bytes());
    Ok(JsValue::from(boa_engine::JsString::from(format!(
        "{digest:x}"
    ))))
}

fn host_sha1(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    use sha1::{Digest, Sha1};
    let s = arg_str(args, 0, ctx);
    let mut h = Sha1::new();
    h.update(s.as_bytes());
    Ok(JsValue::from(boa_engine::JsString::from(hex::encode(
        h.finalize(),
    ))))
}

fn host_base64(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    use base64::Engine;
    let s = arg_str(args, 0, ctx);
    let out = base64::engine::general_purpose::STANDARD.encode(s.as_bytes());
    Ok(JsValue::from(boa_engine::JsString::from(out)))
}

// ---------------------------------------------------------------------------
// leon.http.postForm
// ---------------------------------------------------------------------------

/// `application/x-www-form-urlencoded` 的 POST。
///
/// 网易云 weapi/eapi 的密文必须放在 form body 里（JSON 不行），
/// 而且要带 `Referer` / `Cookie` 这些头才能拿到正常响应。
/// 第三个参数是普通对象，非字符串值会被忽略。
fn host_http_post_form(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let url = arg_str(args, 0, ctx);
    let body = arg_str(args, 1, ctx);
    if url.is_empty() {
        return Err(js_err("leon.http.postForm: url is required"));
    }
    let headers = match args.get(2) {
        Some(v) if !v.is_undefined() && !v.is_null() => {
            js_to_json(v.clone(), ctx).unwrap_or(serde_json::Value::Null)
        }
        _ => serde_json::Value::Null,
    };

    let result = HOST.with(|h| {
        let guard = h.borrow();
        let Some(state) = guard.as_ref() else {
            return Err("host not ready".to_string());
        };
        let has_ct = has_content_type(&headers);
        let mut req = state.http.client_for(&url).post(&url);
        if !has_ct {
            req = req.header("Content-Type", "application/x-www-form-urlencoded");
        }
        req = req.body(body);
        if let Some(map) = headers.as_object() {
            for (k, v) in map {
                if let Some(s) = v.as_str() {
                    req = req.header(k.as_str(), s);
                }
            }
        }
        let resp = req.send().map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.text().map_err(|e| e.to_string())?;
        // 4xx 也把 body 交回去：网易云的错误原因就在 JSON 里
        if !status.is_success() && text.is_empty() {
            return Err(format!("HTTP {status} for {url}"));
        }
        Ok(text)
    });

    match result {
        Ok(text) => Ok(JsValue::from(boa_engine::JsString::from(text))),
        Err(e) => Err(js_err(e)),
    }
}

// ---------------------------------------------------------------------------
// leon.http.postFormEx
// ---------------------------------------------------------------------------

/// `Set-Cookie: MUSIC_U=xxx; Max-Age=315360000; Path=/; Domain=.music.163.com`
/// 只取第一个分号前的那一段。
fn parse_set_cookie(raw: &str) -> Option<(String, String)> {
    let first = raw.split(';').next()?.trim();
    let (name, value) = first.split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    Some((name.to_string(), value.trim().to_string()))
}

/// 把响应头拆成「普通头」和「cookie 表」。
///
/// `HeaderMap` 的多值头必须用 `get_all` 取：网易云 803 时一次下发 7~8 条
/// `Set-Cookie`（MUSIC_U / __csrf / MUSIC_A / …），只靠 `iter()` 的行为
/// 在不同 http 版本里可能只拿到第一条，而缺了 `MUSIC_U` 就等于没登录。
fn split_response_headers(
    headers: &reqwest::header::HeaderMap,
) -> (
    serde_json::Map<String, serde_json::Value>,
    serde_json::Map<String, serde_json::Value>,
) {
    let mut out = serde_json::Map::new();
    let mut cookies = serde_json::Map::new();
    let mut raw_cookies: Vec<String> = Vec::new();

    for (name, value) in headers.iter() {
        let lower = name.as_str().to_ascii_lowercase();
        if lower == "set-cookie" {
            continue;
        }
        out.insert(
            lower,
            serde_json::Value::String(value.to_str().unwrap_or_default().to_string()),
        );
    }
    for raw in headers.get_all("set-cookie").iter() {
        let value = raw.to_str().unwrap_or_default().to_string();
        if let Some((k, v)) = parse_set_cookie(&value) {
            cookies.insert(k, serde_json::Value::String(v));
        }
        raw_cookies.push(value);
    }
    if !raw_cookies.is_empty() {
        out.insert(
            "set-cookie".to_string(),
            serde_json::Value::String(raw_cookies.join("\n")),
        );
    }
    (out, cookies)
}

/// 调用方是否自己指定了 `Content-Type`（大小写不敏感）。
///
/// `postForm` / `postFormEx` 默认补 `application/x-www-form-urlencoded`，
/// 但 reqwest 的 `RequestBuilder::header` 是**追加**语义：再补一次就变成
/// 两个 Content-Type，服务端按第一个（form）解析 JSON body，酷我解锁
/// 接口因此回 439 sign invalid。所以有自定义头时必须让位。
fn has_content_type(headers: &serde_json::Value) -> bool {
    headers
        .as_object()
        .map(|m| m.keys().any(|k| k.eq_ignore_ascii_case("content-type")))
        .unwrap_or(false)
}

/// 与 [`host_http_post_form`] 发同样的请求，但把**响应头和 Set-Cookie 交回脚本**。
///
/// 扫码登录能成立全靠这个：网易云在 `code=803` 时 body 里只有一句
/// `授权登陆成功`，身份凭据 `MUSIC_U` 走的是 `Set-Cookie`。
/// 只返回 body 的 `postForm` 永远登不进去。
///
/// 返回 `{ status, headers, cookies, body }`：
/// * `headers` —— 小写头名，`set-cookie` 多条用 `\n` 连接
/// * `cookies` —— 已解析成 `{ name: value }`，省得插件自己切分号
fn host_http_post_form_ex(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let url = arg_str(args, 0, ctx);
    let body = arg_str(args, 1, ctx);
    if url.is_empty() {
        return Err(js_err("leon.http.postFormEx: url is required"));
    }
    let req_headers = match args.get(2) {
        Some(v) if !v.is_undefined() && !v.is_null() => {
            js_to_json(v.clone(), ctx).unwrap_or(serde_json::Value::Null)
        }
        _ => serde_json::Value::Null,
    };

    let result = HOST.with(|h| -> Result<serde_json::Value, String> {
        let guard = h.borrow();
        let Some(state) = guard.as_ref() else {
            return Err("host not ready".to_string());
        };
        // 只有调用方没自己指定 Content-Type 时才补默认的 form 编码。
        // 追加语义下（reqwest `header()` 是 append）会同时发出两个
        // Content-Type，酷我 BD 的解锁接口会直接判 439 sign invalid——
        // 2026-11-08《恋人》只出 41 秒试听就是这个原因。
        let mut req = state.http.client_for(&url).post(&url);
        if !has_content_type(&req_headers) {
            req = req.header("Content-Type", "application/x-www-form-urlencoded");
        }
        req = req.body(body);
        if let Some(map) = req_headers.as_object() {
            for (k, v) in map {
                if let Some(s) = v.as_str() {
                    req = req.header(k.as_str(), s);
                }
            }
        }
        let resp = req.send().map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let (headers, cookies) = split_response_headers(resp.headers());

        let text = resp.text().map_err(|e| e.to_string())?;
        Ok(serde_json::json!({
            "status": status,
            "headers": headers,
            "cookies": cookies,
            "body": text,
        }))
    });

    match result {
        Ok(v) => json_to_js(&v, ctx).map_err(|e| js_err(e.detail())),
        Err(e) => Err(js_err(e)),
    }
}

// ---------------------------------------------------------------------------
// leon.http.getEx
// ---------------------------------------------------------------------------

/// `leon.http.getEx(url, headers?)` → `{ status, headers, cookies, body }`。
///
/// 扫码登录前先访问一次首页：真实浏览器会先拿到 `_ntes_nuid` / `NMTID`
/// 等会话 Cookie，再带着它们去 unikey 与轮询。第三方实现常跳过这一步，
/// 服务端只看到没有指纹的裸请求，手机端就报「设备环境异常」。
fn host_http_get_ex(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let url = arg_str(args, 0, ctx);
    if url.is_empty() {
        return Err(js_err("leon.http.getEx: url is required"));
    }
    let req_headers = match args.get(1) {
        Some(v) if !v.is_undefined() && !v.is_null() => {
            js_to_json(v.clone(), ctx).unwrap_or(serde_json::Value::Null)
        }
        _ => serde_json::Value::Null,
    };

    let result = HOST.with(|h| -> Result<serde_json::Value, String> {
        let guard = h.borrow();
        let Some(state) = guard.as_ref() else {
            return Err("host not ready".to_string());
        };
        // 首页只是用来收集 Cookie，不能让它拖慢「点扫码 → 出二维码」：
        // 客户端默认 20s，这里单独压到 8s，超时就退化成无预热会话。
        let mut req = state
            .http
            .client_for(&url)
            .get(&url)
            .timeout(std::time::Duration::from_secs(8));
        if let Some(map) = req_headers.as_object() {
            for (k, v) in map {
                if let Some(s) = v.as_str() {
                    req = req.header(k.as_str(), s);
                }
            }
        }
        let resp = req.send().map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let (headers, cookies) = split_response_headers(resp.headers());
        let text = resp.text().map_err(|e| e.to_string())?;
        Ok(serde_json::json!({
            "status": status,
            "headers": headers,
            "cookies": cookies,
            "body": text,
        }))
    });

    match result {
        Ok(v) => json_to_js(&v, ctx).map_err(|e| js_err(e.detail())),
        Err(e) => Err(js_err(e)),
    }
}

// leon.http.getRaw
// ---------------------------------------------------------------------------

/// `leon.http.getRaw(url, opts?)` → `{ status, body }`。
///
/// `body` 是 latin1 形态字符串：响应的每个字节原样映射为一个 char
/// （0–255），不做任何编码解释。给需要读取**二进制响应**的解析后端用
/// ——咪咕 h5 播放地址接口的响应是加密字节流，`resp.text()` 的
/// UTF-8 lossy 解码会把非法序列替换成 U+FFFD，字节一碎解密必失败。
/// 文本还原走配套的 `leon.crypto.utf8Decode`。
/// `opts`: `{ timeoutMs?: number, headers?: Record<string,string> }`，
/// 与 [`host_http_get`] 同一套路（解析后端要短超时快速轮换）。
fn host_http_get_raw(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let url = arg_str(args, 0, ctx);
    if url.is_empty() {
        return Err(js_err("leon.http.getRaw: url is required"));
    }
    let opts = match args.get(1) {
        Some(v) if !v.is_undefined() && !v.is_null() => {
            js_to_json(v.clone(), ctx).unwrap_or(serde_json::Value::Null)
        }
        _ => serde_json::Value::Null,
    };
    let timeout_ms = opts
        .get("timeoutMs")
        .and_then(|v| v.as_u64())
        .filter(|&ms| (500..=30_000).contains(&ms));
    let req_headers = opts
        .get("headers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let result = HOST.with(|h| {
        let guard = h.borrow();
        let Some(state) = guard.as_ref() else {
            return Err("host not ready".to_string());
        };
        let mut req = state.http.client_for(&url).get(&url);
        if let Some(ms) = timeout_ms {
            req = req.timeout(std::time::Duration::from_millis(ms));
        }
        for (k, v) in &req_headers {
            if let Some(s) = v.as_str() {
                req = req.header(k.as_str(), s);
            }
        }
        let resp = req.send().map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes().map_err(|e| e.to_string())?;
        // byte → char 直映射，1:1 无损；长度大时逐字节 collect 仍是一次分配
        let body: String = bytes.iter().map(|&b| b as char).collect();
        Ok((status, body))
    });
    match result {
        Ok((status, body)) => {
            let obj = serde_json::json!({ "status": status, "body": body });
            json_to_js(&obj, ctx).map_err(|e| js_err(e.detail()))
        }
        Err(e) => Err(js_err(e)),
    }
}

// ---------------------------------------------------------------------------
// leon.crypto.*
// ---------------------------------------------------------------------------

/// `leon.crypto.utf8Decode(latin1)` → String。
/// 把 latin1 字符串（每个 charCode 视为一个字节）按 UTF-8 还原为文本，
/// 无效序列用 U+FFFD 替换。与 `leon.http.getRaw` 配套使用。
fn host_crypto_utf8_decode(
    _this: &JsValue,
    args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let s = arg_str(args, 0, _ctx);
    let bytes: Vec<u8> = s.chars().map(|c| u32::from(c) as u8).collect();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(JsValue::from(boa_engine::JsString::from(text)))
}

fn key16(s: &str, what: &str) -> Result<[u8; 16], String> {
    let b = s.as_bytes();
    if b.len() != 16 {
        return Err(format!("{what} 必须是 16 字节，当前 {} 字节", b.len()));
    }
    let mut out = [0u8; 16];
    out.copy_from_slice(b);
    Ok(out)
}

/// AES-128-CBC/PKCS7 加密，输出 base64。网易云 weapi 的两次加密都用它。
fn aes_cbc_b64(text: &str, key: &str, iv: &str) -> Result<String, String> {
    use base64::Engine;
    use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};

    let key = key16(key, "aesCbcB64 key")?;
    let iv = key16(iv, "aesCbcB64 iv")?;

    let enc = cbc::Encryptor::<aes::Aes128>::new_from_slices(&key, &iv)
        .map_err(|e| format!("aes cbc: {e}"))?;

    let raw = text.as_bytes();
    let mut buf = vec![0u8; raw.len() + 16];
    buf[..raw.len()].copy_from_slice(raw);
    let out = enc
        .encrypt_padded_mut::<Pkcs7>(&mut buf, raw.len())
        .map_err(|e| format!("aes cbc: {e}"))?;

    Ok(base64::engine::general_purpose::STANDARD.encode(out))
}

/// AES-128-ECB/PKCS7 加密，输出 hex。网易云 eapi 用它。
fn aes_ecb_hex(text: &str, key: &str) -> Result<String, String> {
    use aes::cipher::{generic_array::GenericArray, BlockEncrypt, KeyInit};

    let key = key16(key, "aesEcbHex key")?;
    let cipher = aes::Aes128::new_from_slice(&key).map_err(|e| format!("aes ecb: {e}"))?;

    // PKCS7：整块时补满 16 字节，与 Node 的 createCipheriv 默认行为一致
    let pad = 16 - text.len() % 16;
    let mut buf = text.as_bytes().to_vec();
    buf.extend(std::iter::repeat(pad as u8).take(pad));
    for chunk in buf.chunks_mut(16) {
        cipher.encrypt_block(GenericArray::from_mut_slice(chunk));
    }
    Ok(hex::encode(buf))
}

/// RSA/ECB/NoPadding（PKCS#1 v1.5 的裸模幂），输出 hex。
///
/// 网易云 weapi 的 `encSecKey` 就是这个：把 16 字节随机 key 左补零到模长，
/// 直接做 `m^e mod n`，不做任何填充。标准库没有这个模式，只能手算。
fn rsa_no_pad_hex(text: &str, pem: &str) -> Result<String, String> {
    use rsa::pkcs8::DecodePublicKey;
    use rsa::traits::PublicKeyParts;

    let key =
        rsa::RsaPublicKey::from_public_key_pem(pem).map_err(|e| format!("rsa: 公钥解析失败: {e}"))?;
    let n = key.n();
    let e = key.e();
    let size = n.bits().div_ceil(8);

    let data = text.as_bytes();
    if data.len() > size {
        return Err(format!(
            "rsa: 明文 {} 字节超过模长 {size} 字节",
            data.len()
        ));
    }
    let mut buf = vec![0u8; size];
    buf[size - data.len()..].copy_from_slice(data);

    let c = rsa::BigUint::from_bytes_be(&buf).modpow(e, n);
    let out = c.to_bytes_be();
    let mut fixed = vec![0u8; size.saturating_sub(out.len())];
    fixed.extend_from_slice(&out);

    Ok(hex::encode(fixed))
}

fn host_aes_cbc_b64(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let out = aes_cbc_b64(
        &arg_str(args, 0, ctx),
        &arg_str(args, 1, ctx),
        &arg_str(args, 2, ctx),
    )
    .map_err(js_err)?;
    Ok(JsValue::from(boa_engine::JsString::from(out)))
}

fn host_aes_ecb_hex(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let out = aes_ecb_hex(&arg_str(args, 0, ctx), &arg_str(args, 1, ctx)).map_err(js_err)?;
    Ok(JsValue::from(boa_engine::JsString::from(out)))
}

fn host_rsa_no_pad_hex(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let out = rsa_no_pad_hex(&arg_str(args, 0, ctx), &arg_str(args, 1, ctx)).map_err(js_err)?;
    Ok(JsValue::from(boa_engine::JsString::from(out)))
}

/// `n` 字节随机数的 base64。网易云用它生成 weapi 的 secretKey。
fn host_random_b64(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    use base64::Engine;
    use rand::RngCore;

    let n = arg_str(args, 0, ctx)
        .parse::<usize>()
        .unwrap_or(16)
        .clamp(1, 64);
    let mut buf = vec![0u8; n];
    rand::rng().fill_bytes(&mut buf);
    Ok(JsValue::from(boa_engine::JsString::from(
        base64::engine::general_purpose::STANDARD.encode(buf),
    )))
}

fn json_to_js(v: &serde_json::Value, ctx: &mut Context) -> AppResult<JsValue> {
    let text = serde_json::to_string(v)
        .map_err(|e| AppError::plugin(format!("serialize arg: {e}")))?;
    // 括号防止对象字面量被解析成 block
    ctx.eval(Source::from_bytes(&format!("({text})")))
        .map_err(|e| AppError::plugin(format!("build arg: {e}")))
}

fn js_to_json(value: JsValue, ctx: &mut Context) -> AppResult<serde_json::Value> {
    if value.is_undefined() || value.is_null() {
        return Ok(serde_json::Value::Null);
    }
    let global = ctx.global_object().clone();
    let json = global
        .get(js_string!("JSON"), ctx)
        .map_err(|e| AppError::plugin(format!("JSON: {e}")))?;
    let obj = json
        .as_object()
        .ok_or_else(|| AppError::plugin("JSON is not an object"))?;
    let stringify = obj
        .get(js_string!("stringify"), ctx)
        .map_err(|e| AppError::plugin(format!("stringify: {e}")))?;
    let callable = stringify
        .as_callable()
        .ok_or_else(|| AppError::plugin("JSON.stringify missing"))?;
    let out = callable
        .call(&json, &[value], ctx)
        .map_err(|e| AppError::plugin(format!("stringify call: {e}")))?;
    if out.is_undefined() {
        return Ok(serde_json::Value::Null);
    }
    let text = out
        .to_string(ctx)
        .map_err(|e| AppError::plugin(format!("stringify result: {e}")))?
        .to_std_string_escaped();
    serde_json::from_str(&text).map_err(|e| AppError::plugin(format!("plugin returned non-json: {e}")))
}

/// 把 boa 的异常压成一句人话。
///
/// boa 的 `Display` 会带完整栈帧，形如：
/// `Error: 未配置服务器地址 (unknown at :52:17)\n    at base (unknown at :9:19)...`
/// 插件作者看日志有用，但音乐播放器不该把这种东西弹给用户。
/// 这里只取第一行，去掉 `Error: ` 前缀与行尾的 `(unknown at :行:列)`。
fn clean_js_error(err: &boa_engine::JsError) -> String {
    let full = err.to_string();
    let first = full.lines().next().unwrap_or("").trim();
    let first = first.strip_prefix("Error: ").unwrap_or(first);
    let cut = first.rfind(" (unknown at").unwrap_or(first.len());
    let msg = first[..cut].trim();
    if msg.is_empty() {
        full
    } else {
        msg.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 期望值全部由 Node 的 crypto 以同样的参数算出：
    //   createCipheriv('aes-128-cbc' | 'aes-128-ecb')
    //   publicEncrypt({padding: RSA_NO_PADDING})
    // 只要这三组对得上，weapi/eapi 的密文就能被服务端解开。
    const IV: &str = "0102030405060708";
    const PRESET_KEY: &str = "0CoJUm6Qyw8W8jud";
    const EAPI_KEY: &str = "e82ckenh8dichen8";

    const PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----\n\
MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDgtQn2JZ34ZC28NWYpAUd98iZ3\n\
7BUrX/aKzmFbt7clFSs6sXqHauqKWqdtLkF2KexO40H1YTX8z2lSgBBOAxLsvakl\n\
V8k4cBFK9snQXE9/DDaFt6Rr7iVZMldczhC0JNgTz+SHXT6CBHuX3e9SdB1Ua44o\n\
ncaTWz7OBGLbCiK45wIDAQAB\n\
-----END PUBLIC KEY-----";

    #[test]
    fn aes_cbc_matches_node() {
        assert_eq!(
            aes_cbc_b64(r#"{"s":"test"}"#, PRESET_KEY, IV).unwrap(),
            "Uqdn0KDBPcFL8vcKU50rcA=="
        );
        // 不足一块也要补齐
        assert_eq!(
            aes_cbc_b64("abc", PRESET_KEY, IV).unwrap(),
            "MQ25khFIIoBfKBIBWskbpg=="
        );
    }

    #[test]
    fn aes_ecb_matches_node() {
        assert_eq!(
            aes_ecb_hex(
                r#"nobody/api/song/enhance/player/url/v1use{"ids":"[1]"}md5forencrypt"#,
                EAPI_KEY
            )
            .unwrap(),
            "654a8103644da656ceff7db1a516166ee062accbe59ee1472830dc7429d6a09\
             024b513123a069dffae1767affebc207a041df2ff8cbce58027d57e9b2e2a9a\
             e7f6a4cb900d7c6200629e35a54cb54b90"
                .replace(' ', "")
        );
        // 空串 → 一个完整填充块，这是最容易写错的分支
        assert_eq!(
            aes_ecb_hex("", EAPI_KEY).unwrap(),
            "6aa3b102fbe7296ab0db9ea5c46ad12b"
        );
    }

    #[test]
    fn rsa_no_padding_matches_node() {
        assert_eq!(
            rsa_no_pad_hex("0123456789abcdef", PUBLIC_KEY).unwrap(),
            "ac744caab466f1fd5a75228365d15df7add288e8982a2c8f80da5f8f22d54834\
             b5e1eab04dde9dac7341851a98de37463d8ba01d9e9c4d2c546c0f948e3163c6\
             67785a1b9c4f160305b0fb1ec3b698de0c704719b2fbc469582654b9c2595317\
             dc40c3ecee32baa6d9753970b66667c01bde9ad34048fa8b2b0f628e56a5bf49"
                .replace(' ', "")
        );
    }

    #[test]
    fn rejects_bad_key_length() {
        assert!(aes_cbc_b64("x", "short", IV).is_err());
        assert!(aes_ecb_hex("x", "0123456789abcdefg").is_err());
    }

    /// 扫码登录能不能成，全看这行解析。
    /// 真实响应长这样（网易云 803 时一次下发好几条）：
    /// `MUSIC_U=00ABCD...; Max-Age=315360000; Expires=...; Path=/; Domain=.music.163.com`
    #[test]
    fn parses_set_cookie() {
        assert_eq!(
            parse_set_cookie(
                "MUSIC_U=00ABC%2Bdef; Max-Age=315360000; Expires=Sun, 02 Nov 2036 18:08:43 GMT; \
                 Path=/; Domain=.music.163.com"
            ),
            Some(("MUSIC_U".into(), "00ABC%2Bdef".into()))
        );
        // 值里带 = 的情况（base64 padding）
        assert_eq!(
            parse_set_cookie("__csrf=abc==; Path=/"),
            Some(("__csrf".into(), "abc==".into()))
        );
        // 兜底：畸形输入不 panic
        assert_eq!(parse_set_cookie(""), None);
        assert_eq!(parse_set_cookie("NOVALUE; Path=/"), None);
        assert_eq!(parse_set_cookie("=x; Path=/"), None);
    }

    /// 803 一次下发多条 Set-Cookie，少收一条 MUSIC_U 就等于没登录。
    #[test]
    fn collects_every_set_cookie() {
        use reqwest::header::{HeaderMap, HeaderValue};

        let mut h = HeaderMap::new();
        h.append(
            "set-cookie",
            HeaderValue::from_static("MUSIC_U=00ABC==; Max-Age=315360000; Path=/"),
        );
        h.append("set-cookie", HeaderValue::from_static("__csrf=xyz; Path=/"));
        h.append("set-cookie", HeaderValue::from_static("NMTID=nope; Path=/"));
        h.insert(
            "content-type",
            HeaderValue::from_static("application/json;charset=UTF-8"),
        );
        let (headers, cookies) = split_response_headers(&h);
        assert_eq!(cookies["MUSIC_U"], "00ABC==");
        assert_eq!(cookies["__csrf"], "xyz");
        assert_eq!(cookies["NMTID"], "nope");
        assert_eq!(cookies.len(), 3, "同名多值头被 iter() 吃掉了");
        assert_eq!(headers["set-cookie"].as_str().unwrap().lines().count(), 3);
        assert_eq!(headers["content-type"], "application/json;charset=UTF-8");
    }

    /// 插件传了自己的 Content-Type 时，宿主不能再补 form 编码。
    /// 回归：2026-11-08 酷我解锁接口收到两个 Content-Type → 439，
    /// 导致《恋人》等 VIP 曲一律回落到 41 秒试听。
    #[test]
    fn caller_content_type_is_not_duplicated() {
        // has_content_type = 「调用方自带」，为 false 时才由宿主补 form 编码
        assert!(!has_content_type(&serde_json::Value::Null), "无头对象应补默认");
        assert!(!has_content_type(&serde_json::json!({})), "空对象应补默认");
        assert!(
            !has_content_type(&serde_json::json!({ "plat": "ar", "devid": "1" })),
            "只有业务头时应补默认"
        );
        assert!(
            has_content_type(&serde_json::json!({ "content-type": "application/json" })),
            "小写 content-type"
        );
        assert!(
            has_content_type(&serde_json::json!({ "Content-Type": "application/json" })),
            "常见大小写混写"
        );
        assert!(
            has_content_type(&serde_json::json!({ "CONTENT-TYPE": "text/plain" })),
            "全大写"
        );
    }

    /// 插件脚本本身要能被 boa 解析，且暴露出约定的入口。
    /// 这条能挡住「改 JS 时漏了逗号但打包时才发现」。
    #[test]
    fn netease_script_exposes_protocol() {
        let script = include_str!("builtin/netease/index.js");
        let mut ctx = Context::default();
        ctx.eval(Source::from_bytes(script)).expect("脚本应可解析");
        let global = ctx.global_object().clone();
        for name in [
            "search",
            "getStreamUrl",
            "getLyric",
            "loginQrCreate",
            "loginQrPoll",
            "loginPollState",
            "loginInfo",
            "mergeCookies",
            "levelFallback",
            "unblockBr",
            "formatFromUrl",
            "qualityFromOfficial",
            "streamResult",
            "unblockOn",
            "isTrialUrlItem",
            "miguResolve",
            // 2.18.0：补位解析后端（酷我 BD / GD）
            "bodianResolve",
            "gdResolve",
            "kuwoMatch",
            // 2.19.0：低规格不收工，按档位判断是否继续问下一个源
            "qualityEnough",
            // 2.14.0：红心 / 排行榜 / 私人FM / 相似歌曲
            "likeTrack",
            "isLikedTrack",
            "getToplists",
            "getPersonalFm",
            "getSimilarSongs",
        ] {
            let v = global.get(js_string!(name), &mut ctx).expect("lookup");
            assert!(v.is_callable(), "缺少入口函数 {name}");
        }
    }

    /// 2.18.0 补位后端：bodian 签名必须在 boa 里跑得通。
    /// 这条同时锁住 Date.now / String.prototype.sort / leon.md5 三个依赖——
    /// 任何一个在 JS 引擎里缺失，签名就是错的，接口会直接 20018。
    #[test]
    fn netease_bodian_sign_in_boa() {
        let script = include_str!("builtin/netease/index.js");
        let url = "http://bd-api.kuwo.cn/api/play/music/v2/audioUrl?&br=2000kflac&musicId=228908";
        let v = call_function(script, "bodianSign", &[serde_json::json!(url)], HashMap::new())
            .expect("bodianSign 执行失败");
        let signed = v.as_str().expect("签名结果应是字符串");
        assert!(signed.starts_with(url), "签名 URL 应以原 URL 开头: {signed}");
        let sign = signed.rsplit("&sign=").next().unwrap_or_default();
        assert_eq!(sign.len(), 32, "md5 应是 32 位 hex: {sign}");
        assert!(sign.chars().all(|c| c.is_ascii_hexdigit()), "md5 非 hex: {sign}");
    }

    /// 真联网：在 boa 里跑完整补位链，验证「宿主实现差异」不会被引擎吞掉。
    ///
    /// 2026-11-08 用户实测《恋人》（网易 2600493765，VIP 版权曲）只出 41 秒
    /// 试听，而 Node 侧同一份 JS 能拿到 53MB FLAC——差异只能出在宿主
    /// （`leon.http.*` / `leon.md5` / `Date.now`）这一层。这条测试把链路
    /// 搬回真实宿主执行，默认 ignore，手动跑：
    /// `cargo test --lib -- --ignored netease_resolve_live`
    #[test]
    #[ignore]
    fn netease_resolve_live_in_boa() {
        let script = include_str!("builtin/netease/index.js");
        let mut cfg = HashMap::new();
        cfg.insert("unblock".to_string(), "on".to_string());
        cfg.insert("unblockQuality".to_string(), "999".to_string());
        let track = serde_json::json!({
            "id": "2600493765",
            "title": "恋人",
            "artist": "李荣浩",
            "durationMs": 276000
        });
        for name in ["kuwoSearch", "bodianResolve", "resolveTrack"] {
            let t0 = std::time::Instant::now();
            // kuwoSearch 吃查询字符串，另两个吃 track 对象
            let arg = if name == "kuwoSearch" {
                serde_json::json!("恋人 李荣浩")
            } else {
                track.clone()
            };
            let r = call_function(script, name, &[arg], cfg.clone());
            match r {
                Ok(v) => {
                    let s = v.to_string();
                    println!(
                        "{name} ({}ms) => {}",
                        t0.elapsed().as_millis(),
                        s.chars().take(300).collect::<String>()
                    );
                }
                Err(e) => println!("{name} ({}ms) ERR {e}", t0.elapsed().as_millis()),
            }
        }
        let v = call_function(script, "resolveTrack", &[track], cfg).expect("resolveTrack 执行失败");
        assert!(
            v.get("url").and_then(|u| u.as_str()).is_some(),
            "补位链没拿到直链: {v}"
        );
    }

    /// kuwoSearch 空查询不走网络，直接返回空数组（引擎里跑通参数校验分支）。
    #[test]
    fn netease_kuwo_search_empty() {
        let script = include_str!("builtin/netease/index.js");
        let v = call_function(script, "kuwoSearch", &[serde_json::json!("")], HashMap::new())
            .expect("kuwoSearch 执行失败");
        assert!(v.is_array() && v.as_array().unwrap().is_empty());
    }

    /// 音质档位：选中档优先、只向下逐档降级；非法值回落默认 exhigh。
    /// 这条锁住「选了标准却去请求高清臻音」这类回归。
    #[test]
    fn netease_quality_levels() {
        let script = include_str!("builtin/netease/index.js");
        let levels = |want: &str| {
            call_function(
                script,
                "levelFallback",
                &[serde_json::json!(want)],
                HashMap::new(),
            )
            .expect("levelFallback 调用失败")
        };
        assert_eq!(
            levels("jymaster"),
            serde_json::json!(["jymaster", "hires", "lossless", "exhigh", "higher", "standard"])
        );
        assert_eq!(
            levels("lossless"),
            serde_json::json!(["lossless", "exhigh", "higher", "standard"])
        );
        assert_eq!(
            levels("exhigh"),
            serde_json::json!(["exhigh", "higher", "standard"])
        );
        assert_eq!(levels("standard"), serde_json::json!(["standard"]));
        assert_eq!(levels("bogus"), levels("exhigh"), "非法档位要回落默认 exhigh");
    }

    /// 音质档位 → 解析请求码率，以及「实际规格」的降级判定。
    /// 官方 eapi 对 lossless/hires/jymaster 会 code 200 静默返回 320k MP3，
    /// 必须能把这件事暴露给播放条，不能只报「已设为无损」。
    #[test]
    fn netease_stream_quality_metadata() {
        let script = include_str!("builtin/netease/index.js");
        let call = |name: &str, args: &[serde_json::Value], cfg: HashMap<String, String>| {
            call_function(script, name, args, cfg).expect("纯函数调用失败")
        };

        let br = |cfg: HashMap<String, String>| {
            call("unblockBr", &[], cfg).as_str().unwrap_or_default().to_string()
        };
        let mut lossless = HashMap::new();
        lossless.insert("level".into(), "lossless".into());
        let mut higher = HashMap::new();
        higher.insert("level".into(), "higher".into());
        let mut manual = lossless.clone();
        manual.insert("unblockQuality".into(), "320".into());
        assert_eq!(br(lossless), "999", "选无损档要向解析服务请求 FLAC");
        assert_eq!(br(higher), "192");
        assert_eq!(br(HashMap::new()), "320");
        assert_eq!(br(manual), "320", "高级设置里手动指定的码率优先");

        assert_eq!(call("formatFromUrl", &[serde_json::json!("https://x/y.flac?k=1")], HashMap::new()), serde_json::json!("flac"));
        assert_eq!(call("formatFromUrl", &[serde_json::json!("https://x/y.mp3?k=1")], HashMap::new()), serde_json::json!("mp3"));

        let official = |item: serde_json::Value, requested: &str, used: &str| {
            call(
                "qualityFromOfficial",
                &[item, serde_json::json!(requested), serde_json::json!(used)],
                HashMap::new(),
            )
        };
        let silent = official(
            serde_json::json!({ "url": "https://x/y.mp3", "br": 320000, "type": "mp3" }),
            "lossless",
            "lossless",
        );
        assert_eq!(silent["downgraded"].as_bool(), Some(true), "静默降级必须可见");
        assert_eq!(silent["format"].as_str(), Some("mp3"));
        let real = official(
            serde_json::json!({ "url": "https://x/y.flac", "br": 999000, "type": "flac" }),
            "lossless",
            "lossless",
        );
        assert_eq!(real["downgraded"].as_bool(), Some(false));
        let stepped = official(
            serde_json::json!({ "url": "https://x/y.mp3", "br": 128000, "type": "mp3" }),
            "exhigh",
            "standard",
        );
        assert_eq!(stepped["downgraded"].as_bool(), Some(true), "逐档降级也要标出来");
        let plain = official(
            serde_json::json!({ "url": "https://x/y.mp3", "br": 320000, "type": "mp3" }),
            "exhigh",
            "exhigh",
        );
        assert_eq!(plain["downgraded"].as_bool(), Some(false));
    }

    /// 8821 / 设备环境异常是账号级风控，旧实现把它当 waiting，
    /// 界面会永远停在「打开手机 App 扫一扫」。这里锁住状态映射。
    #[test]
    fn netease_login_state_mapping() {
        let script = include_str!("builtin/netease/index.js");
        let state = |code: i64, msg: &str| {
            call_function(
                script,
                "loginPollState",
                &[
                    serde_json::json!(code),
                    serde_json::json!({ "code": code, "message": msg }),
                ],
                HashMap::new(),
            )
            .expect("状态映射不应联网")
        };
        assert_eq!(state(801, "")["state"].as_str(), Some("waiting"));
        assert_eq!(state(802, "")["state"].as_str(), Some("scanned"));
        assert_eq!(state(800, "")["state"].as_str(), Some("expired"));

        let risk = state(8821, "请切换其他登录方式或升级新版本再试");
        assert_eq!(risk["state"].as_str(), Some("blocked"));
        assert!(risk["message"].as_str().unwrap_or("").contains("风控"));

        let device = state(400, "检测到当前设备环境异常，本次操作已拦截");
        assert_eq!(device["state"].as_str(), Some("blocked"));
        assert!(device["message"].as_str().unwrap_or("").contains("风控"));

        let other = state(500, "服务异常");
        assert_eq!(other["state"].as_str(), Some("blocked"));
        assert!(other["message"].as_str().unwrap_or("").contains("500"));
    }

    /// 打卡日志协议形状：startplay 进「最近播放」，play 驱动排行/听歌时长。
    /// 老 weapi 打卡接口已被服务端静默丢弃（hana-music-api#31），这里锁住
    /// clientlog eapi 的字段形态，防止以后改坏又不自知。
    #[test]
    fn netease_scrobble_logs() {
        let script = include_str!("builtin/netease/index.js");
        let logs = call_function(
            script,
            "scrobbleLogs",
            &[
                serde_json::json!("186016"),
                serde_json::json!(240),
                serde_json::json!(0),
                serde_json::json!("list"),
            ],
            HashMap::new(),
        )
        .expect("日志组装不应联网");
        let arr = logs.as_array().expect("logs 应是数组");
        assert_eq!(arr.len(), 2, "startplay + play 两条");
        assert_eq!(arr[0]["action"].as_str(), Some("startplay"));
        assert_eq!(arr[0]["json"]["id"].as_str(), Some("186016"));
        assert_eq!(arr[1]["action"].as_str(), Some("play"));
        assert_eq!(arr[1]["json"]["end"].as_str(), Some("playend"));
        assert_eq!(arr[1]["json"]["id"].as_str(), Some("186016"));
        assert_eq!(arr[1]["json"]["time"].as_i64(), Some(240), "整数秒");
        assert_eq!(arr[1]["json"]["source"].as_str(), Some("list"));
    }

    /// 未登录（Cookie 无 MUSIC_U）时打卡要提前拒绝且不发网络请求。
    #[test]
    fn netease_scrobble_requires_login() {
        let script = include_str!("builtin/netease/index.js");
        let r = call_function(
            script,
            "scrobble",
            &[serde_json::json!("186016"), serde_json::json!(240_000)],
            HashMap::new(),
        )
        .expect("未登录打卡应提前返回");
        assert_eq!(r["ok"].as_bool(), Some(false));
        assert_eq!(r["code"].as_i64(), Some(301));
    }

    /// 验证码登录的请求体协议形状锁（对齐 api-enhanced 的
    /// captcha_sent / login_cellphone 实现，防止以后改坏字段名）。
    #[test]
    fn netease_captcha_body_shape() {
        let script = include_str!("builtin/netease/index.js");
        let sent = call_function(
            script,
            "captchaSentBody",
            &[serde_json::json!("13800001234"), serde_json::json!("86")],
            HashMap::new(),
        )
        .expect("发码请求体不应联网");
        assert_eq!(sent["cellphone"].as_str(), Some("13800001234"));
        assert_eq!(sent["ctcode"].as_str(), Some("86"));
        assert_eq!(sent["secrete"].as_str(), Some("music_middleuser_pclogin"));

        let login = call_function(
            script,
            "loginCellphoneBody",
            &[
                serde_json::json!("13800001234"),
                serde_json::json!("123456"),
                serde_json::json!("86"),
            ],
            HashMap::new(),
        )
        .expect("登录请求体不应联网");
        assert_eq!(login["phone"].as_str(), Some("13800001234"), "新版字段名是 phone 不是 cellphone");
        assert_eq!(login["captcha"].as_str(), Some("123456"));
        assert_eq!(login["countrycode"].as_str(), Some("86"));
        assert_eq!(login["remember"].as_str(), Some("true"));
        assert_eq!(login["type"].as_str(), Some("1"));
        assert_eq!(login["https"].as_str(), Some("true"));
        assert_eq!(login["secureCaptcha"].as_str(), Some(""));
    }

    /// 验证码登录失败码翻译：502（验证码错误/过期）要有可读提示。
    #[test]
    fn netease_cellphone_fail_state() {
        let script = include_str!("builtin/netease/index.js");
        let r = call_function(
            script,
            "cellphoneFailState",
            &[serde_json::json!({"code": 502, "message": "登录密码错误"})],
            HashMap::new(),
        )
        .expect("纯函数不应联网");
        assert_eq!(r["state"].as_str(), Some("failed"));
        let msg = r["message"].as_str().unwrap_or("");
        assert!(msg.contains("验证码"), "502 应翻译成验证码相关提示: {msg}");

        // 实测（2026-11-06 真机探针）：错验证码 = code 503 + message「验证码错误」，
        // 走通用路径直接透出服务端原文即可，不吞不猜。
        let real = call_function(
            script,
            "cellphoneFailState",
            &[serde_json::json!({"code": 503, "message": "验证码错误"})],
            HashMap::new(),
        )
        .expect("纯函数不应联网");
        assert_eq!(real["state"].as_str(), Some("failed"));
        let real_msg = real["message"].as_str().unwrap_or("");
        assert!(real_msg.contains("503") && real_msg.contains("验证码错误"), "503 应透出服务端原文: {real_msg}");

        // 8810 = 网络环境级风控（用户真机登录实测，2026-11-06）：给可行动的话术。
        let risk = call_function(
            script,
            "cellphoneFailState",
            &[serde_json::json!({"code": 8810, "message": "您当前的网络环境存在安全风险"})],
            HashMap::new(),
        )
        .expect("纯函数不应联网");
        let risk_msg = risk["message"].as_str().unwrap_or("");
        assert!(
            risk_msg.contains("8810") && risk_msg.contains("代理"),
            "8810 应提示关代理/换网络: {risk_msg}"
        );

        // -462 = 要滑块人机验证（本应用做不了）：引导改走扫码 / Cookie。
        let verify = call_function(
            script,
            "cellphoneFailState",
            &[serde_json::json!({"code": -462, "message": "需要安全验证"})],
            HashMap::new(),
        )
        .expect("纯函数不应联网");
        let verify_msg = verify["message"].as_str().unwrap_or("");
        assert!(
            verify_msg.contains("扫码") && verify_msg.contains("Cookie"),
            "-462 应引导扫码/Cookie: {verify_msg}"
        );
    }

    /// 登录链路必须是**纯 web 指纹**：UA 是浏览器，anonCookie 就不能再混入
    /// os=osx / appver 这类客户端身份（UA 与 os 不自洽会被风控按异常环境
    /// 处理，参考 api-enhanced 易盾文档「UA 与 OS 不一致」一节）。
    #[test]
    fn netease_login_chain_pure_web_fingerprint() {
        let script = include_str!("builtin/netease/index.js");
        let anon = call_function(script, "anonCookie", &[], HashMap::new()).expect("纯函数不应联网");
        assert_eq!(anon.as_str().unwrap_or("x"), "", "anonCookie 应为空（纯 web 会话）");
    }

    /// 真机探针：设 LEON_TEST_CAPTCHA_PHONE 才跑。验证码登录接口对
    /// 「错误验证码」返回可解析的失败态（登录请求不发短信，安全）。
    #[test]
    fn netease_captcha_login_probe_live() {
        let phone = match std::env::var("LEON_TEST_CAPTCHA_PHONE") {
            Ok(v) if v.len() >= 8 => v,
            _ => return,
        };
        let script = include_str!("builtin/netease/index.js");
        let r = call_function(
            script,
            "loginCellphone",
            &[
                serde_json::json!(phone),
                serde_json::json!("000000"),
                serde_json::json!("86"),
                serde_json::json!(""),
            ],
            HashMap::new(),
        )
        .expect("探针请求执行失败");
        println!(
            "[captcha probe] state={} code={:?} message={:?}",
            r["state"].as_str().unwrap_or(""),
            r["code"].as_i64(),
            r["message"].as_str()
        );
        assert_eq!(r["state"].as_str(), Some("failed"), "错验证码应失败: {r}");
        assert!(
            r["message"].as_str().unwrap_or("").len() > 0,
            "失败要带可读信息: {r}"
        );
    }

    /// 真账号端到端：设 LEON_TEST_NETEASE_COOKIE（含 MUSIC_U）才跑。
    /// 打卡计入排行有 10–30 分钟延迟，这里只验证 clientlog 返回 code 200。
    #[test]
    fn netease_scrobble_live() {
        let cookie = match std::env::var("LEON_TEST_NETEASE_COOKIE") {
            Ok(v) if v.contains("MUSIC_U=") => v,
            _ => return, // 未提供登录 Cookie，跳过
        };
        let script = include_str!("builtin/netease/index.js");
        let mut config = HashMap::new();
        config.insert("cookie".to_string(), cookie);
        // 搜索是匿名的，不依赖登录态的新鲜度；打卡本身用 Cookie 配置
        let results = call_function(
            script,
            "search",
            &[
                serde_json::json!("晴天"),
                serde_json::json!(1),
                serde_json::json!(5),
            ],
            config.clone(),
        )
        .expect("搜索失败");
        let id = results[0]["id"]
            .as_str()
            .map(|s| s.to_string())
            .or_else(|| results[0]["id"].as_i64().map(|v| v.to_string()))
            .expect("搜索结果缺 id");
        let r = call_function(
            script,
            "scrobble",
            &[serde_json::Value::String(id), serde_json::json!(240_000)],
            config,
        )
        .expect("打卡请求执行失败");
        assert_eq!(r["ok"].as_bool(), Some(true), "打卡应成功: {r}");
        assert_eq!(r["code"].as_i64(), Some(200), "clientlog 应返回 200: {r}");
    }

    /// 会话 Cookie 要能合并、去重，并容忍用户粘贴时带 "Cookie: " 与换行。
    #[test]
    fn netease_cookie_merge() {
        let script = include_str!("builtin/netease/index.js");
        let merged = call_function(
            script,
            "mergeCookies",
            &[
                serde_json::json!("os=pc; deviceId=old"),
                serde_json::json!("Cookie: __csrf=abc\nNMTID=xyz; os=pc"),
            ],
            HashMap::new(),
        )
        .expect("Cookie 合并不应联网");
        let s = merged.as_str().unwrap_or_default();
        assert!(s.contains("deviceId=old"));
        assert!(s.contains("__csrf=abc"));
        assert!(s.contains("NMTID=xyz"));
        assert_eq!(s.matches("os=pc").count(), 1, "同名项应被覆盖而不是重复");
    }

    /// 官方 eapi 的试听判定不能靠猜：免费曲 freeTrialInfo=null / 320k，
    /// VIP 曲是 {start:0,end:30} / 128k / time=30040。判反了会要么把
    /// 30 秒当成功（不解析），要么每首免费曲都白打一次第三方服务。
    #[test]
    fn netease_trial_detection() {
        let script = include_str!("builtin/netease/index.js");
        let check = |item: serde_json::Value| {
            call_function(script, "isTrialUrlItem", &[item], HashMap::new())
                .expect("判定时不应联网")
                .as_bool()
        };
        assert_eq!(
            check(serde_json::json!({
                "url": "http://m702.music.126.net/x.mp3",
                "br": 128012, "size": 481115, "time": 30040,
                "freeTrialInfo": { "start": 0, "end": 30 }
            })),
            Some(true)
        );
        assert_eq!(
            check(serde_json::json!({
                "url": "http://m701.music.126.net/y.mp3",
                "br": 320000, "size": 9584893, "time": 240000,
                "freeTrialInfo": null
            })),
            Some(false)
        );
        // 没有 freeTrialInfo 但码率/体积明显是片段时也要认出来
        assert_eq!(
            check(serde_json::json!({ "br": 128000, "size": 470000 })),
            Some(true)
        );
        // 开关：空配置=开启，只有显式 off 才关
        let on = call_function(script, "unblockOn", &[], HashMap::new()).unwrap();
        assert_eq!(on.as_bool(), Some(true));
        let mut off = HashMap::new();
        off.insert("unblock".to_string(), "off".to_string());
        let off_value = call_function(script, "unblockOn", &[], off).unwrap();
        assert_eq!(off_value.as_bool(), Some(false));
    }

    /// 真联网跑一遍搜索 → 直链 → 歌词。
    /// 默认跳过（CI 不该依赖网易云可用性），手动执行：
    /// `cargo test --lib netease_live -- --ignored --nocapture`
    #[test]
    #[ignore = "需要联网访问 music.163.com"]
    fn netease_live_roundtrip() {
        let script = include_str!("builtin/netease/index.js");
        let cfg = || HashMap::new();

        let found = call_function(
            script,
            "search",
            &[serde_json::json!("海阔天空"), serde_json::json!(1), serde_json::json!(3)],
            cfg(),
        )
        .expect("搜索失败");
        let list = found.as_array().expect("搜索结果应是数组");
        assert!(!list.is_empty(), "搜索结果为空");
        println!("search -> {}", serde_json::to_string_pretty(&list[0]).unwrap());

        let track = &list[0];
        assert!(!track["title"].as_str().unwrap_or("").is_empty());
        assert!(track["durationMs"].as_i64().unwrap_or(0) > 0);

        let stream = call_function(script, "getStreamUrl", &[track.clone()], cfg())
            .expect("取直链失败");
        let url = stream["url"].as_str().unwrap_or_default();
        println!(
            "stream -> {url}\nquality -> source={} format={} downgraded={}",
            stream["source"].as_str().unwrap_or("-"),
            stream["format"].as_str().unwrap_or("-"),
            stream["downgraded"].as_bool().unwrap_or(false)
        );
        assert!(url.starts_with("http"), "直链不是 http: {url}");

        let lrc = call_function(script, "getLyric", &[track.clone()], cfg())
            .expect("取歌词失败");
        let lrc = lrc.as_str().unwrap_or_default();
        println!("lyric -> {} 字节", lrc.len());
        assert!(lrc.contains('['), "歌词不像 LRC");
    }

    /// 真联网验证扫码登录的「建码 + 轮询」两段（没有手机扫码，只能验到 801）。
    /// `cargo test --lib netease_login_live -- --ignored --nocapture`
    #[test]
    #[ignore = "需要联网访问 music.163.com"]
    fn netease_login_live() {
        let script = include_str!("builtin/netease/index.js");

        let qr = call_function(script, "loginQrCreate", &[], HashMap::new()).expect("建码失败");
        let key = qr["key"].as_str().unwrap_or_default().to_string();
        let url = qr["url"].as_str().unwrap_or_default().to_string();
        let session = qr["session"].as_str().unwrap_or_default().to_string();
        println!("qr -> {url}\nsession cookies -> {session}");
        assert!(!key.is_empty(), "unikey 为空");
        assert!(url.starts_with("https://music.163.com/login?codekey="));

        let state = call_function(
            script,
            "loginQrPoll",
            &[serde_json::json!(key), serde_json::json!(session)],
            HashMap::new(),
        )
        .expect("轮询失败");
        println!("poll -> {state}");
        assert_eq!(state["state"].as_str(), Some("waiting"));

        // 未登录时账号接口应该是 loggedIn=false，而不是报错
        let info = call_function(script, "loginInfo", &[], HashMap::new()).expect("账号信息失败");
        println!("account -> {info}");
        assert_eq!(info["loggedIn"].as_bool(), Some(false));
    }

    /// 真联网验证 2.14.0 四个新接口（匿名态）：排行榜 / 私人FM / 相似歌曲
    /// 可直接用，红心匿名应返回 ok:false + code:301（不抛异常）。
    /// `cargo test --lib netease_four_api_live -- --ignored --nocapture`
    #[test]
    #[ignore = "需要联网访问 music.163.com"]
    fn netease_four_api_live() {
        let script = include_str!("builtin/netease/index.js");

        let tops = call_function(script, "getToplists", &[], HashMap::new()).expect("排行榜失败");
        let tops = tops.as_array().expect("排行榜应是数组");
        println!("toplists -> {} 项，第一项：{}", tops.len(), tops[0]);
        assert!(!tops.is_empty(), "排行榜为空");
        assert!(!tops[0]["id"].as_str().unwrap_or("").is_empty());
        assert!(!tops[0]["name"].as_str().unwrap_or("").is_empty());

        let fm = call_function(script, "getPersonalFm", &[], HashMap::new()).expect("私人FM失败");
        let fm = fm.as_array().expect("私人FM应是数组");
        println!("personalFm -> {} 首，第一首：{}", fm.len(), fm[0]);
        assert!(!fm.is_empty(), "私人FM为空（匿名也应返回 1 首试听）");

        let simi = call_function(
            script,
            "getSimilarSongs",
            &[serde_json::json!("347230"), serde_json::json!(5)],
            HashMap::new(),
        )
        .expect("相似歌曲失败");
        let simi = simi.as_array().expect("相似歌曲应是数组");
        println!("similarSongs -> {} 首", simi.len());
        assert!(!simi.is_empty(), "相似歌曲为空");

        // 匿名红心：必须返回结构化失败（ok:false + code:301），不能抛异常
        let like = call_function(
            script,
            "likeTrack",
            &[serde_json::json!("347230"), serde_json::json!(true)],
            HashMap::new(),
        )
        .expect("红心调用失败（应返回结构化结果而不是异常）");
        println!("likeTrack (anon) -> {like}");
        assert_eq!(like["ok"].as_bool(), Some(false));
        assert_eq!(like["code"].as_i64(), Some(301));
    }

    /// 真联网验证「VIP 曲 → 第三方解析 → 完整文件」这条新增链路。
    /// 1357375695 = 海阔天空 / Beyond，官方匿名只给 30 秒（fee=1）。
    /// `cargo test --lib netease_vip_unblock_live -- --ignored --nocapture`
    #[test]
    #[ignore = "需要联网访问 music.163.com 与第三方解析服务"]
    fn netease_vip_unblock_live() {
        let script = include_str!("builtin/netease/index.js");
        let track = serde_json::json!({
            "id": "1357375695",
            "title": "海阔天空",
            "artist": "Beyond",
            "album": "",
            "durationMs": 240000,
            "coverUrl": ""
        });

        // 关掉开关：只走官方，拿到的应该是 30 秒试听
        let mut off = HashMap::new();
        off.insert("unblock".to_string(), "off".to_string());
        let trial = call_function(script, "getStreamUrl", &[track.clone()], off)
            .expect("官方取链失败");
        let trial = trial["url"].as_str().unwrap_or_default().to_string();
        println!("official -> {trial}");
        assert!(trial.starts_with("http"));

        let stream = call_function(script, "getStreamUrl", &[track], HashMap::new())
            .expect("解析取链失败");
        let url = stream["url"].as_str().unwrap_or_default().to_string();
        println!(
            "unblocked -> {url}\nquality -> {:?} / {:?} / {:?}",
            stream["source"].as_str(),
            stream["format"].as_str(),
            stream["bitrate"].as_u64()
        );
        assert_eq!(stream["source"].as_str(), Some("resolved"));
        // 解析链是 bodian → migu → gd 逐个尝试，且低规格不收工：直链来自哪家
        // CDN 取决于谁先给到档位要求的规格，不能写死域名。
        // 「全曲」由下面的 Content-Range 长度判定。
        assert!(url.starts_with("http"), "解析直链异常: {url}");
        assert_ne!(url, trial, "解析结果和官方试听是同一条，等于没生效");

        // Content-Range 里的总长度能区分「全曲」和「470KB 试听片段」
        let client = reqwest::blocking::Client::new();
        let resp = client
            .get(&url)
            .header("Range", "bytes=0-1023")
            .send()
            .expect("探测直链失败");
        let range = resp
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        println!("range -> {range}");
        let total = range
            .rsplit('/')
            .next()
            .unwrap_or("0")
            .parse::<u64>()
            .unwrap_or(0);
        assert!(total > 5_000_000, "拿到的是试听片段：{range}");

        // 2.5.0：选「无损」档 → 解析 br=999 → 真 FLAC，且规格如实回传。
        // 347597 = 海阔天空 / Beyond；实测 br=999 下发 29.9MB / 44.1kHz 16bit FLAC，
        // br=320 是 10.2MB 的 MP3。这条专盯「选无损却听 MP3」的回归。
        let hi_track = serde_json::json!({
            "id": "347597", "title": "海阔天空", "artist": "Beyond",
            "album": "", "durationMs": 255000, "coverUrl": ""
        });
        let mut lossless = HashMap::new();
        lossless.insert("level".to_string(), "lossless".to_string());
        lossless.insert("unblockQuality".to_string(), "999".to_string());
        let hi = call_function(script, "getStreamUrl", &[hi_track], lossless)
            .expect("无损档取链失败");
        println!(
            "lossless -> source={:?} format={:?} bitrate={:?} downgraded={:?}",
            hi["source"].as_str(),
            hi["format"].as_str(),
            hi["bitrate"].as_u64(),
            hi["downgraded"].as_bool()
        );
        assert_eq!(hi["source"].as_str(), Some("resolved"));
        assert_eq!(hi["format"].as_str(), Some("flac"), "无损档应拿到 FLAC 直链");
        assert_eq!(hi["downgraded"].as_bool(), Some(false));
    }

    /// 真联网验证推荐聚合（2.6.0）：未登录应返回个性推荐歌单（免登录接口）、
    /// loggedIn=false、每日推荐部分留空。登录态（每日推荐歌单/歌曲）无法在
    /// 测试里提供，由用户登录后实测。
    /// `cargo test --lib netease_recommend_live -- --ignored --nocapture`
    #[test]
    #[ignore = "需要联网访问 music.163.com"]
    fn netease_recommend_live() {
        let script = include_str!("builtin/netease/index.js");
        let feed = call_function(script, "getRecommendFeed", &[serde_json::json!(6)], HashMap::new())
            .expect("推荐聚合失败");
        println!(
            "feed -> loggedIn={} playlists={} tracks={}",
            feed["loggedIn"].as_bool().unwrap_or(false),
            feed["playlists"].as_array().map(|a| a.len()).unwrap_or(0),
            feed["tracks"].as_array().map(|a| a.len()).unwrap_or(0)
        );
        assert_eq!(feed["loggedIn"].as_bool(), Some(false), "匿名 Cookie 不应被判成登录");
        let pl = feed["playlists"].as_array().expect("playlists 应是数组");
        assert!(!pl.is_empty(), "未登录也应有个性推荐歌单");
        let first = &pl[0];
        assert!(!first["id"].as_str().unwrap_or("").is_empty(), "歌单 id 为空");
        assert!(!first["name"].as_str().unwrap_or("").is_empty(), "歌单名为空");
        let cover = first["coverUrl"].as_str().unwrap_or_default();
        assert!(cover.is_empty() || cover.starts_with("https://"), "封面必须 https: {cover}");
    }

    /// 真联网验证云端歌单详情（2.6.0）：匿名也能拉公开歌单（热歌榜 3778678），
    /// 曲目字段要齐全——导入链路直接吃这个结构。
    /// `cargo test --lib netease_playlist_live -- --ignored --nocapture`
    #[test]
    #[ignore = "需要联网访问 music.163.com"]
    fn netease_playlist_live() {
        let script = include_str!("builtin/netease/index.js");
        let pl = call_function(script, "getPlaylist", &[serde_json::json!("3778678")], HashMap::new())
            .expect("歌单详情失败");
        let tracks = pl["tracks"].as_array().expect("tracks 应是数组");
        println!(
            "playlist -> id={} name={:?} trackCount={} tracks={}",
            pl["id"], pl["name"], pl["trackCount"], tracks.len()
        );
        assert!(!tracks.is_empty(), "热歌榜不应为空");
        let t = &tracks[0];
        assert!(!t["id"].as_str().unwrap_or("").is_empty(), "曲目 id 为空");
        assert!(!t["title"].as_str().unwrap_or("").is_empty(), "曲目名为空");
        assert!(t["durationMs"].as_i64().unwrap_or(0) > 0, "时长缺失");
    }

    /// 真联网验证账号歌单（2.6.0）：未登录应返回 loggedIn=false 而不是抛错。
    /// 登录态下的完整歌单列表由用户实测。
    /// `cargo test --lib netease_user_playlists_live -- --ignored --nocapture`
    #[test]
    #[ignore = "需要联网访问 music.163.com"]
    fn netease_user_playlists_live() {
        let script = include_str!("builtin/netease/index.js");
        let data = call_function(script, "getUserPlaylists", &[], HashMap::new())
            .expect("账号歌单调用失败");
        println!("userPlaylists -> {data}");
        assert_eq!(data["loggedIn"].as_bool(), Some(false));
        assert!(data["playlists"].as_array().unwrap_or(&vec![]).is_empty());
    }
}
