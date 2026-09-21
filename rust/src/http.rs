//! HTTP 服务。路由和 Python 版一一对应，所以手机端 HTML 不用改。
//!
//! 用 tiny_http 而不是 axum：请求量就这么点，不值得拖一个 tokio 运行时
//! 进来（那会多好几 MB）。

use std::io::{Cursor, Read};
use std::sync::Arc;
use std::time::Duration;

use tiny_http::{Header, Method, Response, Server, StatusCode};

use crate::inject::{self, InjectError};
use crate::net;
use crate::state::{now_secs, App, LogEntry};

// 前端直接复用 Python 版的两个页面，单一来源。
const INDEX_HTML: &str = include_str!("../../index.html");
const CONSOLE_HTML: &str = include_str!("../../console.html");

pub const APP_TAG: &str = "voice-keyboard";

type Reply = Response<Cursor<Vec<u8>>>;

fn reply(code: u16, body: impl Into<Vec<u8>>, ctype: &str) -> Reply {
    let h = Header::from_bytes(&b"Content-Type"[..], ctype.as_bytes())
        .expect("static header");
    let h2 = Header::from_bytes(&b"Cache-Control"[..], &b"no-store"[..])
        .expect("static header");
    Response::from_data(body.into())
        .with_status_code(StatusCode(code))
        .with_header(h)
        .with_header(h2)
}

fn json(code: u16, v: serde_json::Value) -> Reply {
    reply(code, v.to_string().into_bytes(), "application/json; charset=utf-8")
}

fn html(body: String) -> Reply {
    reply(200, body.into_bytes(), "text/html; charset=utf-8")
}

/// 只做「把端口占下来」这一步。
///
/// 单独拆出来是给 GUI 用的：窗口一旦画出来就相当于对用户承诺
/// 「扫这个码就能连」，所以得先确认端口真的占上了再开窗口。
pub fn bind_addr(bind: &str) -> Result<Server, String> {
    Server::http(bind).map_err(|e| format!("监听 {bind} 失败：{e}"))
}

/// 同上，但用自签证书起 HTTPS（麦克风模式）
pub fn bind_https(bind: &str, t: &crate::tls::Tls) -> Result<Server, String> {
    Server::https(
        bind,
        tiny_http::SslConfig {
            certificate: t.cert_pem.clone(),
            private_key: t.key_pem.clone(),
        },
    )
    .map_err(|e| format!("监听 {bind}(HTTPS) 失败：{e}"))
}

pub fn serve_on(app: Arc<App>, server: &Server) -> Result<(), String> {
    for req in server.incoming_requests() {
        let app = app.clone();
        // 每个请求一个线程。请求量很小，不值得上线程池。
        std::thread::spawn(move || {
            let _ = handle(app, req);
        });
    }
    Ok(())
}

fn handle(app: Arc<App>, mut req: tiny_http::Request) -> std::io::Result<()> {
    let path = req.url().split('?').next().unwrap_or("/").to_string();
    let is_local = req
        .remote_addr()
        .map(|a| a.ip().is_loopback())
        .unwrap_or(false);
    let method = req.method().clone();

    let token = req
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case("authorization"))
        .map(|h| h.value.as_str().to_string())
        .and_then(|v| v.strip_prefix("Bearer ").map(|s| s.to_string()))
        .unwrap_or_default();

    let mut body = String::new();
    if method == Method::Post {
        // as_reader() 给的是 &mut dyn Read，而 Read::take 需要 Self: Sized
        // （trait object 不是 Sized）。包一层 &mut 之后 Self 就是 &mut dyn Read 了。
        let mut reader = req.as_reader();
        let mut buf = Vec::new();
        let _ = (&mut reader).take(1 << 20).read_to_end(&mut buf);
        body = String::from_utf8_lossy(&buf).into_owned();
    }
    let data: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);

    let resp = route(&app, &method, &path, is_local, &token, &data);
    req.respond(resp)
}

