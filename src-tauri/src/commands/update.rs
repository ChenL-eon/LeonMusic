//! 应用内更新：下载发行版附件，再交给平台自己完成安装。
//!
//! # 为什么不用 tauri-plugin-updater
//!
//! 官方 updater 只认「静态 latest.json + 签名」那一套，而本项目的下载渠道是
//! Gitee / GitHub 的 Release 附件，版本号由接口动态返回；它并且只会跑 NSIS
//! 安装包，而本项目主推的是**便携版**（包里只有一个 exe），根本没有安装器。
//!
//! 所以这里只做两件事：
//!
//! * [`update_download`]：流式下载到 `cache/updates/`，边下边发进度事件；
//! * [`update_apply`]：Windows 解出便携包里的 exe 后借一个外部小进程替换并重启，
//!   Android 把 APK 交给系统安装器。
//!
//! # Windows 为什么必须借外部进程
//!
//! 运行中的 exe 不能被覆盖，所以「先替换再启动」不可能；而「先启动新进程再退出」
//! 也不行——`tauri-plugin-single-instance` 会让第二个实例把参数转交给第一个后
//! 立刻自杀。唯一的办法是让一个活得更久的进程等我们退出，再覆盖、再拉起。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::error::{AppError, AppResult};
use crate::events::EVT_UPDATE_PROGRESS;
use crate::paths;

/// 单次下载上限。便携包约 7 MB、APK 约 25 MB，留足余量，
/// 同时挡住「接口返回了别的巨型附件」这种意外。
const MAX_BYTES: u64 = 300 * 1024 * 1024;

/// 用户点了「取消」：流式循环每轮检查一次。
static CANCELLED: AtomicBool = AtomicBool::new(false);

/// 最近一次下载/安装活动的 Unix 秒（进程内）。
/// 启动清理靠它避开「用户刚下好、还没点安装」的包。
static LAST_DOWNLOAD: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Serialize)]
struct Progress {
    downloaded: u64,
    total: u64,
}

fn update_dir() -> AppResult<PathBuf> {
    let dir = paths::cache_dir()?.join("updates");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

// ---------------------------------------------------------------------------
// 更新包清理
// ---------------------------------------------------------------------------

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn mark_download() {
    LAST_DOWNLOAD.store(now_secs(), Ordering::SeqCst);
}

fn dir_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&p) {
            for entry in entries.flatten() {
                match entry.file_type() {
                    Ok(t) if t.is_dir() => stack.push(entry.path()),
                    _ => total += entry.metadata().map(|m| m.len()).unwrap_or(0),
                }
            }
        }
    }
    total
}

/// 清掉 `updates/` 里的下载包、半成品和解压副本，返回（删除条目数, 释放字节数）。
///
/// `include_backup = false`（启动自动清理）保留 `previous.exe`：它是最近一次更新前
/// 的旧版程序，留一份可在新版本出问题时手动覆盖回去；下次更新会覆盖它，始终只有一份。
/// `include_backup = true`（设置页「清空缓存」）连它一起删。
/// `update.log` 始终保留：每次覆盖写、只有几 KB，却是更新失败时唯一的线索。
///
/// 只扫一级子项且按名字白名单删除；目录里任何不认识的文件都不碰。
/// 删除失败（文件被占用）静默跳过，下次启动或手动清理再试。
pub fn cleanup_updates(include_backup: bool) -> (usize, u64) {
    match update_dir() {
        Ok(dir) => cleanup_in(&dir, include_backup),
        Err(_) => (0, 0),
    }
}

fn cleanup_in(dir: &Path, include_backup: bool) -> (usize, u64) {
    let mut removed = 0usize;
    let mut freed = 0u64;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let lower = name.to_string_lossy().to_ascii_lowercase();
        let is_stage = lower == "stage";
        let is_package = lower.ends_with(".zip")
            || lower.ends_with(".apk")
            || lower.ends_with(".part");
        let is_backup = include_backup && lower == "previous.exe";
        if !(is_stage || is_package || is_backup) {
            continue;
        }
        let path = entry.path();
        let bytes = if is_stage {
            dir_bytes(&path)
        } else {
            entry.metadata().map(|m| m.len()).unwrap_or(0)
        };
        let ok = if is_stage {
            std::fs::remove_dir_all(&path).is_ok()
        } else {
            std::fs::remove_file(&path).is_ok()
        };
        if ok {
            removed += 1;
            freed += bytes;
        }
    }
    (removed, freed)
}

