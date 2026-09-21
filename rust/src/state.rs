//! 运行时状态。
//!
//! 每个可变部分各自一把锁，而不是一个大 `Mutex<App>`——注入可能要花几秒
//! （长文本 + 逐字符延迟），用一把大锁会把所有 HTTP 请求都堵住。

use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;
use std::time::Instant;

use rand::Rng;

#[derive(Clone, serde::Serialize)]
pub struct LogEntry {
    pub t: f64,
    pub chars: usize,
    pub enter: bool,
}

#[derive(Clone, serde::Serialize)]
pub struct Event {
    pub t: f64,
    pub kind: String,
    pub text: String,
}

pub struct App {
    pub port: u16,
    pub pin: Option<String>,
    pub pair_key: String,
    pub mode: String,
    started: Instant,

    pub allow_enter: Mutex<bool>,
    pub tokens: Mutex<HashSet<String>>,
    pub log: Mutex<VecDeque<LogEntry>>,
    pub events: Mutex<VecDeque<Event>>,

    /// 注入串行化。长文本注入期间不接第二个请求。
    pub inject: Mutex<()>,

    /// 配对尝试的时间戳，用来限流（60 秒内最多 8 次）。
    pub attempts: Mutex<VecDeque<Instant>>,

    /// 「把界面打开」的请求位。
    ///
    /// 界面在浏览器里，所以这就是「再开一次控制台页面」的意思：
    /// 托盘点一下、或者用户又双击了一次 exe（新进程发现已经有实例在跑，
    /// 就请求老进程开页面），都走这个位。
    pub show_request: std::sync::atomic::AtomicBool,
    /// 手机端点了「在电脑上打开引导」：开控制台页时要带上引导
    pub guide_request: std::sync::atomic::AtomicBool,

}

const TOKEN_CHARS: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";

pub fn random_token(n: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..n)
        .map(|_| TOKEN_CHARS[rng.gen_range(0..TOKEN_CHARS.len())] as char)
        .collect()
}

/// 6 位数字 PIN，给「看不到二维码时手输」用。
pub fn random_pin() -> String {
    format!("{:06}", rand::thread_rng().gen_range(0..1_000_000u32))
}

pub fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

impl App {
    pub fn new(port: u16, pin: Option<String>, mode: String, allow_enter: bool) -> Self {
        App {
            port,
            pin,
            pair_key: random_token(32),
            mode,
            started: Instant::now(),
            allow_enter: Mutex::new(allow_enter),
            tokens: Mutex::new(HashSet::new()),
            log: Mutex::new(VecDeque::new()),
            events: Mutex::new(VecDeque::new()),
            inject: Mutex::new(()),
            attempts: Mutex::new(VecDeque::new()),
            show_request: std::sync::atomic::AtomicBool::new(false),
            guide_request: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn request_show(&self) {
        self.show_request
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// 手机端请求"在电脑上打开麦克风引导"
    pub fn request_show_guide(&self) {
        self.guide_request
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.request_show();
    }

    /// 这次要开的控制台页要不要带麦克风引导
    pub fn take_guide_request(&self) -> bool {
        self.guide_request
            .swap(false, std::sync::atomic::Ordering::Relaxed)
    }

    /// 阻塞等到有人要开窗口（托盘「打开主窗口」、或又双击了一次 exe）。
    ///
    /// 窗口关掉之后主线程就停在这儿——服务照跑，手机不断；
    /// 有人要窗口时再开一个新的（比「把藏起来的窗口弄回来」可靠得多）。
    pub fn wait_for_show(&self) {
        loop {
            if self.take_show_request() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    }

    /// 取一次就走（消费掉），返回 true 表示这次要把窗口叫出来。
    pub fn take_show_request(&self) -> bool {
        self.show_request
            .swap(false, std::sync::atomic::Ordering::Relaxed)
    }

    pub fn uptime(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    pub fn enter_enabled(&self) -> bool {
        *self.allow_enter.lock().unwrap()
    }

    pub fn set_enter(&self, on: bool) {
        *self.allow_enter.lock().unwrap() = on;
    }

    pub fn issue_token(&self) -> String {
        let t = random_token(32);
        self.tokens.lock().unwrap().insert(t.clone());
        t
    }

    pub fn check_token(&self, token: &str) -> bool {
        if self.pin.is_none() {
            return true; // --no-auth
        }
        self.tokens.lock().unwrap().contains(token)
    }

    pub fn client_count(&self) -> usize {
        self.tokens.lock().unwrap().len()
    }

    pub fn note(&self, kind: &str, text: impl Into<String>) {
        let ev = Event {
            t: now_secs(),
            kind: kind.to_string(),
            text: text.into(),
        };
        let mut q = self.events.lock().unwrap();
        q.push_front(ev);
        q.truncate(100);
    }

    pub fn push_log(&self, entry: LogEntry) {
        let mut q = self.log.lock().unwrap();
        q.push_front(entry);
        q.truncate(50);
    }

    pub fn log_json(&self, limit: usize) -> Vec<LogEntry> {
        self.log.lock().unwrap().iter().take(limit).cloned().collect()
    }

    pub fn events_json(&self, limit: usize) -> Vec<Event> {
        self.events.lock().unwrap().iter().take(limit).cloned().collect()
    }

    /// 配对尝试限流：60 秒内最多 8 次。
    /// 返回 true 表示这次允许尝试。
    pub fn rate_limit_ok(&self) -> bool {
        let now = Instant::now();
        let mut g = self.attempts.lock().unwrap();
        g.retain(|t| now.duration_since(*t).as_secs() < 60);
        if g.len() >= 8 {
            return false;
        }
        g.push_back(now);
        true
    }
}

impl Default for App {
    fn default() -> Self {
        App::new(0, None, "auto".into(), true)
    }
}