fn route(
    app: &Arc<App>,
    method: &Method,
    path: &str,
    is_local: bool,
    token: &str,
    data: &serde_json::Value,
) -> Reply {
    let is_get = *method == Method::Get;

    // ---------- 静态页面 ----------
    if is_get && (path == "/" || path == "/index.html") {
        return html(INDEX_HTML.to_string());
    }
    if is_get && path == "/favicon.ico" {
        return reply(200, Vec::new(), "image/x-icon");
    }

    if is_get && (path == "/console" || path == "/qr") {
        if !is_local {
            return reply(403, b"only local".to_vec(), "text/plain; charset=utf-8");
        }
        let eps = net::endpoints(app.port);
        let url = eps
            .first()
            .map(|e| net::pair_url(e, Some(&app.pair_key), app.pin.as_deref()))
            .unwrap_or_default();
        let svg = net::svg(&url, 8).unwrap_or_default();
        return html(
            CONSOLE_HTML
                .replace("{{QR_SVG}}", &svg)
                .replace("{{PAIR_URL}}", &url),
        );
    }

    // ---------- 公开 ----------
    if *method == Method::Get && path == "/api/info" {
        return json(
            200,
            serde_json::json!({
                "app": APP_TAG,
                "need_auth": app.pin.is_some(),
                "mode": app.mode,
                "allow_enter": app.enter_enabled(),
                "host": hostname(),
            }),
        );
    }

    if *method == Method::Post && path == "/api/pair" {
        return api_pair(app, data);
    }

    // ---------- 仅本机 ----------
    if path == "/api/status" {
        if !is_local {
            return json(403, serde_json::json!({ "error": "仅本机" }));
        }
        let eps = net::endpoints(app.port);
        return json(
            200,
            serde_json::json!({
                "mode": app.mode,
                "endpoints": eps,
                "pin": app.pin,
                "pair_key": app.pair_key,
                "clients": app.client_count(),
                "allow_enter": app.enter_enabled(),
                "log": app.log_json(10),
                "events": app.events_json(40),
                "uptime": app.uptime(),
            }),
        );
    }

    if path == "/api/settings" {
        if !is_local {
            return json(403, serde_json::json!({ "error": "仅本机" }));
        }
        if let Some(v) = data.get("allow_enter").and_then(|v| v.as_bool()) {
            app.set_enter(v);
        }
        return json(
            200,
            serde_json::json!({
                "ok": true,
                "allow_enter": app.enter_enabled(),
                "endpoints": net::endpoints(app.port),
            }),
        );
    }

    if path == "/api/show" {
        if !is_local {
            return json(403, serde_json::json!({ "error": "仅本机" }));
        }
        app.request_show();
        return json(200, serde_json::json!({ "ok": true }));
    }

    if path == "/api/shutdown" {
        if !is_local {
            return json(403, serde_json::json!({ "error": "仅本机" }));
        }
        crate::say!("  · 收到停止指令，正在退出");
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(250)); // 先把响应发出去
            std::process::exit(0);
        });
        return json(200, serde_json::json!({ "ok": true, "bye": true }));
    }

    // ---------- 需要 token ----------
    if !app.check_token(token) {
        return json(401, serde_json::json!({ "error": "未授权" }));
    }

    if *method == Method::Get && path == "/api/endpoints" {
        return json(200, serde_json::json!({ "endpoints": net::endpoints(app.port) }));
    }

    if *method == Method::Post && path == "/api/send" {
        return api_send(app, data);
    }

    if *method == Method::Post && path == "/api/undo" {
        return api_undo(app, data);
    }
    if *method == Method::Post && path == "/api/scroll" {
        return api_scroll(app, data);
    }
    if *method == Method::Post && path == "/api/key" {
        return api_key(app, data);
    }

    // ---------- 麦克风模式 ----------
    // 需要 token：手机端切换 Tab 时调。
    if *method == Method::Get && path == "/api/mic/status" {
        let p = crate::mic::probe();
        return json(200, serde_json::json!({
            "ok": p.ok,
            "platform": p.platform,
            "device": p.device,
            "reason": p.reason,
            "tls": crate::listener::tls_on(),
        }));
    }

    if *method == Method::Get && path == "/api/mic/guide" {
        return json(200, crate::mic::guide(crate::listener::tls_on()));
    }

    // 一键创建虚拟麦克风（只有 Linux 能自动）
    if *method == Method::Post && path == "/api/mic/setup" {
        return match crate::mic::setup() {
            Ok(msg) => json(200, serde_json::json!({ "ok": true, "message": msg })),
            Err(e) => json(200, serde_json::json!({ "ok": false, "error": e })),
        };
    }

    if *method == Method::Get && path == "/api/tls" {
        return json(200, serde_json::json!({
            "enabled": crate::listener::tls_on(),
            "cert": crate::tls::cert_exists(),
        }));
    }

    // 生成自签证书并当场把协议换成 HTTPS（同一个端口、同一个令牌，不用重新配对）
    if *method == Method::Post && path == "/api/tls/enable" {
        let ips = net::local_ips();
        return match crate::tls::generate(&crate::tls::base(), &ips) {
            Ok(_) => {
                crate::listener::request_tls(true);
                let url = net::endpoints(app.port)
                    .first()
                    .map(|e| e.replacen("http://", "https://", 1))
                    .unwrap_or_default();
                app.note("tls", "已生成自签证书，切到 HTTPS".to_string());
                json(200, serde_json::json!({
                    "ok": true, "switching": true, "url": url,
                    "hint": "手机需要用 https 重新打开；首次要信任一次证书"
                }))
            }
            Err(e) => json(200, serde_json::json!({ "ok": false, "error": e })),
        };
    }

    // 删掉证书、退回 HTTP
    if *method == Method::Post && path == "/api/tls/disable" {
        return match crate::tls::remove(&crate::tls::base()) {
            Ok(()) => {
                crate::listener::request_tls(false);
                json(200, serde_json::json!({ "ok": true, "switching": true }))
            }
            Err(e) => json(200, serde_json::json!({ "ok": false, "error": e })),
        };
    }

    json(404, serde_json::json!({ "error": "no such api" }))
}

