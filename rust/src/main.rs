//! 语音键盘 · 电脑端（Rust 版）
//!
//! 手机浏览器输入文字 -> 电脑当前光标处插入。
//!
//! 这一版是 Python 版的翻译：逻辑一一对应，但产出单个可执行文件，
//! 不需要 Python 运行时，也不用外挂 xdotool 之类的命令。

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod http;
mod inject;
mod log;
mod net;
mod state;
mod status;
mod tray;
mod vkclient;

use std::process::ExitCode;
use std::sync::Arc;

use state::{random_pin, App};

/// 端口最多往后试几个。
///
/// 8765 被占时不应该直接报错死掉（用户不会知道去改什么），
/// 但也不该无限往后扫——扫太远就真的“不好找”了。
const PORT_SCAN: u16 = 10;

struct Args {
    port: u16,
    host: String,
    mode: String,
    pin: Option<String>,
    allow_enter: bool,
    quiet: bool,
    headless: bool,
    /// 端口被占就换一个。默认开。
    auto_port: bool,
    /// 不要托盘图标
    no_tray: bool,
    /// 启动时不要自动打开控制台页面
    no_browser: bool,
    /// 只查当前在跑的实例，不起服务。
    status_only: bool,
    /// 停掉正在跑的实例。
    stop_only: bool,
    /// 打开当前实例的控制台页面。
    console_only: bool,
}

const USAGE: &str = "\
语音键盘 · 电脑端

用法: voice-keyboard [选项]

  --port <n>       监听端口（默认 8765；被占用时自动往后找空端口）
  --host <addr>    监听地址（默认 0.0.0.0）
  --mode <m>       auto | dryrun（dryrun 只打印不注入，无图形环境调试用）
  --headless       只跑服务：不开托盘、不开浏览器（服务器 / SSH 用）
  --no-tray        不要托盘图标
  --no-browser     启动时不要自动打开控制台页面
  --strict-port    端口被占用就报错退出，不要换端口（防火墙/脚本写死了端口时用）
  --status         打印当前实例的端口 / 地址 / 配对链接（不起服务）
  --console        在浏览器里打开当前实例的控制台页面
  --stop           停掉正在跑的实例（窗口收进托盘后找不到出口时用）
  --no-auth        关闭 PIN 校验（仅限完全可信的网络）
  --no-enter       禁用「发送后回车」功能
  --quiet          不打印横幅（后台模式内部用）
  -h, --help       显示本帮助
\n端口说明：默认 8765；被别的程序占了就依次试 8766、8767…（最多 10 个），
换成了哪个端口会在窗口、终端、日志里都写清楚；`--status` 随时能查到。
";

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        port: 8765,
        host: "0.0.0.0".into(),
        mode: "auto".into(),
        pin: None,
        allow_enter: true,
        quiet: false,
        headless: false,
        auto_port: true,
        no_tray: false,
        no_browser: false,
        status_only: false,
        stop_only: false,
        console_only: false,
    };
    let mut no_auth = false;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--port" => {
                a.port = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--port 需要一个数字")?
            }
            "--host" => a.host = it.next().ok_or("--host 需要一个地址")?,
            "--mode" => {
                let m = it.next().ok_or("--mode 需要一个值")?;
                if m != "auto" && m != "dryrun" {
                    return Err(format!("不支持的 --mode: {m}（只有 auto / dryrun）"));
                }
                a.mode = m;
            }
            "--no-auth" => no_auth = true,
            "--no-enter" => a.allow_enter = false,
            "--headless" => a.headless = true,
            "--quiet" => a.quiet = true,
            "--strict-port" => a.auto_port = false,
            "--no-tray" => a.no_tray = true,
            "--no-browser" => a.no_browser = true,
            "--status" => a.status_only = true,
            "--stop" => a.stop_only = true,
            "--console" => a.console_only = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("未知参数: {other}")),
        }
    }
    a.pin = if no_auth { None } else { Some(random_pin()) };
    Ok(a)
}

