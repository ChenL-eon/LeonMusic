//! HTTP 流式音源：把一个远程音频 URL 包装成 symphonia 可用的 `MediaSource`
//! （`Read + Seek`），边下边播。
//!
//! # 设计
//!
//! * **顺序下载线程**把响应体追加进内存缓冲 `buf`（`buf[0]` 对应全局偏移
//!   `base`），同时写 `.part` 临时文件；EOF 时如果内存里恰是完整文件就把
//!   `buf` 落盘 rename 成正式缓存。
//! * `Read` 从缓冲取数，取不到就 Condvar 等下载推进（下载速度远高于实时
//!   码率：128k mp3 ≈ 16KB/s，FLAC ≈ 176KB/s，任何宽带都跑赢）。已消费的
//!   头部按窗口淘汰（保留 1MB 供 MediaSourceStream 小幅回退），内存占用
//!   与播放进度挂钩而非文件全长。
//! * `Seek` 目标落在已缓冲区间 → 立即定位；落在区间外（MP4 的 moov 回跳、
//!   用户拖进度条）→ 通知下载线程用 `Range: bytes={n}-` 重开连接。CDN 都
//!   支持 Range，秒回；服务器若不支持 Range（回 200 全量），退化成顺序
//!   下载等推进——行为与旧的整曲落盘一致，只是先出声。
//! * **落盘安全性**：重连会丢弃部分缓冲，所以只有 `.part` 覆盖全文件时才
//!   finalize（写库）。发生过重连的会话播放不受影响（数据按需重拉），
//!   只是缓冲不完整、不进缓存、下次重播再走一次流式。
//! * **取消**：下载线程每收一个 chunk 都检查 `Arc::strong_count`——只剩
//!   线程自己持有（Session 已 drop）就中止、删 `.part`、退出。
//! * **防御**：缓冲带 64MB 硬上限（背压暂停下载）、下载线程 panic 被
//!   catch_unwind 兜住并通知读端（panic=unwind 下线程死了不能让 read 永等）。

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use symphonia::core::io::MediaSource;

use crate::error::{AppError, AppResult};

/// 内存缓冲硬上限：超过后下载线程暂停（背压），等读端消费掉一部分再继续。
/// 64MB 足够任何整曲无损全量缓冲；没有上限时，一个超大响应能把内存吃穿。
const BUF_LIMIT: usize = 64 * 1024 * 1024;
/// read 消费后保留的头部历史窗口：MediaSourceStream 的小幅回退 seek
/// （MP4 box 解析偶尔回读）落在这窗口内就不用重开 Range 连接。
const KEEP_WINDOW: usize = 1024 * 1024;
/// 头部历史超过这个值才触发淘汰（drain 有 memmove 成本，别每个 read 都做）。
const TRIM_THRESHOLD: usize = 4 * 1024 * 1024;

/// 一次流式下载会话：`stream` 交给解码器，`done` 给回填线程等落盘结果
/// （`Ok(path)` = 缓存文件就位；切歌取消时 sender 被丢弃，recv 直接 Err）。
pub struct PendingDownload {
    pub stream: HttpStream,
    pub done: mpsc::Receiver<AppResult<PathBuf>>,
}

struct StreamState {
    /// `buf[0]` 对应的全局字节偏移（Range 重连后等于重连点）。
    base: u64,
    /// 已下载的数据（从 `base` 起顺序追加）。
    buf: Vec<u8>,
    /// 读位置（全局偏移）。
    pos: u64,
    /// 文件全长（Content-Length / Content-Range total）。
    len: Option<u64>,
    /// 首次连接的响应 Content-Type。音源失效时服务端常回 200 + HTML/JSON，
    /// 探测器只会说「不认识这个格式」，带上它才能一眼看出是「回了个网页」。
    content_type: Option<String>,
    /// 请求下载线程从该偏移重开 Range 连接；None = 继续当前连接。
    restart: Option<u64>,
    eof: bool,
    err: Option<String>,
}

struct Inner {
    state: Mutex<StreamState>,
    cv: Condvar,
    /// 下载线程已写盘的字节数（诊断用）。
    written: AtomicU64,
}

/// symphonia `MediaSource` 实现。
pub struct HttpStream(Arc<Inner>);

impl Clone for HttpStream {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl HttpStream {
    fn lock(&self) -> std::sync::MutexGuard<'_, StreamState> {
        self.0.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// 首次连接的 Content-Type（尚未建连时为 None）。
    pub fn content_type(&self) -> Option<String> {
        self.lock().content_type.clone()
    }