fn api_pair(app: &Arc<App>, data: &serde_json::Value) -> Reply {
    // 长密钥（二维码里那个）永远可用
    if let Some(k) = data.get("k").and_then(|v| v.as_str()) {
        if !k.is_empty() {
            if k == app.pair_key {
                let t = app.issue_token();
                let n = app.client_count();
                app.note("pair", format!("新设备已配对 (共 {n} 台)"));
                return json(200, serde_json::json!({ "token": t }));
            }
            std::thread::sleep(Duration::from_millis(400));
            return json(403, serde_json::json!({ "error": "密钥不对" }));
        }
    }

    if app.pin.is_none() {
        return json(200, serde_json::json!({ "token": app.issue_token() }));
    }

    let pin = data.get("pin").and_then(|v| v.as_str()).unwrap_or("");
    if !app.rate_limit_ok() {
        return json(403, serde_json::json!({ "error": "PIN 不对，或尝试过于频繁" }));
    }
    let want = app.pin.clone().unwrap_or_default();
    let ok = pin.len() == want.len()
        && pin
            .bytes()
            .zip(want.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0;
    if !ok {
        std::thread::sleep(Duration::from_millis(600));
        return json(403, serde_json::json!({ "error": "PIN 不对，或尝试过于频繁" }));
    }

    let t = app.issue_token();
    let n = app.client_count();
    app.note("pair", format!("新设备已配对 (共 {n} 台)"));
    json(200, serde_json::json!({ "token": t }))
}

/// 单独敲一个键。手机上的用途最主要是「回车」：
/// 关掉「发送后回车」之后，手机端就没别的办法让电脑那边回车了。
fn api_key(app: &Arc<App>, data: &serde_json::Value) -> Reply {
    let name = data.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let Some(key) = inject::KeyName::parse(name) else {
        return json(
            400,
            serde_json::json!({ "error": format!("不认识的按键：{name}") }),
        );
    };
    let times = data
        .get("times")
        .and_then(|v| v.as_i64())
        .unwrap_or(1)
        .clamp(1, 20) as usize;
    let dryrun = app.mode == "dryrun";
    let result = {
        let _guard = app.inject.lock().unwrap();
        inject::press(key, times, dryrun)
    };
    match result {
        Ok(()) => json(200, serde_json::json!({ "ok": true, "key": name, "times": times })),
        Err(e) => json(500, serde_json::json!({ "error": format!("{e}") })),
    }
}

/// 滚轮：手机端那根摇杆拖一下发一次，一格 = 滚轮咔一下。
///
/// 不加注入锁：它跟打字是两回事，长文本注入时摇杆还得能用，
/// 让它排队等两秒手感就废了。每次调用自己开一个 Enigo 连接，互不干扰。
fn api_scroll(app: &Arc<App>, data: &serde_json::Value) -> Reply {
    let num = |k: &str| data.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    // 夹一下，防止客户端算错量级把桌面滚飞（一格已经不小了）
    let clamp = |v: i64| v.clamp(-10, 10) as i32;
    let (dx, dy) = (clamp(num("dx")), clamp(num("dy")));
    if dx == 0 && dy == 0 {
        return json(200, serde_json::json!({ "ok": true, "dx": 0, "dy": 0 }));
    }
    let dryrun = app.mode == "dryrun";
    match inject::scroll(dx, dy, dryrun) {
        Ok(()) => json(200, serde_json::json!({ "ok": true, "dx": dx, "dy": dy })),
        Err(e) => json(500, serde_json::json!({ "error": format!("{e}") })),
    }
}

fn api_send(app: &Arc<App>, data: &serde_json::Value) -> Reply {
    let text = data.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if text.trim().is_empty() {
        return json(400, serde_json::json!({ "error": "内容为空" }));
    }
    let want_enter = data.get("enter").and_then(|v| v.as_bool()).unwrap_or(false);
    let enter = want_enter && app.enter_enabled();
    let dryrun = app.mode == "dryrun";

    let result = {
        // 注入期间不接第二个请求
        let _guard = app.inject.lock().unwrap();
        inject::type_text(text, dryrun).and_then(|_| {
            if enter {
                std::thread::sleep(Duration::from_millis(50));
                inject::press(inject::KeyName::Enter, 1, dryrun)
            } else {
                Ok(())
            }
        })
    };

    match result {
        Ok(()) => {
            let chars = text.chars().count();
            app.push_log(LogEntry {
                t: now_secs(),
                chars,
                enter,
                // 刻意不记录正文：内存、接口、页面里都不留用户输入原文
            });
            crate::say!("  · 已插入 {chars} 字");
            app.note("send", format!("已插入 {chars} 字"));
            json(200, serde_json::json!({ "ok": true, "chars": chars }))
        }
        Err(e) => {
            let msg = e.0.clone();
            crate::say_err!("  ! {msg}");
            json(500, serde_json::json!({ "error": msg }))
        }
    }
}

fn api_undo(app: &Arc<App>, data: &serde_json::Value) -> Reply {
    let chars = data.get("chars").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let chars = chars.min(20_000);
    let want_enter = data.get("enter").and_then(|v| v.as_bool()).unwrap_or(false);
    let enter = want_enter && app.enter_enabled();
    let total = chars + usize::from(enter);
    let dryrun = app.mode == "dryrun";

    let result = {
        let _guard = app.inject.lock().unwrap();
        inject::press(inject::KeyName::Backspace, total, dryrun)
    };

    match result {
        Ok(()) => {
            crate::say!("  · 已撤销 {chars} 字");
            app.note("undo", format!("已撤销 {chars} 字"));
            json(200, serde_json::json!({ "ok": true, "removed": chars }))
        }
        Err(InjectError(msg)) => json(500, serde_json::json!({ "error": msg })),
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".into())
}
