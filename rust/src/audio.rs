//! 麦克风模式的音频通路：把手机发来的 PCM 灌进"电脑上那个虚拟麦克风"。
//!
//! 数据怎么走：手机网页采集 → 每 ~100ms 一小块 16-bit 单声道 PCM(48k)
//! → `POST /api/mic/audio`（裸 body）→ 这里写进虚拟设备 → 会议/游戏/直播软件选它即可。
//!
//! 为什么用小块 POST 而不是 WebSocket：
//! - 不需要新依赖（tiny_http 就够），二进制不长胖——这是这个项目的底线
//! - iOS Safari 不支持流式请求体，但"反复 POST 小块"完全没问题
//! - 局域网 48k×16bit ≈ 768 kbps，块间隔 100ms ⇒ 延迟可接受
//!
//! 后端分平台：
//! - Linux：`pacat` 管子（PipeWire/Pulse 自带，**零安装**）
//! - Windows/macOS：`cpal` 输出到 CABLE Input / BlackHole（这两个必须用户自己装，见 mic.rs）
//! - `--mode dryrun`：只统计字节数不输出，给测试和排障用

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// 采样率/位深/声道：手机端也按这个采集，服务端不做重采样
pub const RATE: u32 = 48_000;
pub const CHANNELS: u16 = 1;

/// 音频会话（一次"开麦"）
pub struct Session {
    backend: Backend,
    pub started: std::time::Instant,
    pub chunks: u64,
    pub bytes: u64,
    pub last_at: std::time::Instant,
}

enum Backend {
    Dryrun,
    #[cfg(all(unix, not(target_os = "macos")))]
    Pacat(std::process::Child),
    #[cfg(any(windows, target_os = "macos"))]
    Cpal(Cpal),
}

/// 统计给接口用（原子量，避免和会话锁打架）
static CHUNKS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();

fn session() -> &'static Mutex<Option<Session>> {
    SESSION.get_or_init(|| Mutex::new(None))
}

pub fn is_running() -> bool {
    session().lock().map(|g| g.is_some()).unwrap_or(false)
}

pub fn stats() -> (u64, u64) {
    (CHUNKS.load(Ordering::Relaxed), BYTES.load(Ordering::Relaxed))
}

/// 开麦。dryrun 下不做任何输出，只统计。
pub fn start(dryrun: bool) -> Result<String, String> {
    let mut g = session().lock().map_err(|_| "会话锁坏了".to_string())?;
    if g.is_some() {
        return Ok("麦克风已经在用了".into());
    }
    CHUNKS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);

    let backend = if dryrun {
        Backend::Dryrun
    } else {
        open_backend()?
    };

    *g = Some(Session {
        backend,
        started: std::time::Instant::now(),
        chunks: 0,
        bytes: 0,
        last_at: std::time::Instant::now(),
    });
    // 看门狗：手机切后台/断网时不会再有数据，5 秒后自动关麦
    std::thread::spawn(|| loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        if !is_running() {
            break;
        }
        if autostop_if_idle(5) {
            crate::log::line("麦克风 5 秒没数据，已自动关闭");
            break;
        }
    });

    Ok(if dryrun {
        "已开始（dryrun：只统计不输出）".into()
    } else {
        "麦克风已打开".into()
    })
}

/// 收一块 PCM
pub fn push(pcm: &[u8]) -> Result<(), String> {
    let mut g = session().lock().map_err(|_| "会话锁坏了".to_string())?;
    let s = g.as_mut().ok_or_else(|| "还没开麦（先调 /api/mic/start）".to_string())?;
    s.chunks += 1;
    s.bytes += pcm.len() as u64;
    s.last_at = std::time::Instant::now();
    CHUNKS.store(s.chunks, Ordering::Relaxed);
    BYTES.store(s.bytes, Ordering::Relaxed);

    match &mut s.backend {
        Backend::Dryrun => Ok(()),
        #[cfg(all(unix, not(target_os = "macos")))]
        Backend::Pacat(child) => {
            use std::io::Write;
            let stdin = child.stdin.as_mut().ok_or_else(|| "pacat 的 stdin 没了".to_string())?;
            stdin.write_all(pcm).map_err(|e| format!("写 pacat 失败：{e}"))
        }
        #[cfg(any(windows, target_os = "macos"))]
        Backend::Cpal(c) => {
            c.push(pcm);
            Ok(())
        }
    }
}

/// 关麦。返回这轮的统计（手机端可以显示"传了多少"）
pub fn stop() -> Result<serde_json::Value, String> {
    let mut g = session().lock().map_err(|_| "会话锁坏了".to_string())?;
    let s = g.take().ok_or_else(|| "本来就没开麦".to_string())?;
    let secs = s.started.elapsed().as_secs_f64();
    drop(s.backend); // Linux 上这里会关掉 pacat（子进程随之退出，不会留僵尸）
    Ok(serde_json::json!({
        "seconds": (secs * 10.0).round() / 10.0,
        "chunks": s.chunks,
        "bytes": s.bytes,
    }))
}