/// 启动后延迟清理历史更新包。
///
/// 延迟是必须的：Windows 的 helper 要等旧进程退出 → 覆盖 exe → 拉起本进程，
/// 之后还要观察约 8 秒；`stage/LeonMusic.exe` 在覆盖完成前不能删。Android 交给
/// 系统安装器后，也得留出安装器读取 APK 的时间。
/// 若本次启动后又发生过下载，整批跳过，避免删掉刚下好还没安装的包。
pub fn spawn_startup_cleanup() {
    std::thread::spawn(|| {
        #[cfg(target_os = "android")]
        let wait = Duration::from_secs(90);
        #[cfg(not(target_os = "android"))]
        let wait = Duration::from_secs(15);
        std::thread::sleep(wait);

        let last = LAST_DOWNLOAD.load(Ordering::SeqCst);
        if last > 0 && now_secs().saturating_sub(last) < 300 {
            return;
        }
        let (files, bytes) = cleanup_updates(false);
        if files > 0 {
            tracing::info!("cleaned {files} stale update files ({bytes} bytes)");
        }
    });
}

/// 文件名白名单。
///
/// 名字来自发行版附件，本质上是远端可控字符串，只能当**文件名**用：
/// 不允许路径分隔符、不允许前导点（隐藏文件）、不允许非 ASCII。
fn safe_name(raw: &str) -> AppResult<String> {
    let name = raw.trim();
    let ok = !name.is_empty()
        && name.len() <= 120
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if !ok {
        return Err(AppError::invalid(format!("非法的文件名：{raw}")));
    }
    Ok(name.to_string())
}

// ---------------------------------------------------------------------------
// 下载
// ---------------------------------------------------------------------------

/// 把发行版附件下载到 `cache/updates/<file_name>`，返回本地路径。
///
/// 阻塞 IO 放在 `spawn_blocking` 里：reqwest 的流式读取是同步的，
/// 直接丢进 async 上下文会卡住整个 runtime（播放命令也在这个 runtime 上）。
#[tauri::command]
pub async fn update_download(app: AppHandle, url: String, file_name: String) -> AppResult<String> {
    let url = url.trim().to_string();
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(AppError::invalid("下载地址必须是 http(s)"));
    }
    let name = safe_name(&file_name)?;

    tauri::async_runtime::spawn_blocking(move || download(&app, &url, &name))
        .await
        .map_err(|e| AppError::net(format!("下载任务异常退出：{e}")))?
}

fn download(app: &AppHandle, url: &str, name: &str) -> AppResult<String> {
    CANCELLED.store(false, Ordering::SeqCst);
    mark_download();

    let dir = update_dir()?;
    let final_path = dir.join(name);
    // 先写 .part 再改名：中途失败 / 取消不会留下一个「看起来完整」的安装包
    let part = dir.join(format!("{name}.part"));
    let _ = std::fs::remove_file(&part);

    let client = reqwest::blocking::Client::builder()
        .user_agent(crate::net::client::USER_AGENT)
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::net(format!("初始化下载客户端：{e}")))?;

    let mut resp = client
        .get(url)
        .send()
        .map_err(|e| AppError::net(format!("连接下载地址失败：{e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::net(format!(
            "下载地址返回 HTTP {}（发行版附件可能已被替换）",
            resp.status()
        )));
    }

    let total = resp.content_length().unwrap_or(0);
    if total > MAX_BYTES {
        return Err(AppError::net(format!(
            "安装包大小异常（{} MB），已停止下载",
            total / 1024 / 1024
        )));
    }

    let mut file = std::fs::File::create(&part)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut done: u64 = 0;
    let mut last_emit = Instant::now();

    loop {
        if CANCELLED.load(Ordering::SeqCst) {
            drop(file);
            let _ = std::fs::remove_file(&part);
            return Err(AppError::cancelled("已取消下载"));
        }
        let n = resp
            .read(&mut buf)
            .map_err(|e| AppError::net(format!("下载中断：{e}")))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        if done > MAX_BYTES {
            drop(file);
            let _ = std::fs::remove_file(&part);
            return Err(AppError::net("安装包超过 300 MB，已停止下载"));
        }
        // 120ms 一次：进度条够顺滑，又不会把 IPC 刷爆
        if last_emit.elapsed() >= Duration::from_millis(120) || (total > 0 && done >= total) {
            let _ = app.emit(
                EVT_UPDATE_PROGRESS,
                Progress {
                    downloaded: done,
                    total,
                },
            );
            last_emit = Instant::now();
        }
    }

    file.flush()?;
    drop(file);

    if total > 0 && done != total {
        let _ = std::fs::remove_file(&part);
        return Err(AppError::net(format!(
            "下载不完整（{done}/{total} 字节），请重试"
        )));
    }
    std::fs::rename(&part, &final_path)?;
    mark_download();

    let _ = app.emit(
        EVT_UPDATE_PROGRESS,
        Progress {
            downloaded: done,
            total: total.max(done),
        },
    );
    Ok(final_path.to_string_lossy().into_owned())
}