    /// 探测用：等首块数据到齐，复制开头 `max` 字节，**不移动读游标**。
    ///
    /// 只对「从 0 开始、尚未淘汰头部」的会话有效；已重连到中间位置时返回空。
    /// 目的是让调用方在交给 symphonia 之前就能识别「服务器回的是网页/JSON」，
    /// 否则只会得到一句 `no suitable format reader found`。
    pub fn sniff(&self, max: usize, wait: Duration) -> Vec<u8> {
        let deadline = Instant::now() + wait;
        let mut st = self.lock();
        loop {
            if st.base == 0 && !st.buf.is_empty() {
                let n = max.min(st.buf.len());
                return st.buf[..n].to_vec();
            }
            if st.base > 0 || st.eof || st.err.is_some() {
                return Vec::new();
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Vec::new();
            }
            let (next, _) = self
                .0
                .cv
                .wait_timeout(st, left)
                .unwrap_or_else(|p| p.into_inner());
            st = next;
        }
    }
}

impl Read for HttpStream {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let mut st = self.lock();
        loop {
            if let Some(e) = &st.err {
                return Err(std::io::Error::other(e.clone()));
            }
            if st.pos < st.base {
                // 读位置落在已淘汰/已重置的缓冲之前：moov 回跳的 seek 刚设置
                // restart，下载线程还没处理完（清 buf、推进 base），本线程的
                // 下一个 read 就到了。绝不能算 `pos - base`（u64 下溢 = 线上
                // crash.log 的 panic 源头），重新标记重连并等 buf 就绪。
                if st.restart.is_none() {
                    st.restart = Some(st.pos);
                    self.0.cv.notify_all();
                }
            } else {
                let avail_end = st.base + st.buf.len() as u64;
                if st.pos < avail_end {
                    let from = (st.pos - st.base) as usize;
                    let n = (st.buf.len() - from).min(out.len());
                    out[..n].copy_from_slice(&st.buf[from..from + n]);
                    st.pos += n as u64;
                    // 窗口化：头部历史超过阈值就淘汰，内存占用跟播放进度走。
                    // 保留 KEEP_WINDOW 字节给 MSS 的小幅回退 seek；更远的回跳
                    // 会走 Seek 的 restart 重连路径（正确性不受影响）。
                    if from > TRIM_THRESHOLD {
                        let drop_n = from - KEEP_WINDOW;
                        st.buf.drain(..drop_n);
                        st.base += drop_n as u64;
                    }
                    return Ok(n);
                }
                if st.eof {
                    return Ok(0);
                }
            }
            // 2s 兜底超时防永久阻塞；正常推进时每 chunk 都会 notify。
            let (nst, _) = self
                .0
                .cv
                .wait_timeout(st, Duration::from_secs(2))
                .unwrap_or_else(|p| p.into_inner());
            st = nst;
        }
    }
}

impl Seek for HttpStream {
    fn seek(&mut self, whence: SeekFrom) -> std::io::Result<u64> {
        let mut st = self.lock();
        let new = match whence {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => st.pos as i64 + n,
            SeekFrom::End(n) => {
                let len = st.len.ok_or_else(|| {
                    std::io::Error::other("stream length unknown (no Content-Length)")
                })? as i64;
                len + n
            }
        };
        if new < 0 {
            return Err(std::io::Error::other("seek before stream start"));
        }
        // 超出文件末尾按 EOF 位置处理（symphonia 不会 seek 超尾，防御性兜底）
        let new = match st.len {
            Some(len) if (new as u64) > len => len,
            _ => new as u64,
        };
        st.pos = new;
        let avail_end = st.base + st.buf.len() as u64;
        if new >= st.base && new < avail_end {
            // 已缓冲区间内 → 直接用
        } else if st.len == Some(new) {
            // seek 恰好到 EOF 位置：symphonia 探测 fMP4 遇「延伸到文件尾的
            // box」（size=0 的 mdat）会 seek 到 End(0) = len。这不该触发
            // 网络重连——`Range: bytes={len}-` 在 RFC 7233 下起点越界，B 站
            // CDN 按标准回 416 Range Not Satisfiable，probe 直接失败
            // （2026-11-08 用户真实日志）。就地标记 EOF，读端立即得 Ok(0)。
            st.eof = true;
        } else {
            // 区间外（moov 回跳 / 拖进度条 / eof 后回 seek）→ 重开 Range 连接
            st.restart = Some(new);
            self.0.cv.notify_all();
        }
        Ok(new)
    }
}

impl MediaSource for HttpStream {
    fn is_seekable(&self) -> bool {
        // 支持：已缓冲区间内即时定位，区间外 Range 重连
        true
    }

    fn byte_len(&self) -> Option<u64> {
        self.lock().len
    }
}

/// 启动流式下载。
///
/// * `client`：无总超时的 blocking client（下载可达几十秒）。
/// * `referer`：按域注入防盗链（B 站 CDN 必须）。
/// * `tmp_path` / `final_path`：`.part` 临时文件与最终缓存文件。
/// * `on_finish`：EOF/失败后由下载线程调用一次（写缓存库等）。
pub fn spawn_download(
    url: String,
    client: reqwest::blocking::Client,
    referer: Option<&'static str>,
    tmp_path: PathBuf,
    final_path: PathBuf,
    on_finish: Box<dyn FnOnce(AppResult<()>) + Send + 'static>,
) -> AppResult<PendingDownload> {
    let inner = Arc::new(Inner {
        state: Mutex::new(StreamState {
            base: 0,
            buf: Vec::new(),
            pos: 0,
            len: None,
            content_type: None,
            restart: None,
            eof: false,
            err: None,
        }),
        cv: Condvar::new(),
        written: AtomicU64::new(0),
    });
    let (tx, rx) = mpsc::channel::<AppResult<PathBuf>>();

    let worker = inner.clone();
    std::thread::Builder::new()
        .name("audio-stream-dl".into())
        .spawn(move || {
            // panic=unwind 下，下载线程若 panic 只杀自己，读端会拿不到
            // err/eof 而永远等 Condvar——这里 catch_unwind 兜住并通知读端，
            // 让上层看到可读错误而不是「加载卡死」。
            let watch = worker.clone();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_download(
                    worker.clone(),
                    url,
                    client,
                    referer,
                    tmp_path,
                    final_path,
                    tx,
                    on_finish,
                );
            }));
            if let Err(p) = result {
                let msg = if let Some(s) = p.downcast_ref::<&str>() {
                    (*s).to_string()
                } else if let Some(s) = p.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                tracing::error!("audio stream download thread panicked: {msg}");
                notify(&watch, Some(format!("download thread panicked: {msg}")), false);
            }
        })
        .map_err(|e| AppError::audio(format!("spawn stream thread: {e}")))?;

    Ok(PendingDownload {
        stream: HttpStream(inner),
        done: rx,
    })
}