/// 太久没数据就自动关麦（手机切后台/断网时别让 pacat 一直挂着）
pub fn autostop_if_idle(secs: u64) -> bool {
    let mut g = match session().lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let idle = match g.as_ref() {
        Some(s) => s.last_at.elapsed().as_secs() >= secs,
        None => return false,
    };
    if idle {
        if let Some(s) = g.take() {
            drop(s.backend);
        }
        true
    } else {
        false
    }
}

// ── Linux：pacat 管子 ─────────────────────────────────────────
#[cfg(all(unix, not(target_os = "macos")))]
fn open_backend() -> Result<Backend, String> {
    // 设备不存在就先建（Linux 上零安装，程序自己搞定）
    let probe = crate::mic::probe();
    if !probe.ok {
        return Err(format!(
            "{} 先按引导把虚拟麦克风准备好（点「一键创建」）",
            probe.reason
        ));
    }
    let child = std::process::Command::new("pacat")
        .args([
            "--playback",
            "--device=voice-keyboard-mic",
            "--rate=48000",
            "--channels=1",
            "--format=s16le",
            "--latency-msec=80",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("启动 pacat 失败：{e}"))?;
    Ok(Backend::Pacat(child))
}

// ── Windows / macOS：cpal 输出到虚拟声卡 ─────────────────────
#[cfg(any(windows, target_os = "macos"))]
pub struct Cpal {
    buf: std::sync::Arc<Mutex<std::collections::VecDeque<i16>>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(any(windows, target_os = "macos"))]
impl Cpal {
    fn push(&self, pcm: &[u8]) {
        let mut b = match self.buf.lock() {
            Ok(b) => b,
            Err(_) => return,
        };
        // 上限约 2 秒：万一输出设备卡住，别把内存吃光
        if b.len() > (RATE as usize * 2) {
            b.clear();
        }
        for c in pcm.chunks_exact(2) {
            b.push_back(i16::from_le_bytes([c[0], c[1]]));
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
impl Drop for Cpal {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn open_backend() -> Result<Backend, String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let probe = crate::mic::probe();
    let want = probe
        .device
        .clone()
        .ok_or_else(|| format!("{} 先按引导装好虚拟声卡", probe.reason))?;

    let host = cpal::default_host();
    let device = host
        .output_devices()
        .map_err(|e| format!("列不出输出设备：{e}"))?
        .find(|d| {
            d.name()
                .map(|n| n.to_lowercase().contains(&want.to_lowercase()))
                .unwrap_or(false)
        })
        .ok_or_else(|| format!("找不到输出设备「{want}」"))?;

    let cfg = device
        .default_output_config()
        .map_err(|e| format!("读输出配置失败：{e}"))?;
    let channels = cfg.channels() as usize;

    let buf = std::sync::Arc::new(Mutex::new(std::collections::VecDeque::<i16>::new()));
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // cpal 的 Stream 在部分平台上不是 Send，必须在它自己的线程里建、也只能在那里 drop。
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let (buf_t, stop_t) = (buf.clone(), stop.clone());
    let sample_rate = cfg.sample_rate().0;
    let handle = std::thread::spawn(move || {
        let stream = device.build_output_stream(
            &cpal::StreamConfig {
                channels: channels as u16,
                sample_rate: cpal::SampleRate(sample_rate),
                buffer_size: cpal::BufferSize::Default,
            },
            move |out: &mut [i16], _| {
                let mut b = match buf_t.lock() {
                    Ok(b) => b,
                    Err(_) => return,
                };
                let frames = out.len() / channels;
                for i in 0..frames {
                    let v = b.pop_front().unwrap_or(0);
                    for c in 0..channels {
                        // 单声道复制到所有声道（BlackHole 是 2ch）
                        out[i * channels + c] = v;
                    }
                }
            },
            |e| crate::log::line(format!("音频输出出错：{e}")),
            None,
        );
        match stream {
            Ok(s) => {
                if let Err(e) = s.play() {
                    let _ = tx.send(Err(format!("播放失败：{e}")));
                    return;
                }
                let _ = tx.send(Ok(()));
                while !stop_t.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
            Err(e) => {
                let _ = tx.send(Err(format!("打不开输出流：{e}")));
            }
        }
    });

    match rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            // 流没起来，回收线程
            stop.store(true, Ordering::Relaxed);
            let _ = handle.join();
            return Err(e);
        }
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            return Err("打开音频输出超时".into());
        }
    }

    Ok(Backend::Cpal(Cpal {
        buf,
        stop,
        handle: Some(handle),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dryrun_counts_bytes() {
        let _ = stop();
        start(true).unwrap();
        assert!(is_running());
        push(&[0u8; 960]).unwrap();
        push(&[1u8; 960]).unwrap();
        let (chunks, bytes) = stats();
        assert_eq!(chunks, 2);
        assert_eq!(bytes, 1920);
        let s = stop().unwrap();
        assert_eq!(s["chunks"], 2);
        assert_eq!(s["bytes"], 1920);
        assert!(!is_running());
    }

    #[test]
    fn push_without_start_is_error() {
        let _ = stop();
        assert!(push(&[0u8; 16]).is_err());
    }
}
