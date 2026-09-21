//! 启动日志 + 崩溃兜底。
//!
//! 为什么需要这个：程序在 Windows 上多半是双击启动的，控制台窗口一关，
//! 出错信息就跟着没了——用户看到的就是「打开之后闪退」，我们什么都拿不到。
//! 所以：
//!
//!   1. 关键节点都写一份到 exe 同目录的 `voice-keyboard.log`（写不了就退到临时目录）
//!   2. panic 也记进日志，Windows 上再弹个对话框把原因糊到脸上
//!
//! 日志路径在启动横幅里也会打出来。
//!
//! 时间戳是 **UTC**（不引时区库），格式 `[2026-09-20T18:40:12Z +12.3s]`，
//! 后面的 `+12.3s` 是相对进程启动的秒数——两边对照着看足够定位了。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// 往控制台打一行，**写不进去就算了**。
///
/// 为什么不用 `println!`：Windows 上 GUI 模式会把控制台 `FreeConsole` 掉，
/// 之后 stdout 是无效句柄，`println!` 会 panic —— 「打开之后闪退」的经典成因。
/// 真正的记录在日志文件里，控制台只是顺带看一眼。
#[macro_export]
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

#[macro_export]
macro_rules! say_err {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

static SINK: OnceLock<Mutex<Option<File>>> = OnceLock::new();
static FILE_PATH: OnceLock<PathBuf> = OnceLock::new();
static STARTED: OnceLock<SystemTime> = OnceLock::new();

/// 打开日志文件并装上 panic 钩子。启动时第一件事就调它。
pub fn init() -> PathBuf {
    let _ = STARTED.set(SystemTime::now());
    let path = pick_path();
    let file = OpenOptions::new().create(true).append(true).open(&path).ok();
    let _ = SINK.set(Mutex::new(file));
    let _ = FILE_PATH.set(path.clone());
    install_panic_hook();
    line(format!(
        "──────── 启动 v{} · {} ────────",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS
    ));
    let opened = SINK
        .get()
        .and_then(|s| s.lock().ok())
        .map(|g| g.is_some())
        .unwrap_or(false);
    if !opened {
        crate::say_err!("  ! 日志文件写不了：{}", path.display());
    }
    path
}

/// 日志文件在哪（给界面和错误提示用）。
pub fn path() -> PathBuf {
    FILE_PATH
        .get()
        .cloned()
        .unwrap_or_else(|| std::env::temp_dir().join("voice-keyboard.log"))
}

/// 写一行日志，同时打到 stderr（控制台在的时候两边都能看到）。
pub fn line(msg: impl AsRef<str>) {
    let text = format!("{} {}", stamp(), msg.as_ref());
    if let Some(sink) = SINK.get() {
        if let Ok(mut guard) = sink.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = writeln!(f, "{text}");
                let _ = f.flush();
            }
        }
    }
    crate::say_err!("{text}");
}

fn pick_path() -> PathBuf {
    const NAME: &str = "voice-keyboard.log";
    // 放 exe 旁边最方便（用户拿到就能发过来）；程序装在 Program Files 里时
    // 目录不可写，那就退回临时目录。
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join(NAME);
            if OpenOptions::new().create(true).append(true).open(&p).is_ok() {
                return p;
            }
        }
    }
    std::env::temp_dir().join(NAME)
}

fn stamp() -> String {
    let now = SystemTime::now();
    let up = STARTED
        .get()
        .and_then(|s| now.duration_since(*s).ok())
        .unwrap_or_default();
    let secs = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "[{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z +{:.1}s]",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
        up.as_secs_f64()
    )
}

/// 1970-01-01 起的天数 → 年月日（Howard Hinnant 的 civil_from_days）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// panic 也留个记录，别让它随控制台一起消失。
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        line(format!("!!! 崩溃（panic）：{info}"));
        line(format!("!!! 日志：{}", path().display()));
        error_box(
            "语音键盘 · 崩溃了",
            &format!(
                "{info}\n\n详细日志：{}\n把这个文件发过来就能定位。",
                path().display()
            ),
        );
        default_hook(info);
    }));
}

/// Windows 上弹个对话框。其它平台没有“双击启动的窗口会跟着消失”这个问题，
/// 控制台里的信息留着就够了。
#[cfg(windows)]
pub fn error_box(title: &str, msg: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let wide = |s: &str| -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    };
    let (t, m) = (wide(title), wide(msg));
    // SAFETY: 两个字符串都以 NUL 结尾，调用期间一直活着。
    unsafe {
        MessageBoxW(std::ptr::null_mut(), m.as_ptr(), t.as_ptr(), MB_OK | MB_ICONERROR);
    }
}

#[cfg(not(windows))]
pub fn error_box(_title: &str, _msg: &str) {}