/// 每收一个 chunk 检查：只剩下载线程自己持有 Arc（Session 已 drop）就中止。
fn cancelled(inner: &Arc<Inner>) -> bool {
    Arc::strong_count(inner) <= 1
}

fn notify(inner: &Inner, err: Option<String>, eof: bool) {
    {
        let mut st = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        if err.is_some() {
            st.err = err;
        }
        st.eof = eof;
    }
    inner.cv.notify_all();
}

#[allow(clippy::too_many_arguments)]
fn run_download(
    inner: Arc<Inner>,
    url: String,
    client: reqwest::blocking::Client,
    referer: Option<&'static str>,
    tmp_path: PathBuf,
    final_path: PathBuf,
    tx: mpsc::Sender<AppResult<PathBuf>>,
    on_finish: Box<dyn FnOnce(AppResult<()>) + Send + 'static>,
) {
    let mut on_finish = Some(on_finish);
    let mut file: Option<std::fs::File> = None;
    let mut resp: Option<reqwest::blocking::Response> = None;
    let mut connected = false;
    /// EOF 已处理完（落盘/放弃缓存的决策已做）。线程转入驻留：不再主动
    /// 下载，只等读端 seek（EOF 后拖回/重播）触发 restart 重开 Range 连接。
    /// 旧版 EOF 后直接 return，restart 无人处理 → 读端永久卡死。
    let mut parked = false;

    loop {
        // ---- cancel 检查（Session drop）----
        if cancelled(&inner) {
            drop(resp);
            if let Some(f) = file.as_mut() {
                use std::io::Write as _;
                let _ = f.flush();
            }
            drop(file.take());
            let _ = std::fs::remove_file(&tmp_path);
            return; // 不调 on_finish：切歌后没人关心落盘结果
        }

        // ---- 处理重连请求 ----
        let want = {
            let mut st = inner.state.lock().unwrap_or_else(|p| p.into_inner());
            st.restart.take()
        };
        if let Some(n) = want {
            // 越界 restart 防御：len=None 窗口（首连完成前）的 seek 不会被
            // 读端钳位，这里补一道——n >= len 的语义就是 EOF，发
            // `Range: bytes={n}-` 必被 CDN 回 416（RFC 7233）。
            let clamped = {
                let mut st = inner.state.lock().unwrap_or_else(|p| p.into_inner());
                match st.len {
                    Some(len) if n >= len => {
                        st.pos = len;
                        st.eof = true;
                        None
                    }
                    _ => Some(n),
                }
            };
            let Some(n) = clamped else {
                continue; // 已按 EOF 处理，无需重连
            };
            let _ = resp.take(); // 锁外丢弃旧连接
            tracing::info!("stream reconnect at byte {n} for {url}");
            if let Some(f) = file.as_mut() {
                use std::io::{Seek as _, Write as _};
                let _ = f.flush();
                // .part 必须始终反映 [0..n) 已丢弃的事实：
                // 截到 n，后续 Range 数据从 n 追加
                let _ = f.set_len(n);
                let _ = f.seek(SeekFrom::Start(n));
            }
            match open_range(&client, &url, referer, Some(n)) {
                Ok((r, base, total)) => {
                    let mut st = inner.state.lock().unwrap_or_else(|p| p.into_inner());
                    // 重开连接的数据语义：
                    // * 断流续传（resume = base + buf_len > st.base）→ buf 保留，
                    //   新数据紧接已缓冲尾部 append。
                    // * 回跳 / 同点重开（base <= st.base）→ buf 必须清掉重建。
                    //   读端在旧 restart 被 take 后、重连生效前的窗口里会补设
                    //   第二个 restart（见 Read::read 的 pos<base 分支），处理它
                    //   会同偏移重开连接——不清 buf 就会把整段头部再 append
                    //   一遍，数据流错位（回归测试抓到：byte at 8192 读出
                    //   pattern[0]，实为 buf 里的第二份重复数据）。
                    if base <= st.base {
                        st.buf.clear();
                    }
                    st.base = base;
                    st.eof = false;
                    st.err = None;
                    if total.is_some() {
                        st.len = total;
                    }
                    resp = Some(r);
                    connected = true;
                }
                Err(e) => {
                    notify(&inner, Some(e.detail().to_string()), false);
                    if let Some(f) = on_finish.take() {
                        f(Err(e));
                    }
                    let _ = tx.send(Err(AppError::net("stream reconnect failed")));
                    return;
                }
            }
        }

        // ---- 驻留检查：EOF 后没有 restart 就小睡等待，绝不能掉进
        // 下面的「首次连接」把整曲从 0 重下 ----
        if parked {
            if want.is_some() {
                parked = false; // restart 已在上方处理（重开 Range、清 eof 标志）
            } else {
                std::thread::sleep(Duration::from_millis(120));
                continue;
            }
        }

        // ---- 首次连接 ----
        if !connected {
            match open_range(&client, &url, referer, None) {
                Ok((r, _base, total)) => {
                    let ct = r
                        .headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                    let mut st = inner.state.lock().unwrap_or_else(|p| p.into_inner());
                    st.len = total;
                    st.content_type = ct;
                    resp = Some(r);
                    connected = true;
                }
                Err(e) => {
                    notify(&inner, Some(e.detail().to_string()), false);
                    if let Some(f) = on_finish.take() {
                        f(Err(e));
                    }
                    let _ = tx.send(Err(AppError::net("stream open failed")));
                    return;
                }
            }
            if file.is_none() {
                match std::fs::File::create(&tmp_path) {
                    Ok(f) => file = Some(f),
                    Err(e) => {
                        let msg = format!("create part file: {e}");
                        notify(&inner, Some(msg.clone()), false);
                        if let Some(f) = on_finish.take() {
                            f(Err(AppError::net(msg.clone())));
                        }
                        let _ = tx.send(Err(AppError::net(msg)));
                        return;
                    }
                }
            }
        }

        // ---- 读一个 chunk ----
        let Some(r) = resp.as_mut() else {
            // 防御性兜底：正常循环里 resp 与 connected 同步维护，不应该出现
            // 「已连接但无响应句柄」。标记重连而不是 panic（旧版是 unreachable!，
            // 会让整个进程跟着下载线程一起死）。
            connected = false;
            continue;
        };
        let mut chunk = [0u8; 64 * 1024];
        match r.read(&mut chunk) {
            Ok(0) => {
                // 连接结束。区分「真 EOF」与「中途断开」：
                // len 已知且没读满 → 断流，从当前 base 重连续传。
                let (buf_len, base, len) = {
                    let st = inner.state.lock().unwrap_or_else(|p| p.into_inner());
                    (st.buf.len() as u64, st.base, st.len)
                };
                let want_total = len.unwrap_or(buf_len);
                if len.is_some() && base + buf_len < want_total {
                    // 从「已缓冲末尾」续传；buf 会被重连分支按新 base 重置
                    let resume = base + buf_len;
                    tracing::info!(
                        "stream stalled at {resume}/{want_total}, resuming for {url}"
                    );
                    drop(resp.take());
                    connected = false;
                    {
                        let mut st = inner.state.lock().unwrap_or_else(|p| p.into_inner());
                        st.restart = Some(resume);
                    }
                    continue;
                }
                // 真 EOF：.part 文件完整（覆盖全文件）才落盘缓存。
                // 用文件实际大小判定——断流续传 / moov 回跳后内存 base 可能
                // 不为 0，但 .part 始终反映 [0..len) 的真实内容。
                let full = len
                    .is_some()
                    && std::fs::metadata(&tmp_path)
                        .map(|m| m.len() == len.unwrap())
                        .unwrap_or(false);
                if let Some(f) = file.as_mut() {
                    use std::io::Write as _;
                    let _ = f.flush();
                }
                drop(file.take());
                if full {
                    tracing::info!("stream finalized {} -> {} for {url}", len.unwrap(), final_path.display());
                    if let Err(e) = std::fs::rename(&tmp_path, &final_path) {
                        let msg = format!("finalize cache: {e}");
                        notify(&inner, Some(msg.clone()), true);
                        if let Some(f) = on_finish.take() {
                            f(Err(AppError::net(msg.clone())));
                        }
                        let _ = tx.send(Err(AppError::net(msg)));
                        return;
                    }
                    notify(&inner, None, true);
                    if let Some(f) = on_finish.take() {
                        f(Ok(()));
                    }
                    let _ = tx.send(Ok(final_path.clone()));
                } else {
                    // 重连过 / 长度未知：不缓存（下次重播再流式），播放不受影响
                    let _ = std::fs::remove_file(&tmp_path);
                    notify(&inner, None, true);
                    if let Some(f) = on_finish.take() {
                        f(Err(AppError::net("stream not fully buffered, not cached")));
                    }
                    let _ = tx.send(Err(AppError::net("stream not fully buffered, not cached")));
                }
                // EOF 处理完毕 → 驻留而非退出：读端仍可能 seek 回头
                // （重播、拖进度条），restart 必须有人处理。
                resp = None;
                connected = false;
                parked = true;
                continue;
            }
            Ok(n) => {
                // 背压：缓冲已达硬上限且文件还没下完时暂停推进，等读端消费。
                // 正常一首歌 ≤ 40MB 全量缓冲远够；没有这条，异常大响应会吃穿内存。
                loop {
                    if cancelled(&inner) {
                        break;
                    }
                    let stalled = {
                        let st = inner.state.lock().unwrap_or_else(|p| p.into_inner());
                        st.buf.len() >= BUF_LIMIT
                            && st.len.map_or(true, |l| l > st.base + st.buf.len() as u64)
                    };
                    if !stalled {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(150));
                }
                let bytes = &chunk[..n];
                if let Some(f) = file.as_ref() {
                    use std::io::Write as _;
                    let mut f = f;
                    let _ = f.write_all(bytes);
                }
                inner.written.fetch_add(n as u64, Ordering::Relaxed);
                {
                    let mut st = inner.state.lock().unwrap_or_else(|p| p.into_inner());
                    st.buf.extend_from_slice(bytes);
                }
                inner.cv.notify_all();
            }
            Err(e) => {
                let msg = format!("stream read: {e}");
                notify(&inner, Some(msg.clone()), false);
                if let Some(f) = on_finish.take() {
                    f(Err(AppError::net(msg.clone())));
                }
                let _ = tx.send(Err(AppError::net(msg)));
                return;
            }
        }
    }
}

