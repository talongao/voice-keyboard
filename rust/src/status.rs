//! 「现在到底跑在哪个端口上」得要能被别的程序问到。
//!
//! 端口不再是写死的 8765 了（被占用就自动往后找，见 main.rs），所以
//! 「约定 8765」这件事靠不住：脚本、托盘、其它客户端都得知道当前实例在哪。
//! 做法是启动成功后写一份 status.json 到固定位置，退出时删掉；
//! `--status` 就是读它。文件里带上端口、地址、配对链接、pid、日志路径。
//!
//! 位置（尽量按各平台惯例，不引 dirs 这种库）：
//!
//! | 平台 | 路径 |
//! |---|---|
//! | Windows | `%LOCALAPPDATA%\voice-keyboard\status.json` |
//! | macOS | `~/Library/Application Support/voice-keyboard/status.json` |
//! | Linux | `$XDG_STATE_HOME/voice-keyboard/status.json`，退回 `~/.local/state/voice-keyboard/` |
//!
//! 都拿不到（HOME 都没有的怪环境）就退回临时目录。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use crate::http::APP_TAG;

pub fn dir() -> PathBuf {
    #[cfg(windows)]
    if let Some(p) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(p).join(APP_TAG);
    }
    #[cfg(target_os = "macos")]
    if let Some(h) = std::env::var_os("HOME") {
        return PathBuf::from(h)
            .join("Library/Application Support")
            .join(APP_TAG);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(p) = std::env::var_os("XDG_STATE_HOME") {
            return PathBuf::from(p).join(APP_TAG);
        }
        if let Some(h) = std::env::var_os("HOME") {
            return PathBuf::from(h).join(".local/state").join(APP_TAG);
        }
    }
    std::env::temp_dir().join(APP_TAG)
}

pub fn file() -> PathBuf {
    dir().join("status.json")
}

pub fn write(v: &serde_json::Value) -> Option<PathBuf> {
    let f = file();
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d).ok()?;
    }
    let text = serde_json::to_string_pretty(v).ok()?;
    std::fs::write(&f, text).ok()?;
    Some(f)
}

pub fn read() -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(file()).ok()?).ok()
}

pub fn clear() {
    let _ = std::fs::remove_file(file());
}

/// 某个端口上是不是本程序在跑？返回 `/api/info` 里的 app 名。
///
/// 用来区分两种「端口被占」：自己已经开了一个（那就不该再起一个），
/// 还是别的程序占着（那就换端口）。
pub fn who_is_on(port: u16) -> Option<String> {
    let body = http_get(port, "/api/info")?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    v.get("app")?.as_str().map(str::to_string)
}

/// 极简 HTTP/1.0 POST（只为 /api/shutdown 这种本机指令用）。
pub fn post(port: u16, path: &str) -> Result<String, String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(600))
        .map_err(|e| format!("连不上 127.0.0.1:{port}：{e}"))?;
    let _ = s.set_read_timeout(Some(Duration::from_millis(1500)));
    write!(
        s,
        "POST {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}"
    )
    .map_err(|e| e.to_string())?;
    let mut buf = String::new();
    s.read_to_string(&mut buf).map_err(|e| e.to_string())?;
    buf.split_once("\r\n\r\n")
        .map(|(_, body)| body.trim().to_string())
        .ok_or_else(|| "响应不对".to_string())
}

/// 极简 HTTP/1.0 GET：只为了问一句「这端口上是谁」，
/// 不值得为此拖一个 HTTP 客户端进来。
fn http_get(port: u16, path: &str) -> Option<String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(400)).ok()?;
    let _ = s.set_read_timeout(Some(Duration::from_millis(800)));
    write!(s, "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").ok()?;
    let mut buf = String::new();
    s.read_to_string(&mut buf).ok()?;
    buf.split_once("\r\n\r\n")
        .map(|(_, body)| body.trim().to_string())
}

/// 退出时把 status.json 删掉。挂在 main 上，返回/panic 都能走到 Drop。
pub struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        clear();
    }
}