fn banner(app: &App, endpoints: &[String], port_note: Option<&str>) {
    let line = "─".repeat(58);
    crate::say!();
    crate::say!("{line}");
    crate::say!("  语音键盘 · 电脑端");
    crate::say!("{line}");
    crate::say!("  注入模式 : {}", app.mode);
    // 端口被占换了端口的话，说清楚——用户可能按老端口配过防火墙/书签
    if let Some(note) = port_note {
        crate::say!("  端口变化 : {note}");
    }
    match &app.pin {
        Some(p) => crate::say!("  配对 PIN : {p}"),
        None => crate::say!("  配对 PIN : 已关闭"),
    }
    // 双击启动的场合，出问题时控制台可能一闪就没了，把日志路径打出来。
    crate::say!("  日志文件 : {}", crate::log::path().display());
    crate::say!();
    if endpoints.is_empty() {
        crate::say!("  ! 没有检测到可用的局域网地址");
    } else {
        crate::say!("  手机扫码（或手动打开）：");
        for e in endpoints {
            crate::say!("      {e}");
        }
        crate::say!();
        let url = net::pair_url(&endpoints[0], Some(&app.pair_key), app.pin.as_deref());
        if !net::print_terminal(&url) {
            crate::say!("  ! 二维码生成失败，手动输入上面的地址 + PIN");
        }
        crate::say!();
        crate::say!("  配对链接（发到手机上点开即可，已含密钥）：");
        for e in endpoints {
            crate::say!("      {}", net::pair_url(e, Some(&app.pair_key), app.pin.as_deref()));
        }
    }
    crate::say!();
    crate::say!("  手机操作：扫码 → 打字/语音 → 发送");
    if cfg!(target_os = "macos") {
        crate::say!("  ! macOS 首次使用需授权：系统设置 → 隐私与安全性 → 辅助功能");
        crate::say!("    （不授权的话：菜单栏图标不出现、注入也会被静默挡掉）");
    }
    crate::say!("{line}");
    crate::say!("  Ctrl+C 停止");
    crate::say!();
}

/// 挑一个能用的端口。
///
/// 三种结果分得很清楚，因为对用户的意义完全不同：
///   * 自己已经开了一个 → 不该再开第二个（两个实例都在等手机连，容易搞混）
///   * 别的东西占着端口 → 往后找一个空的就行，不必打扰用户
///   * 全都被占 → 只能报错
enum Pick {
    Bound(tiny_http::Server, u16, Option<String>),
    AlreadyRunning(u16),
    Failed(String),
}

fn pick_port(host: &str, want: u16, auto: bool) -> Pick {
    let last = if auto { want.saturating_add(PORT_SCAN) } else { want };
    for port in want..=last {
        let bind = format!("{host}:{port}");
        match http::bind_addr(&bind) {
            Ok(server) => {
                let note = (port != want).then(|| format!("{want} 被占用，改用 {port}"));
                return Pick::Bound(server, port, note);
            }
            Err(e) => {
                // 端口被占时先问一句「是你吗」：如果是本程序，
                // 说明用户双击了两次（或者忘了还开着一个）。
                if crate::status::who_is_on(port).as_deref() == Some(http::APP_TAG) {
                    return Pick::AlreadyRunning(port);
                }
                if !auto {
                    return Pick::Failed(e);
                }
                log::line(format!("{port} 用不了（{e}），试下一个"));
            }
        }
    }
    Pick::Failed(format!(
        "{want}～{last} 全都被占用，没有可用端口（用 --port 指定一个，或 --status 看有没有实例在跑）"
    ))
}

