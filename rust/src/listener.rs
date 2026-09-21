//! 监听器的生命周期，以及**运行中换协议**（HTTP ↔ HTTPS）。
//!
//! 为什么要支持运行中切换：麦克风模式必须 HTTPS（浏览器只在安全上下文给麦克风），
//! 但默认又不想让所有人先过一遍证书——所以策略是
//! **没证书就全 HTTP，有证书就全 HTTPS**。用户在手机上点「开启麦克风模式」时，
//! 服务端现场签发自签证书并把监听器从 HTTP 换成 HTTPS，同一个端口、同一个配对令牌，
//! 不需要重启进程、不需要重新配对（手机只要刷新到 https 地址即可）。
//!
//! 实现要点：`tiny_http::Server` 不是 Clone，但 `unblock()` 只要 `&self`。
//! 所以用 `Arc<Server>` 交给 serve 线程（它只需要 `&Server`），
//! 主监督循环留着同一个 Arc 用来 unblock，交换完成后再 drop 掉旧监听器释放端口。

use std::path::PathBuf;
use std::sync::atomic::{AtomicI8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tiny_http::Server;

use crate::state::App;
use crate::{http, tls};

/// 待处理的协议切换请求：0 = 不动，1 = 要开 HTTPS，-1 = 要回 HTTP
static WANT: AtomicI8 = AtomicI8::new(0);

/// 当前是否在用 HTTPS（给状态接口看的）
static TLS_ON: AtomicI8 = AtomicI8::new(0);

pub fn tls_on() -> bool {
    TLS_ON.load(Ordering::Relaxed) == 1
}

/// 请求换协议。真正的切换由监督循环在 200ms 内完成，
/// 这样 HTTP 响应能先发出去（手机先拿到"好了"，再跳 https）。
pub fn request_tls(on: bool) {
    WANT.store(if on { 1 } else { -1 }, Ordering::Relaxed);
}

/// 按证书是否在，决定现在该用什么协议
pub fn should_use_tls(cert_base: &std::path::Path, force_http: bool) -> bool {
    !force_http && tls::exists(cert_base)
}

fn bind(bind_addr: &str, use_tls: bool, cert_base: &std::path::Path) -> Result<Server, String> {
    if !use_tls {
        return http::bind_addr(bind_addr);
    }
    let t = tls::load(cert_base)?;
    http::bind_https(bind_addr, &t)
}

/// 跑服务，并在收到切换请求时原地换协议。正常情况下永不返回。
/// `initial`：启动时已经绑好的监听器（HTTP，调用方为了占端口先绑的）。
/// 传 None 表示按协议自己绑（要 HTTPS 时的情况）。
pub fn run(
    app: Arc<App>,
    initial: Option<Server>,
    bind_addr: String,
    cert_base: PathBuf,
    force_http: bool,
    on_switch: impl Fn(bool) + Send + 'static,
) -> Result<(), String> {
    let mut use_tls = initial.is_none() && should_use_tls(&cert_base, force_http);
    let mut cur: Option<Arc<Server>> = initial.map(Arc::new);
    if cur.is_some() {
        TLS_ON.store(0, Ordering::Relaxed);
    }

    loop {
        if cur.is_none() {
            let srv = bind(&bind_addr, use_tls, &cert_base)?;
            cur = Some(Arc::new(srv));
            TLS_ON.store(if use_tls { 1 } else { 0 }, Ordering::Relaxed);
        }
        let srv = cur.clone().expect("刚放过");

        let app_srv = app.clone();
        let srv_serve = srv.clone();
        let h = std::thread::spawn(move || {
            if let Err(e) = http::serve_on(app_srv, &srv_serve) {
                crate::log::line(format!("服务异常退出：{e}"));
            }
        });

        // 等切换请求（200ms 一轮：响应先出去，再换协议）
        loop {
            if WANT.load(Ordering::Relaxed) != 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }

        let want = WANT.swap(0, Ordering::Relaxed);
        let want_tls = want == 1;
        srv.unblock();
        let _ = h.join();
        drop(srv);
        cur = None; // 旧监听器在这里真正释放端口

        // 端口可能还没立刻释放，重试几次
        let mut backoff = 0;
        loop {
            match bind(&bind_addr, want_tls, &cert_base) {
                Ok(s) => {
                    // 先确认新监听器能用，再换状态
                    use_tls = want_tls;
                    TLS_ON.store(if use_tls { 1 } else { 0 }, Ordering::Relaxed);
                    cur = Some(Arc::new(s));
                    break;
                }
                Err(e) => {
                    backoff += 1;
                    if backoff > 25 {
                        return Err(format!("换协议失败（{e}）"));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
        on_switch(use_tls);
    }
}