/// 打开连接。`start = Some(n)` 发 `Range: bytes={n}-`；返回实际 base
/// （206 → n；200 → 0，服务器不支持 Range 时退化为顺序下载）与全长。
fn open_range(
    client: &reqwest::blocking::Client,
    url: &str,
    referer: Option<&'static str>,
    start: Option<u64>,
) -> AppResult<(reqwest::blocking::Response, u64, Option<u64>)> {
    let mut req = client.get(url).timeout(Duration::from_secs(120));
    if let Some(n) = start {
        req = req.header("Range", format!("bytes={n}-"));
    }
    if let Some(r) = referer {
        req = req.header("Referer", r);
    }
    let resp = req
        .send()
        .map_err(|e| AppError::net(format!("stream GET {url}: {e}")))?;
    let status = resp.status();
    if status.as_u16() == 206 {
        // Content-Range: bytes {start}-{end}/{total}
        let total = resp
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit('/').next()?.trim().parse::<u64>().ok());
        return Ok((resp, start.unwrap_or(0), total));
    }
    if status.is_success() {
        // 200 全量：不支持 Range，从 0 顺序下载
        let total = resp.content_length();
        return Ok((resp, 0, total));
    }
    Err(AppError::net(format!("HTTP {status} for {url}")))
}

// ---------------------------------------------------------------------------
// 真实网络集成测试（cargo test --release stream -- --nocapture）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::decoder::DecodeSource;
    use std::time::Instant;

    /// 酷我 antiserver：返回《淘汰》128k mp3 的 CDN 直链（前面探针验证过稳定）。
    const ANTI_API: &str = "http://antiserver.kuwo.cn/anti.s?type=convert_url&rid=MUSIC_228908&format=mp3&response=url";

    fn fetch_test_url() -> String {
        let client = reqwest::blocking::Client::builder()
            .user_agent("okhttp/3.10.0")
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap();
        client
            .get(ANTI_API)
            .send()
            .unwrap()
            .text()
            .unwrap()
            .trim()
            .to_string()
    }

    fn scratch(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir();
        let tmp = dir.join(format!("lm-stream-{name}-{}.part", std::process::id()));
        let fin = tmp.with_extension("mp3");
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&fin);
        (tmp, fin)
    }

    /// 本地 mock HTTP 服务器：支持 Range（206 Partial Content）与全量 200，
    /// 返回 `http://127.0.0.1:{port}/file.bin` 与服务器线程句柄。
    /// 外网测试文件（SoundHelix 等）经系统代理时通时断（seek_forward 曾挂满
    /// 120s 超时），流式链路测试一律本地化，字节内容调用方自定义。
    fn spawn_mock(data: Vec<u8>) -> (String, std::thread::JoinHandle<()>) {
        spawn_mock_with_type(data, None)
    }

    /// 同 `spawn_mock`，但指定响应 Content-Type（验证「回的是网页」诊断）。
    fn spawn_mock_with_type(
        data: Vec<u8>,
        ct: Option<&'static str>,
    ) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read as _, Write as _};
        use std::net::{TcpListener, TcpStream};

        fn serve_range(conn: &mut TcpStream, data: &[u8], ct: Option<&str>) -> std::io::Result<()> {
            let mut buf = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                let n = conn.read(&mut byte)?;
                if n == 0 {
                    return Ok(());
                }
                buf.push(byte[0]);
                if buf.ends_with(b"\r\n\r\n") {
                    break;
                }
                if buf.len() > 64 * 1024 {
                    return Ok(());
                }
            }
            let head = String::from_utf8_lossy(&buf);
            let start = head
                .lines()
                .find_map(|l| l.strip_prefix("Range: bytes="))
                .and_then(|v| v.split('-').next())
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            let body = &data[start..];
            let ct_line = ct.map(|c| format!("Content-Type: {c}\r\n")).unwrap_or_default();
            let resp = if head.contains("Range:") {
                format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\n{ct_line}Connection: close\r\n\r\n",
                    body.len(),
                    start,
                    data.len() - 1,
                    data.len()
                )
            } else {
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n{ct_line}Connection: close\r\n\r\n", body.len())
            };
            conn.write_all(resp.as_bytes())?;
            conn.write_all(body)?;
            Ok(())
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let th = std::thread::spawn(move || {
            for conn in listener.incoming() {
                let mut conn = match conn {
                    Ok(c) => c,
                    Err(_) => break,
                };
                let data = data.clone();
                std::thread::spawn(move || {
                    let _ = serve_range(&mut conn, &data, ct);
                });
            }
        });
        (format!("http://127.0.0.1:{port}/file.bin"), th)
    }

    /// ① 起播延迟：probe + 解出前几包应远快于整曲下载（~7s @128k）
    /// ② 解到 EOF 后缓存文件落盘、done 通道报 Ok(path)
    #[test]
    fn stream_head_and_finalize() {
        let url = fetch_test_url();
        assert!(url.starts_with("http"), "antiserver direct url: {url}");
        let (tmp, fin) = scratch("head");
        let client = crate::net::client::BlockingHttp::new();
        let pending = spawn_download(
            url.clone(),
            client.stream_client_for(&url).clone(),
            None,
            tmp.clone(),
            fin.clone(),
            Box::new(|_| {}),
        )
        .unwrap();

        let t0 = Instant::now();
        let mut source = DecodeSource::open_stream(pending.stream, &url).unwrap();
        let mut frames = 0u64;
        for _ in 0..5 {
            let chunk = source.next_chunk(1152).unwrap().expect("audio chunk");
            frames += chunk.samples.len() as u64 / chunk.channels as u64;
        }
        let head_ms = t0.elapsed().as_millis();
        println!(
            "[head] {head_ms}ms frames={frames} sr={} ch={} dur_ms={}",
            source.sample_rate(),
            source.channels(),
            source.duration_ms()
        );
        assert!(frames > 1000, "decoded {frames} frames");
        // 整曲 128k ≈ 16KB/s × 278s ≈ 4.4MB；起播只解码头部，必须显著更快
        assert!(head_ms < 5000, "head decode too slow: {head_ms}ms");

        // 解到 EOF（顺带验证顺序读取全程不断流）
        let t1 = Instant::now();
        let mut chunks = 0u64;
        loop {
            match source.next_chunk(4096).unwrap() {
                Some(_) => chunks += 1,
                None => break,
            }
        }
        println!("[eof] total {chunks} chunks, {}ms", t1.elapsed().as_millis());

        // 落盘完成
        let path = pending
            .done
            .recv_timeout(Duration::from_secs(60))
            .expect("done signal")
            .expect("finalize ok");
        let size = std::fs::metadata(&path).unwrap().len();
        println!("[cache] {} bytes", size);
        assert!(size > 100_000, "cached file too small: {size}");
        assert!(!tmp.exists(), ".part should be renamed away");
        let _ = std::fs::remove_file(&path);
    }

    /// ③ 超前 seek 触发 Range 重连：用户拖进度条 / MP4 moov 回跳走的路径。
    ///    本地 mock + 合成 WAV（60s 440Hz 正弦，16bit mono 44.1k ≈ 5.3MB），
    ///    不再依赖外网大文件。
    /// ④ 取消场景：drop 解码器（Session 结束），下载线程应静默退出。
    #[test]
    fn stream_seek_forward() {
        let sr = 44100u32;
        let n = (sr * 60) as usize;
        let mut wav = Vec::with_capacity(44 + n * 2);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + n as u32 * 2).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&sr.to_le_bytes());
        wav.extend_from_slice(&(sr * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&((n as u32) * 2).to_le_bytes());
        for i in 0..n {
            let t = i as f32 / sr as f32;
            let s = (t * 440.0 * 2.0 * std::f32::consts::PI).sin();
            wav.extend_from_slice(&((s * 20000.0) as i16).to_le_bytes());
        }
        let (url, server) = spawn_mock(wav);
        let (tmp, fin) = scratch("seek");
        // 直连 client（127.0.0.1 会被系统代理劫持，见 stream_backjump 注释）
        let http = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap();
        let pending = spawn_download(
            url.clone(),
            http,
            None,
            tmp.clone(),
            fin.clone(),
            Box::new(|_| {}),
        )
        .unwrap();

        let mut source = DecodeSource::open_stream(pending.stream, &url).unwrap();
        let _ = source.next_chunk(1152).unwrap(); // 先出声
        let total = source.duration_ms();
        let target = total * 9 / 10; // 90% 处：缓冲（刚起步）必然覆盖不到
        let t0 = Instant::now();
        source.seek(target).unwrap();
        let chunk = source.next_chunk(1152).unwrap().expect("chunk after seek");
        let seek_ms = t0.elapsed().as_millis();
        let pos = source.position_ms();
        println!(
            "[seek] target={target}ms seek={seek_ms}ms landed={pos}ms sr={}",
            source.sample_rate()
        );
        assert!(
            (pos as i64 - target as i64).abs() < 2000,
            "seek landed {pos}ms, want ~{target}ms"
        );
        assert!(!chunk.samples.is_empty());

        // 取消场景：drop 解码器（Session 结束），下载线程应静默退出
        drop(source);
        drop(pending.done);
        drop(server);
        std::thread::sleep(Duration::from_millis(300));
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&fin);
    }

    /// 回归测试（crash.log 真实 panic，2026-11-07 ×3）：moov 回跳的 seek 与
    /// 下载线程的重连处理存在竞态——seek 设置 restart 后音频线程立刻 read，
    /// 此时 buf/base 还没更新，旧代码 `pos - base` u64 下溢 panic：
    /// `range start index 18446744073704424094 out of range for slice`。
    /// 本地 mock HTTP 服务器确定性复现：顺序读过头部淘汰阈值 → seek 回 0 →
    /// **立刻**读（不给下载线程处理窗口）。修复后必须等重连并返回正确数据。
    #[test]
    fn stream_backjump_after_trim() {
        use std::net::{TcpListener, TcpStream};

        // 6MB 确定性模式：byte[i] = (i*7+13) % 251，可逐字节验证全局偏移
        let total = 6usize * 1024 * 1024;
        let pattern: Vec<u8> = (0..total).map(|i| ((i * 7 + 13) % 251) as u8).collect();

        fn serve_range(conn: &mut TcpStream, pattern: &[u8]) -> std::io::Result<()> {
            use std::io::Write as _;
            let mut buf = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                let n = conn.read(&mut byte)?;
                if n == 0 {
                    return Ok(());
                }
                buf.push(byte[0]);
                if buf.ends_with(b"\r\n\r\n") {
                    break;
                }
                if buf.len() > 64 * 1024 {
                    return Ok(());
                }
            }
            let head = String::from_utf8_lossy(&buf);
            let start = head
                .lines()
                .find_map(|l| l.strip_prefix("Range: bytes="))
                .and_then(|v| v.split('-').next())
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            let body = &pattern[start..];
            let resp = if head.contains("Range:") {
                format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                    body.len(),
                    start,
                    pattern.len() - 1,
                    pattern.len()
                )
            } else {
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())
            };
            conn.write_all(resp.as_bytes())?;
            conn.write_all(body)?;
            Ok(())
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server_pattern = pattern.clone();
        let server = std::thread::spawn(move || {
            for conn in listener.incoming() {
                let mut conn = match conn {
                    Ok(c) => c,
                    Err(_) => break,
                };
                let pat = server_pattern.clone();
                std::thread::spawn(move || {
                    let _ = serve_range(&mut conn, &pat);
                });
            }
        });

        let url = format!("http://127.0.0.1:{port}/file.bin");
        let (tmp, fin) = scratch("backjump");
        // 直连 client：127.0.0.1 不在 is_direct_host 名单里，默认 client 会
        // 走系统代理（Clash 会把回环地址转发到远端节点，直接失败）。
        let http = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap();
        let pending = spawn_download(
            url.clone(),
            http,
            None,
            tmp.clone(),
            fin.clone(),
            Box::new(|_| {}),
        )
        .unwrap();

        let mut s = pending.stream;
        let target = 4usize * 1024 * 1024 + 512 * 1024; // 4.5MB，跨过 TRIM_THRESHOLD
        let mut sink = vec![0u8; 64 * 1024];
        let mut got = 0usize;
        while got < target {
            let n = s.read(&mut sink).unwrap();
            assert!(n > 0, "eof before {target}");
            for (k, &b) in sink[..n].iter().enumerate() {
                let gi = got + k;
                assert_eq!(b, ((gi * 7 + 13) % 251) as u8, "seq read byte at {gi}");
            }
            got += n;
        }
        // base 已推进（头部被淘汰），pos=4.5MB

        // crash.log 场景：seek 回文件头后立刻读，命中 pos < base 竞态分支
        s.seek(std::io::SeekFrom::Start(0)).unwrap();
        let n = s.read(&mut sink).unwrap(); // 修复前：u64 下溢 panic
        assert!(n > 0, "no data after backjump");
        for (k, &b) in sink[..n].iter().enumerate() {
            assert_eq!(b, ((k * 7 + 13) % 251) as u8, "backjump byte at {k}");
        }

        // 重连后从 0 继续连续读 2MB，数据流必须完整正确
        let mut off = n;
        while off < 2 * 1024 * 1024 {
            let n2 = s.read(&mut sink).unwrap();
            if n2 == 0 {
                break;
            }
            for (k, &b) in sink[..n2].iter().enumerate() {
                let gi = off + k;
                assert_eq!(b, ((gi * 7 + 13) % 251) as u8, "post-backjump byte at {gi}");
            }
            off += n2;
        }

        // ④ 确定性 EOF 驻留场景：把整首读到底（下载线程必然已完成落盘并
        //    驻留），再 seek 回头部——旧版线程已退出，restart 无人处理，
        //    read 永久卡死（回归测试首跑抓到的第二个缺陷）。
        loop {
            let n3 = s.read(&mut sink).unwrap();
            if n3 == 0 {
                break;
            }
            for (k, &b) in sink[..n3].iter().enumerate() {
                let gi = off + k;
                assert_eq!(b, ((gi * 7 + 13) % 251) as u8, "tail byte at {gi}");
            }
            off += n3;
        }
        assert_eq!(off, total, "must reach exact EOF at {total}");
        s.seek(std::io::SeekFrom::Start(1024)).unwrap();
        let n4 = s.read(&mut sink).unwrap();
        assert!(n4 > 0, "no data after post-EOF backjump (parked thread restart)");
        for (k, &b) in sink[..n4].iter().enumerate() {
            let gi = 1024 + k;
            assert_eq!(b, ((gi * 7 + 13) % 251) as u8, "post-eof byte at {gi}");
        }

        drop(s);
        drop(pending.done);
        drop(server);
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&fin);
    }

    /// ⑤ 复现 B 站 CDN 416（2026-11-08 用户真实日志）：symphonia 探测 fMP4
    ///    遇「延伸到文件尾的 box」（size=0 的 mdat）会 seek 到 End(0) = len；
    ///    旧逻辑把「seek 恰好到 EOF 位置」也当区间外回跳，发
    ///    `Range: bytes={len}-` 重连，B 站按 RFC 7233 回 416 Range Not
    ///    Satisfiable → probe failed。seek 到 EOF 是合法操作，应就地 EOF。
    ///    需真实 B 站 m4s URL：环境变量 BILI_416_URL（node 探针生成）。
    #[test]
    #[ignore]
    fn stream_seek_to_end_416_live() {
        let url = std::env::var("BILI_416_URL").unwrap_or_else(|_| {
            let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../build/bili_test_url.txt");
            std::fs::read_to_string(p)
                .expect("build/bili_test_url.txt (run probe_bili_416.cjs first)")
                .trim()
                .to_string()
        });
        let (tmp, fin) = scratch("bili416");
        let client = crate::net::client::BlockingHttp::new();
        let pending = spawn_download(
            url.clone(),
            client.stream_client_for(&url).clone(),
            Some("https://www.bilibili.com/"),
            tmp.clone(),
            fin.clone(),
            Box::new(|_| {}),
        )
        .unwrap();

        // 等 Content-Length 就位（首次连接完成）
        let mut len = None;
        for _ in 0..50 {
            if let Some(n) = pending.stream.byte_len() {
                len = Some(n);
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let len = len.expect("Content-Length known after first connection");
        println!("[416] len={len}");

        let mut s = pending.stream.clone();
        // symphonia 遇 size=0 box 的行为：seek 到文件末尾
        let pos = s.seek(std::io::SeekFrom::End(0)).unwrap();
        assert_eq!(pos, len);
        // 修复前：下载线程发 Range: bytes={len}- → CDN 416 → read Err
        // 修复后：就地 EOF，read 返回 Ok(0)
        let mut buf = [0u8; 1];
        let r = s.read(&mut buf);
        println!("[416] read at EOF -> {:?}", r.as_ref().map(|n| *n));
        assert!(
            matches!(r, Ok(0)),
            "seek to EOF must yield clean EOF, got {r:?}"
        );

        drop(s);
        drop(pending.done);

        // 完整场景验证：symphonia probe 打开同款 B 站 fMP4 流（用户日志里
        // 失败的那一步），修复后应正常探测并解出音频帧。
        let pending2 = spawn_download(
            url.clone(),
            client.stream_client_for(&url).clone(),
            Some("https://www.bilibili.com/"),
            tmp.clone(),
            fin.clone(),
            Box::new(|_| {}),
        )
        .unwrap();
        let mut source = crate::audio::decoder::DecodeSource::open_stream(pending2.stream, &url)
            .expect("probe must succeed on bilibili fMP4 stream");
        let chunk = source
            .next_chunk(1152)
            .expect("decode after probe")
            .expect("audio chunk");
        println!(
            "[416] probe ok: sr={} ch={} first_frame_samples={}",
            source.sample_rate(),
            source.channels(),
            chunk.samples.len()
        );
        assert!(!chunk.samples.is_empty());
        drop(source);
        drop(pending2.done);
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&fin);
    }

    /// ⑥ 音源失效时服务端常回 200 + 登录页 / JSON / 空体。旧版只看得到
    ///    symphonia 的 `no suitable format reader found`（2026-11-08 安卓端
    ///    用户截图），分不清是「链接失效」还是「格式不支持」。现在按开头
    ///    字节判定，直接给出中文结论 + Content-Type。
    #[test]
    fn non_audio_response_is_explained() {
        let cases: [(&str, Vec<u8>); 3] = [
            (
                "html",
                b"<!DOCTYPE html><html><body>Please sign in</body></html>".to_vec(),
            ),
            ("json", br#"{"error":"token expired"}"#.to_vec()),
            ("empty", Vec::new()),
        ];
        for (name, body) in cases {
            let (url, server) = match name {
                "html" => spawn_mock_with_type(body, Some("text/html; charset=utf-8")),
                "json" => spawn_mock_with_type(body, Some("application/json")),
                _ => spawn_mock(body),
            };
            let (tmp, fin) = scratch(name);
            let client = crate::net::client::BlockingHttp::new();
            let pending = spawn_download(
                url.clone(),
                client.stream_client_for(&url).clone(),
                None,
                tmp.clone(),
                fin.clone(),
                Box::new(|_| {}),
            )
            .unwrap();
            let err = match DecodeSource::open_stream(pending.stream, &url) {
                Ok(_) => panic!("{name}: non-audio body must not probe successfully"),
                Err(e) => e.detail().to_string(),
            };
            println!("[{name}] {err}");
            if name == "empty" {
                assert!(err.contains("没有返回任何数据"), "{name}: {err}");
            } else {
                assert!(err.contains("不是音频"), "{name}: {err}");
            }
            if name == "html" {
                assert!(err.contains("网页/XML"), "{name}: {err}");
                assert!(err.contains("text/html"), "{name}: {err}");
            }
            if name == "json" {
                assert!(err.contains("JSON"), "{name}: {err}");
            }
            drop(pending.done);
            drop(server);
            let _ = std::fs::remove_file(&tmp);
            let _ = std::fs::remove_file(&fin);
        }
    }
}