fn cmd_status() -> ExitCode {
    let Some(st) = crate::status::read() else {
        crate::say!("没有在运行的实例。");
        crate::say!("（找的是 {}）", crate::status::file().display());
        return ExitCode::from(1);
    };
    let port = st.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
    let alive = crate::status::who_is_on(port).as_deref() == Some(http::APP_TAG);
    let text = |k: &str| -> Option<String> {
        let v = st.get(k)?;
        Some(v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))
    };
    // 标签用中文，宽度得自己排（中文占 2 列，不能靠 {:<8} 对齐）：
    // 下面每行冒号前都是 8 列。
    crate::say!();
    crate::say!("  端口    : {port}");
    if let Some(eps) = st.get("endpoints").and_then(|v| v.as_array()) {
        for (i, e) in eps.iter().enumerate() {
            if let Some(e) = e.as_str() {
                crate::say!("  {}: {e}", if i == 0 { "地址    " } else { "        " });
            }
        }
    }
    for (label, key) in [
        ("配对链接", "pair_url"),
        ("配对 PIN", "pin"),
        ("注入模式", "mode"),
        ("进程 PID", "pid"),
        ("日志    ", "log"),
    ] {
        if let Some(v) = text(key) {
            crate::say!("  {label}: {v}");
        }
    }
    crate::say!("  状态文件: {}", crate::status::file().display());
    crate::say!();
    if !alive {
        crate::say!("  ! 这个文件是旧的：端口 {port} 上没人在跑（已经清掉）");
        crate::status::clear();
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

/// 停掉正在跑的实例。窗口收进托盘之后，万一托盘图标没显示出来，
/// 这就是唯一的出口——不然只能去任务管理器里杀进程。
fn cmd_stop() -> ExitCode {
    let Some(st) = crate::status::read() else {
        crate::say!("没有在运行的实例。");
        return ExitCode::from(1);
    };
    let port = st.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
    if crate::status::who_is_on(port).as_deref() != Some(http::APP_TAG) {
        crate::say!("端口 {port} 上没有实例在跑（状态文件是旧的，已清掉）");
        crate::status::clear();
        return ExitCode::from(1);
    }
    match crate::status::post(port, "/api/shutdown") {
        Ok(_) => {
            std::thread::sleep(std::time::Duration::from_millis(400));
            let gone = crate::status::who_is_on(port).is_none();
            crate::say!(
                "已停止端口 {port} 上的实例{}",
                if gone { "" } else { "（还在退出中，稍等一下）" }
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            crate::say!("停不掉：{e}");
            ExitCode::from(1)
        }
    }
}

/// 打开当前实例的控制台页面（浏览器）。不依赖托盘、不依赖任何 IPC——
/// 直接读 status.json 里记的地址。托盘万一不好使，这是最稳的入口。
fn cmd_console() -> ExitCode {
    let Some(st) = crate::status::read() else {
        println!("没有在运行的实例。先双击 voice-keyboard.exe 启动它。");
        return ExitCode::from(1);
    };
    let port = st.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
    if crate::status::who_is_on(port).as_deref() != Some(http::APP_TAG) {
        println!("端口 {port} 上没有实例在跑（状态文件是旧的，已清掉）");
        crate::status::clear();
        return ExitCode::from(1);
    }
    let url = format!("http://127.0.0.1:{port}/console");
    println!("打开控制台：{url}");
    if open_url(&url) {
        ExitCode::SUCCESS
    } else {
        println!("打不开浏览器，手动访问上面的地址也一样");
        ExitCode::SUCCESS
    }
}

fn main() -> ExitCode {
    // Rust 默认忽略 SIGPIPE，于是「管道被下游关掉」会变成 println! 的错误 →
    // panic。`--status | head` 就中招，看着像程序崩了。恢复成 Unix 惯例：
    // 被下游关掉就安静结束。
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            // 能把参数写错，说明是在终端里敲的，挂回那个终端再报错
            attach_parent_console();
            crate::say_err!("{e}\n");
            crate::say_err!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    // release 版是「无控制台」程序（双击不会有黑框）。只有**命令行模式**才需要
    // 把输出挂回父终端；GUI 模式压根没有控制台，不用也不该去碰它。
    //
    // 为什么要分开：`AttachConsole` 在某些环境下会卡住（wine 里实测必卡，
    // 表现是「正在开窗口…」之后就没动静了）。它只影响输出显示，
    // 不值得让它有机会拖住整个启动流程。
    if args.status_only || args.stop_only || args.headless || args.no_tray {
        attach_parent_console();
    }

    // --status / --stop 只是操作一下，不是启动服务：
    // 别往日志里写「启动」污染上一轮的记录。
    if args.status_only {
        log::install_panic_hook();
        return cmd_status();
    }
    if args.stop_only {
        log::install_panic_hook();
        return cmd_stop();
    }
    if args.console_only {
        log::install_panic_hook();
        return cmd_console();
    }

    // 启动服务：第一件事就把日志打开。后面任何一步挂了都要留得下痕迹——
    // Windows 上双击启动，控制台窗口一关，出错信息就没了
    //（用户看到的就是「打开之后闪退」，然后什么都查不了）。
    let log_path = log::init();
    log::line(format!(
        "参数：port={} host={} mode={} headless={} auto_port={} auth={} enter={}",
        args.port,
        args.host,
        args.mode,
        args.headless,
        args.auto_port,
        args.pin.is_some(),
        args.allow_enter
    ));
    log::line(format!("日志文件：{}", log_path.display()));

    // 先占端口：占到了才谈得上开会窗口、写状态文件。
    let (server, port, port_note) = match pick_port(&args.host, args.port, args.auto_port) {
        Pick::Bound(s, p, note) => (s, p, note),
        Pick::AlreadyRunning(p) => {
            // 顺手把那个实例的窗口叫出来：窗口可能收进托盘了，
            // 用户找不到入口就又双击了一次——那就把窗口还给他。
            let shown = crate::status::post(p, "/api/show").is_ok();
            let msg = if shown {
                format!("已经有一个在跑了（端口 {p}），已经把它的控制台页面打开了。")
            } else {
                format!("已经有一个在跑了（端口 {p}），不再启动第二个。")
            };
            log::line(&msg);
            crate::say!("  {msg}");
            crate::say!("  想看它在哪： voice-keyboard --status");
            if !shown {
                log::error_box("语音键盘 · 已经在运行", &msg);
            }
            return ExitCode::SUCCESS;
        }
        Pick::Failed(e) => {
            log::line(format!("启动失败：{e}"));
            crate::say!("  启动失败：{e}");
            log::error_box("语音键盘 · 启动失败", &format!("{e}\n\n日志：{}", log_path.display()));
            return ExitCode::from(1);
        }
    };
    if let Some(note) = &port_note {
        log::line(note.clone());
    }
    log::line(format!("HTTP 已监听 {}:{}", args.host, port));

    let app = Arc::new(App::new(
        port,
        args.pin.clone(),
        args.mode.clone(),
        args.allow_enter,
    ));

    // 告诉别的程序「现在在哪个端口」——端口会变，光靠约定 8765 靠不住。
    let endpoints = net::endpoints(port);
    let pair_url = endpoints
        .first()
        .map(|e| net::pair_url(e, Some(&app.pair_key), app.pin.as_deref()))
        .unwrap_or_default();
    let mut st = serde_json::json!({
        "app": http::APP_TAG,
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "port": port,
        "requested_port": args.port,
        "host": args.host,
        "mode": args.mode,
        "endpoints": endpoints,
        "pair_url": pair_url,
        "log": log_path,
    });
    if let Some(p) = &app.pin {
        st["pin"] = serde_json::Value::String(p.clone());
    }
    let status_path = crate::status::write(&st);
    match &status_path {
        Some(p) => log::line(format!("状态文件：{}", p.display())),
        None => log::line("! 状态文件写不了，--status 和别的客户端就找不到这个实例了"),
    }
    let _status_guard = crate::status::Guard;

    if !args.quiet {
        banner(&app, &endpoints, port_note.as_deref());
    }

    // macOS：任何 AppKit 调用之前先把 NSApplication 初始化出来（顺序错了会 abort）
    #[cfg(target_os = "macos")]
    let bundled = in_app_bundle();
    #[cfg(target_os = "macos")]
    if bundled {
        macos_init_app();
    }

    // 托盘：这是 PC 端唯一的常驻入口（界面在浏览器里）
    #[cfg(target_os = "macos")]
    let want_tray = !args.no_tray && !args.headless && bundled;
    #[cfg(not(target_os = "macos"))]
    let want_tray = !args.no_tray && !args.headless;

    #[cfg(target_os = "macos")]
    if !want_tray && !args.no_tray && !args.headless {
        log::line(
            "这不是 .app 包 → 跳过托盘（macOS 的状态栏图标要在 app 包里才稳）。\
             想再打开控制台用 --console",
        );
    }

    let _tray = if !want_tray {
        None
    } else {
        let (rgba, w, h) = tray::app_icon();
        match tray::Tray::new(app.clone(), rgba, w, h) {
            Some(t) => Some(t),
            None => {
                #[cfg(any(windows, target_os = "macos"))]
                log::line("托盘图标没建起来（不影响服务；用 --status / --console / --stop 也能管它）");
                #[cfg(not(any(windows, target_os = "macos")))]
                log::line(
                    "这个平台上没有托盘（界面就是浏览器里的控制台页；再打开用 --console）",
                );
                None
            }
        }
    };
    // 地址和配对链接也记一份：窗口没开起来时，这是唯一能拿到连接方式的途径。
    for e in &endpoints {
        log::line(format!(
            "配对链接：{}",
            net::pair_url(e, Some(&app.pair_key), app.pin.as_deref())
        ));
    }

    let headless = args.headless || !has_display();
    if headless {
        if !args.headless && !args.quiet {
            crate::say!("  （没有检测到图形界面，改用 headless 模式）");
        }
        log::line("headless 模式（Ctrl+C 停止）");
        let app2 = app.clone();
        if let Err(e) = http::serve_on(app2, server) {
            log::line(format!("服务异常退出：{e}"));
            log::error_box("语音键盘 · 服务异常退出", &format!("{e}\n\n日志：{}", log_path.display()));
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    // 没有窗口了：服务在后台线程跑，界面就是浏览器里的控制台页。
    //
    // 为什么不要原生窗口：这个程序的活儿（托盘、关窗口、单实例、藏起来再叫回来）
    // 在 eframe 那种"画布框架"上全靠手搓，一步一个坑（藏起来的窗口叫不回来、
    // 事件循环不醒来导致托盘点了没反应、关窗口把服务一起带走……）。
    // 改成"托盘 + 浏览器页"之后，这些问题整个不存在。
    let app_srv = app.clone();
    std::thread::spawn(move || {
        if let Err(e) = http::serve_on(app_srv, server) {
            log::line(format!("HTTP 服务异常退出：{e}"));
        }
    });

    if !args.no_browser {
        open_console(&endpoints, args.port);
    }
    log::line("服务在跑。托盘右键可以打开控制台 / 复制配对链接 / 停止服务并退出");

    // 「有人要控制台就开页面」这件事放后台线程做：
    // 主线程有更重要的活儿——**在 Windows 上抽消息**（见下面注释）。
    let app_show = app.clone();
    let eps = endpoints.clone();
    let port = args.port;
    let quiet = args.quiet;
    std::thread::spawn(move || loop {
        app_show.wait_for_show();
        log::line("有人要控制台，打开页面");
        open_console(&eps, port);
        let _ = quiet;
    });

    // 主线程：Windows 上必须抽消息。
    //
    // 托盘图标和右键菜单都是挂在一个隐藏窗口上的，**点击菜单产生的是
    // 发给那个窗口的 WM_COMMAND**——没人抽消息、没人派发，事件就永远到不了
    // 我们手里（v0.1.7 能用是因为当时 eframe 的事件循环在替我们抽；
    // v0.1.9 把 eframe 删了、主线程改成 sleep 轮询，托盘就哑了）。
    //
    // 所以主线程老老实实做消息循环，其它活（HTTP、托盘事件、开页面）
    // 都在各自的线程里干。
    #[cfg(windows)]
    {
        log::line("主线程进入消息循环（托盘事件靠它派发）");
        win32_message_loop();
        return ExitCode::SUCCESS; // 正常要到 WM_QUIT 才返回
    }
    // macOS：主线程要跑 NSApplication 的事件循环，托盘（NSStatusItem）才会出现，
    // 菜单点击也才会被派发。跟 Windows 那个消息循环是一回事。
    #[cfg(target_os = "macos")]
    {
        if bundled {
            log::line("主线程进入 macOS 事件循环（托盘靠它）");
            macos_run_loop();
            return ExitCode::SUCCESS;
        }
        // 裸二进制：跟 Linux 一样，守住「有人要控制台」这件事就行，
        // 一个 AppKit 调用都不做（做了就是那个断言崩溃）
        loop {
            app.wait_for_show();
            log::line("有人要控制台，打开页面");
            open_console(&endpoints, args.port);
        }
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        // Linux 没有托盘，主线程就守着「有人要控制台」这件事
        loop {
            app.wait_for_show();
            log::line("有人要控制台，打开页面");
            open_console(&endpoints, args.port);
        }
    }
}

/// 是不是跑在 `.app` 包里？
///
/// macOS 上这个判断很关键：状态栏图标（NSStatusItem）必须在**正经的 app 包**里
/// 才稳妥。裸二进制从终端直接跑时碰 AppKit 会直接 abort：
///     Assertion failed: (CGAtomicGet(&is_initialized), function CGSConnectionByID
/// 这是崩溃，不是错误码，try/catch 也拦不住——所以只能提前判断，别去碰它。
#[cfg(target_os = "macos")]
fn in_app_bundle() -> bool {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
        .unwrap_or(false)
}

/// macOS：先把 NSApplication 初始化出来（Accessory 模式：菜单栏有图标、Dock 里不出现）。
///
/// **必须在任何 AppKit 调用之前**做——包括创建托盘。Windows 上没这个讲究，
/// macOS 上顺序错了就是那个 CGSConnectionByID 断言。
#[cfg(target_os = "macos")]
fn macos_init_app() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    let Some(mtm) = MainThreadMarker::new() else {
        log::line("macOS：不在主线程上，没法初始化 NSApplication");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    // **这一步很关键**：把启动流程走完（会触发 applicationDidFinishLaunching）。
    // 没走完就建状态栏图标的话，图标建了也不显示，而且不报任何错 ——
    // 现象就是"菜单栏没有图标，一切看起来正常"。
    app.finishLaunching();
    log::line("macOS：NSApplication 已初始化并完成启动（finishLaunching）");
}

/// macOS 的事件循环：Accessory 模式（菜单栏有图标，Dock 里不出现）。
#[cfg(target_os = "macos")]
fn macos_run_loop() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    let Some(mtm) = MainThreadMarker::new() else {
        log::line("不在主线程上，起不了 macOS 事件循环");
        return;
    };
    NSApplication::sharedApplication(mtm).run();
}

/// Windows 消息循环：托盘/菜单的窗口过程靠它派发。
/// 退出走 `process::exit`（托盘菜单里那个），所以这里基本不会返回。
#[cfg(windows)]
fn win32_message_loop() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, TranslateMessage, MSG,
    };
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        // 返回 0 表示收到 WM_QUIT，-1 表示出错
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// 在默认浏览器里打开控制台页面。
///
/// 控制台页（`/console`）只允许本机访问，所以它就是"PC 端的界面"：
/// 二维码、地址、PIN、开关都在上面。
fn open_console(endpoints: &[String], port: u16) {
    let url = format!("http://127.0.0.1:{port}/console");
    log::line(format!("打开控制台：{url}"));
    if !open_url(&url) {
        log::line("打不开浏览器，手动访问上面的地址也一样");
        for e in endpoints {
            crate::say!("  控制台（电脑本机打开）： http://127.0.0.1:{port}/console");
            crate::say!("  手机用： {e}");
        }
    }
}

fn open_url(url: &str) -> bool {
    #[cfg(windows)]
    let r = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    match r {
        // 必须回收：不 wait 的话 xdg-open/open 退出后会留下僵尸子进程
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            true
        }
        Err(_) => false,
    }
}

/// 从已有终端启动时，把标准输出挂回那个终端。
///
/// release 版在 Windows 上是「无控制台」子系统（`windows_subsystem = "windows"`），
/// 双击不会有黑框，代价是 stdout 默认没地方去。这一句把父进程的控制台接过来：
/// 从 cmd / PowerShell 跑就有输出，双击启动则失败（没父控制台），什么都不显示。
#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}

/// 粗略判断有没有图形界面。
fn has_display() -> bool {
    if cfg!(target_os = "windows") || cfg!(target_os = "macos") {
        return true;
    }
    std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
}