/// 取消进行中的下载（流式循环下一轮就会退出并删除半成品）。
#[tauri::command]
pub fn update_cancel() {
    CANCELLED.store(true, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// 安装
// ---------------------------------------------------------------------------

/// 安装已经下载好的更新包。
///
/// * Windows：`archive` 是便携包 zip，解出 exe 后替换当前程序并重启；
///   调用返回后进程会在 ~0.5s 内退出，所以前端不会收到「已完成」。
/// * Android：`archive` 是 APK，拉起系统安装器后立即返回。
#[tauri::command]
pub fn update_apply(app: AppHandle, archive: String) -> AppResult<()> {
    mark_download();
    let path = PathBuf::from(&archive);
    if !path.is_file() {
        return Err(AppError::NotFound("更新包已被清理，请重新下载".into()));
    }

    #[cfg(windows)]
    {
        windows::apply(&app, &path)
    }
    #[cfg(target_os = "android")]
    {
        let _ = &app;
        android::apply(&path)
    }
    #[cfg(not(any(windows, target_os = "android")))]
    {
        let _ = &app;
        Err(AppError::Unsupported("当前平台不支持应用内更新".into()))
    }
}

// ---------------------------------------------------------------------------
// Windows：解压 → 借 PowerShell 等待退出 → 覆盖 → 重启
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod windows {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::Duration;

    use base64::Engine as _;
    use tauri::AppHandle;

    use super::{update_dir, AppError, AppResult};

    /// 控制台程序不加这个标志会闪一下黑框。
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub fn apply(app: &AppHandle, archive: &Path) -> AppResult<()> {
        let exe = std::env::current_exe()
            .map_err(|e| AppError::invalid(format!("定位当前程序失败：{e}")))?;
        let dir = exe
            .parent()
            .ok_or_else(|| AppError::invalid("找不到程序所在目录"))?
            .to_path_buf();

        ensure_writable(&dir)?;
        let staged = extract_exe(archive, &update_dir()?.join("stage"))?;
        spawn_swapper(&staged, &exe)?;

        // 先让 IPC 响应回到前端，再退出。
        // 覆盖动作由 helper 负责，它一开始就等着我们这个 PID 消失。
        let handle = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            handle.exit(0);
        });
        Ok(())
    }

    /// 目录可写性预检。
    ///
    /// 装在 `Program Files` 里的副本需要管理员权限才能覆盖 exe。这种情况
    /// 与其让 helper 静默失败，不如提前告诉用户「请手动下载安装包」。
    fn ensure_writable(dir: &Path) -> AppResult<()> {
        let probe = dir.join(".leonmusic-update-probe");
        match std::fs::write(&probe, b"ok") {
            Ok(_) => {
                let _ = std::fs::remove_file(&probe);
                Ok(())
            }
            Err(_) => Err(AppError::Permission(
                "程序目录没有写入权限（可能装在 Program Files）。请用「下载页」手动覆盖安装，或以管理员身份运行后重试"
                    .into(),
            )),
        }
    }

    /// 从便携包里取出 exe 并落到 `out_dir`。
    ///
    /// `out_dir` 由调用方给，测试里就能塞临时目录，不必拉起整个 Tauri App。
    fn extract_exe(archive: &Path, out_dir: &Path) -> AppResult<PathBuf> {
        let file = std::fs::File::open(archive)?;
        let mut zip = zip::ZipArchive::new(file)
            .map_err(|e| AppError::invalid(format!("更新包损坏（不是有效的 zip）：{e}")))?;

        let mut found: Option<(String, u64)> = None;
        for i in 0..zip.len() {
            let entry = zip
                .by_index(i)
                .map_err(|e| AppError::invalid(format!("读取更新包失败：{e}")))?;
            if !entry.is_file() {
                continue;
            }
            let name = entry.name().to_string();
            if name.to_ascii_lowercase().ends_with(".exe") {
                found = Some((name, entry.size()));
                break;
            }
        }
        let Some((name, size)) = found else {
            return Err(AppError::invalid("更新包里没有可执行文件"));
        };
        if size < 256 * 1024 {
            return Err(AppError::invalid("更新包里的程序小得异常，已放弃安装"));
        }

        let _ = std::fs::remove_dir_all(out_dir);
        std::fs::create_dir_all(out_dir)?;
        let out = out_dir.join("LeonMusic.exe");

        let mut src = zip
            .by_name(&name)
            .map_err(|e| AppError::invalid(format!("读取更新包失败：{e}")))?;
        let mut dst = std::fs::File::create(&out)?;
        std::io::copy(&mut src, &mut dst)
            .map_err(|e| AppError::invalid(format!("解压失败：{e}（更新包可能不完整）")))?;
        drop(dst);

        // PE 头校验：解压出错或拿到的是网页错误页时，绝不能拿去覆盖正在用的程序
        let mut head = [0u8; 2];
        std::fs::File::open(&out)?.read_exact(&mut head)?;
        if &head != b"MZ" {
            return Err(AppError::invalid("更新包内容不是 Windows 程序，已放弃安装"));
        }
        Ok(out)
    }

    /// PowerShell 里的单引号字符串转义：`'` → `''`。
    fn ps_quote(s: &str) -> String {
        s.replace('\'', "''")
    }

    /// `-EncodedCommand` 要的是 UTF-16LE 字节的 base64。
    ///
    /// 这样路径里有中文 / 空格 / 引号都不会被编码问题咬到，
    /// 也不需要生成临时 .ps1 文件（那还得处理执行策略与残留）。
    fn encode_command(script: &str) -> String {
        let bytes: Vec<u8> = script
            .encode_utf16()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// 等本进程退出 → 备份旧版 → 覆盖 exe → 拉起新版 → 新版起不来就回滚。
    ///
    /// 用占位符替换而不是 `format!`：脚本里的 `$()` `{}` 太多，转义一次就容易错。
    /// 只写日志文件、不重定向子进程输出——`Start-Process` 一旦带 `-RedirectStandard*`
    /// 就会走 `UseShellExecute = false`，PS 5.1 构造环境变量字典时遇到
    /// `HTTPS_PROXY` / `https_proxy` 这类大小写重复的变量会直接抛异常。
    ///
    /// 回滚的意义：更新包损坏 / 架构不对 / 缺运行库时，新进程会秒退。没有回滚
    /// 用户就只剩一个点不开的图标，而更新本来是「点一下」的事，不该有这种代价。
    const SWAP_TEMPLATE: &str = r#"$ErrorActionPreference = 'SilentlyContinue'
'--- start pid=@@PID@@' | Out-File -Encoding utf8 '@@LOG@@'
function W($m) { "$(Get-Date -Format 'HH:mm:ss') $m" | Out-File -Append -Encoding utf8 '@@LOG@@' }
$p = Get-Process -Id @@PID@@
if ($p) { $p.WaitForExit(30000) }
W 'previous instance gone'
Start-Sleep -Milliseconds 700
Copy-Item -LiteralPath '@@DST@@' -Destination '@@BAK@@' -Force
$ok = $false
$i = 0
while ($i -lt 40) {
  try { Copy-Item -LiteralPath '@@SRC@@' -Destination '@@DST@@' -Force -ErrorAction Stop; $ok = $true; break } catch { $i++; Start-Sleep -Milliseconds 400 }
}
W "replace ok=$ok tries=$i"
if (-not $ok) {
  Start-Process -FilePath '@@DST@@' -WorkingDirectory '@@DIR@@'
  W 'replace failed, restarted the old build'
  exit 1
}
$n = Start-Process -FilePath '@@DST@@' -WorkingDirectory '@@DIR@@' -PassThru
W "spawned pid=$($n.Id)"
$j = 0
while ($j -lt 16) { Start-Sleep -Milliseconds 500; if ($n.HasExited) { break }; $j++ }
if ($n -and -not $n.HasExited) {
  # previous.exe 保留为最近一次旧版备份：由应用启动清理 / 设置页「清空缓存」管理，
  # 下次更新 Copy-Item -Force 会覆盖它，始终只有一份。
  W 'new instance alive; previous build kept for rollback'
} else {
  W "new instance gone after $($j * 0.5)s (code=$($n.ExitCode)), rolling back"
  Copy-Item -LiteralPath '@@BAK@@' -Destination '@@DST@@' -Force
  Start-Process -FilePath '@@DST@@' -WorkingDirectory '@@DIR@@'
  W 'rolled back and restarted the old build'
  # 备份不删：新版起不来时，它至少是用户能手动覆盖回去的旧程序
}
"#;

    /// 模板替换集中在这里：单测能直接拿到脚本字符串校验占位符，
    /// 不必真的拉起 PowerShell 去覆盖正在运行的程序。
    fn build_swap_script(new_exe: &Path, target: &Path, dir: &Path) -> String {
        let exe_dir = target.parent().unwrap_or(Path::new("."));
        SWAP_TEMPLATE
            .replace(
                "@@LOG@@",
                &ps_quote(&dir.join("update.log").display().to_string()),
            )
            .replace(
                "@@BAK@@",
                &ps_quote(&dir.join("previous.exe").display().to_string()),
            )
            .replace("@@PID@@", &std::process::id().to_string())
            .replace("@@SRC@@", &ps_quote(&new_exe.display().to_string()))
            .replace("@@DST@@", &ps_quote(&target.display().to_string()))
            .replace("@@DIR@@", &ps_quote(&exe_dir.display().to_string()))
    }

    fn spawn_swapper(new_exe: &Path, target: &Path) -> AppResult<()> {
        let dir = update_dir()?;
        let script = build_swap_script(new_exe, target, &dir);

        std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-WindowStyle",
                "Hidden",
                "-EncodedCommand",
                &encode_command(&script),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| AppError::invalid(format!("无法启动更新进程：{e}")))?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use std::io::Write;

        use super::*;

        fn tmp(tag: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!("leonmusic-update-{tag}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        /// 造一个便携包。CI 用 Compress-Archive，产物就是 Deflate 的普通 zip。
        fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
            let file = std::fs::File::create(path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, data) in entries {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(data).unwrap();
            }
            zip.finish().unwrap();
        }

        /// 600KB 的假 exe：前两字节是 PE 的 `MZ`，其余是 0。
        fn fake_exe(magic: &[u8; 2]) -> Vec<u8> {
            let mut data = vec![0u8; 600 * 1024];
            data[0] = magic[0];
            data[1] = magic[1];
            data
        }

        #[test]
        fn extracts_single_exe_from_portable_zip() {
            let dir = tmp("extract-ok");
            let archive = dir.join("portable.zip");
            let payload = fake_exe(b"MZ");
            make_zip(&archive, &[("LeonMusic.exe", &payload)]);

            let out = extract_exe(&archive, &dir.join("stage")).unwrap();
            assert_eq!(out.file_name().unwrap(), "LeonMusic.exe");
            assert_eq!(
                std::fs::metadata(&out).unwrap().len(),
                payload.len() as u64
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn ignores_non_exe_entries() {
            let dir = tmp("extract-skip");
            let archive = dir.join("portable.zip");
            make_zip(
                &archive,
                &[
                    ("readme.txt", b"hello".as_slice()),
                    ("LeonMusic.exe", &fake_exe(b"MZ")),
                ],
            );

            let out = extract_exe(&archive, &dir.join("stage")).unwrap();
            assert_eq!(out.file_name().unwrap(), "LeonMusic.exe");
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn rejects_html_instead_of_program() {
            // 下载到的是网页错误页时，绝不能拿去覆盖正在用的程序
            let dir = tmp("extract-html");
            let archive = dir.join("portable.zip");
            make_zip(
                &archive,
                &[("LeonMusic.exe", &fake_exe(b"<h"))],
            );

            assert!(extract_exe(&archive, &dir.join("stage")).is_err());
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn rejects_tiny_payload() {
            let dir = tmp("extract-tiny");
            let archive = dir.join("portable.zip");
            make_zip(&archive, &[("LeonMusic.exe", b"MZ")]);

            assert!(extract_exe(&archive, &dir.join("stage")).is_err());
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn rejects_zip_without_exe() {
            let dir = tmp("extract-none");
            let archive = dir.join("portable.zip");
            make_zip(&archive, &[("readme.txt", b"hello".as_slice())]);

            assert!(extract_exe(&archive, &dir.join("stage")).is_err());
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn swap_script_fills_every_placeholder() {
            let dir = tmp("swap-script");
            let script = build_swap_script(
                &dir.join("stage").join("LeonMusic.exe"),
                &dir.join("LeonMusic.exe"),
                &dir,
            );
            assert!(!script.contains("@@"), "占位符没填全：{script}");
            assert!(script.contains("previous build kept for rollback"));
            // helper 不再删旧版备份，清理统一交给 Rust 侧
            assert!(!script.contains("Remove-Item -LiteralPath"));
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn encodes_script_as_utf16le_base64() {
            // PowerShell 的 -EncodedCommand 只认这种编码：
            // base64(UTF-16LE)，用 UTF-8 会直接报「命令行参数格式错误」
            let b64 = encode_command("中文 $x");
            let raw = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
            let text: Vec<u16> = raw
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            assert_eq!(String::from_utf16(&text).unwrap(), "中文 $x");
        }
    }
}

// ---------------------------------------------------------------------------
// Android：把 APK 交给系统安装器
// ---------------------------------------------------------------------------

#[cfg(target_os = "android")]
mod android {
    use std::path::Path;

    use super::{update_dir, AppError, AppResult};

    pub fn apply(archive: &Path) -> AppResult<()> {
        // 只接受下载目录里的 .apk：路径来自前端，不能让它顺手装任意文件
        let dir = update_dir()?;
        let base = dir.canonicalize().unwrap_or(dir);
        let path = archive
            .canonicalize()
            .map_err(|e| AppError::invalid(format!("安装包不存在：{e}")))?;
        if !path.starts_with(&base) {
            return Err(AppError::invalid("安装包路径不合法"));
        }
        if path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("apk"))
            != Some(true)
        {
            return Err(AppError::invalid("不是 APK 文件"));
        }

        match crate::platform::android::bridge::install_apk(&path.to_string_lossy()) {
            Ok(msg) if msg.is_empty() => Ok(()),
            // 例：还没授予「安装未知应用」，Kotlin 侧已把用户送到授权页
            Ok(msg) => Err(AppError::Permission(msg)),
            Err(e) => Err(AppError::invalid(e)),
        }
    }
}

#[cfg(test)]
mod name_tests {
    use super::{cleanup_in, safe_name};

    #[test]
    fn rejects_path_like_names() {
        // 附件名来自远端，只能当文件名用：分隔符 / 前导点 / 中文一律拒绝
        for bad in [
            "",
            ".",
            "..",
            "../evil.zip",
            "a/b.zip",
            "a\\b.zip",
            "x y.zip",
            "更新包.zip",
        ] {
            assert!(safe_name(bad).is_err(), "{bad:?} 应该被拒绝");
        }
    }

    #[test]
    fn keeps_release_asset_names() {
        for good in [
            "LeonMusic-1.4.0-windows-x64-portable.zip",
            "LeonMusic-1.4.0-android-arm64.apk",
        ] {
            assert_eq!(safe_name(good).unwrap(), good);
        }
    }

    #[test]
    fn cleanup_removes_packages_but_keeps_rollback_backup() {
        let dir = std::env::temp_dir().join("leonmusic-update-cleanup");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("stage")).unwrap();
        let fixtures: [(&str, &[u8]); 7] = [
            ("LeonMusic-1.3.0-windows-x64-portable.zip", b"zip"),
            ("LeonMusic-1.4.0-android-arm64.apk", b"apk"),
            ("half.part", b"part"),
            ("update.log", b"log"),
            ("previous.exe", b"old"),
            ("keep.txt", b"keep"),
            ("stage/LeonMusic.exe", b"exe"),
        ];
        for (name, data) in fixtures {
            std::fs::write(dir.join(name), data).unwrap();
        }

        // 启动自动清理：删包、半成品、解压副本，留备份与日志
        let (n, bytes) = cleanup_in(&dir, false);
        assert_eq!(n, 4);
        assert!(bytes > 0);
        assert!(!dir.join("LeonMusic-1.3.0-windows-x64-portable.zip").exists());
        assert!(!dir.join("LeonMusic-1.4.0-android-arm64.apk").exists());
        assert!(!dir.join("half.part").exists());
        assert!(!dir.join("stage").exists());
        assert!(dir.join("previous.exe").exists());
        assert!(dir.join("update.log").exists());
        assert!(dir.join("keep.txt").exists());

        // 「清空缓存」：连旧版备份一起删，日志仍保留供排障
        let (n2, _) = cleanup_in(&dir, true);
        assert_eq!(n2, 1);
        assert!(!dir.join("previous.exe").exists());
        assert!(dir.join("update.log").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
