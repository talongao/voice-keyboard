//! 麦克风模式：探测"这台电脑能不能被当成一个麦克风用"，以及装驱动/证书的分步引导。
//!
//! 为什么需要探测：把音频灌进电脑**不是权限问题，是虚拟音频设备问题**。
//! 麦克风模式下，手机把音频发过来，程序必须把它写进一个"看起来像麦克风"的设备，
//! 会议/游戏/直播软件才选得到：
//!
//! - Linux：PipeWire/PulseAudio 自带 null-sink + loopback，**零安装**（程序自己建）
//! - Windows：必须装 VB-CABLE 一类虚拟声卡（或 Voicemeeter）
//! - macOS：必须装 BlackHole，并且装完要重启
//!
//! 所以 `/api/mic/status` 只回答"能不能用 / 缺什么 / 缺了怎么办"，
//! 引导步骤交给 `/api/mic/guide`，手机端拿它一步步显示。

use serde_json::{json, Value};

/// 探测结果
pub struct Probe {
    /// 现在能不能直接用
    pub ok: bool,
    /// 平台名（给界面显示）
    pub platform: &'static str,
    /// 找到的虚拟设备名（None = 没找到）
    pub device: Option<String>,
    /// 不能用时，卡在哪（给用户看的一句话）
    pub reason: String,
}

pub fn platform_name() -> &'static str {
    if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Linux"
    }
}

// ── Linux：PipeWire / PulseAudio ─────────────────────────────
#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use super::Probe;
    use std::process::Command;

    /// 有没有可用的音频服务（pactl 能连上就说明有）
    fn pactl_ok() -> bool {
        Command::new("pactl")
            .arg("info")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn probe() -> Probe {
        let device = "voice-keyboard-mic".to_string();
        if pactl_ok() {
            return Probe {
                ok: true,
                platform: "Linux",
                device: Some(device),
                reason: String::new(),
            };
        }
        Probe {
            ok: false,
            platform: "Linux",
            device: None,
            reason: "未检测到 PulseAudio 或 PipeWire（pactl 无法连接）。".into(),
        }
    }

    /// 自己把虚拟麦克风建起来（Linux 上零安装，这也是我们唯一能全自动的平台）
    pub fn setup() -> Result<String, String> {
        let out = Command::new("pactl")
            .args([
                "load-module",
                "module-null-sink",
                "sink_name=voice-keyboard-mic",
                "sink_properties=device.description=语音键盘麦克风",
            ])
            .output()
            .map_err(|e| format!("执行 pactl 失败：{e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        Ok("已创建虚拟麦克风「语音键盘麦克风」".into())
    }
}

// ── Windows：找 VB-CABLE / Voicemeeter 之类 ──────────────────
#[cfg(windows)]
mod imp {
    use super::Probe;
    use std::process::Command;

    /// 用 PowerShell 列一下声音设备，找常见的虚拟声卡名
    fn find_virtual() -> Option<String> {
        let ps = "Get-CimInstance Win32_SoundDevice | Select-Object -ExpandProperty Name";
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-Command", ps]);
        // 不给它加"无窗口"标志的话，每次探测都会闪一个黑框
        crate::no_window(&mut c);
        let out = c.output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            let l = line.trim();
            for key in ["CABLE", "VB-Audio", "Voicemeeter", "Virtual Audio"] {
                if l.to_ascii_lowercase().contains(&key.to_ascii_lowercase()) {
                    return Some(l.to_string());
                }
            }
        }
        None
    }

    pub fn probe() -> Probe {
        match find_virtual() {
            Some(d) => Probe {
                ok: true,
                platform: "Windows",
                device: Some(d),
                reason: String::new(),
            },
            None => Probe {
                ok: false,
                platform: "Windows",
                device: None,
                reason: "未检测到虚拟声卡（VB-CABLE / Voicemeeter）。Windows 不自带虚拟麦克风。".into(),
            },
        }
    }

    /// Windows 必须装第三方驱动，程序不能替用户装
    pub fn setup() -> Result<String, String> {
        Err("Windows 需手动安装一次虚拟声卡，请按页面提示操作。".into())
    }
}

// ── macOS：找 BlackHole ──────────────────────────────────────
#[cfg(target_os = "macos")]
mod imp {
    use super::Probe;
    use std::path::Path;

    const DRIVER_PATHS: &[&str] = &[
        "/Library/Audio/Plug-Ins/HAL/BlackHole2ch.driver",
        "/Library/Audio/Plug-Ins/HAL/BlackHole16ch.driver",
        "/Library/Audio/Plug-Ins/HAL/BlackHole64ch.driver",
    ];

    pub fn probe() -> Probe {
        for p in DRIVER_PATHS {
            if Path::new(p).exists() {
                return Probe {
                    ok: true,
                    platform: "macOS",
                    device: Some("BlackHole 2ch".into()),
                    reason: String::new(),
                };
            }
        }
        Probe {
            ok: false,
            platform: "macOS",
            device: None,
            reason: "未检测到 BlackHole（macOS 的虚拟音频驱动）。".into(),
        }
    }

    pub fn setup() -> Result<String, String> {
        Err("macOS 需先安装 BlackHole，安装后重启一次。".into())
    }
}

pub fn probe() -> Probe {
    imp::probe()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_returns_platform_and_reason() {
        let p = probe();
        assert!(!p.platform.is_empty());
        if !p.ok {
            assert!(!p.reason.is_empty(), "不可用时必须给出原因");
        }
    }
}

/// 分步引导。手机端照着 steps 一步步显示：
/// 每步有 title / detail，可选 url / command / need_restart。
///
/// 门槛有两个，缺哪个就先引导哪个：
/// ① 手机得信任自签证书（否则浏览器不给麦克风）
/// ② 电脑得装虚拟声卡（Linux 除外，程序自己建）
pub fn guide(tls_on: bool) -> Value {
    let mut steps: Vec<Value> = Vec::new();

    // ① 证书还没就绪 → 先走这一步
    if !tls_on {
        steps.push(json!({
            "title": "开启 HTTPS（生成自签证书）",
            "detail": "浏览器仅在 https 下允许网页使用麦克风，此步骤必需。点击下方按钮即可，程序会自动生成证书，无需手工执行命令。",
            "action": "enable_tls"
        }));
        steps.push(json!({
            "title": "手机信任这张证书",
            "detail": if cfg!(windows) || cfg!(target_os = "macos") {
                "切换后手机会提示「不安全」，需手动信任：iOS 前往「设置 → 通用 → VPN与设备管理」安装描述文件，再到「关于本机 → 证书信任设置」启用；Android 在「设置 → 安全 → 加密与凭据」中安装。仅点击「继续访问」不足以启用麦克风。"
            } else {
                "切换后手机提示「不安全」时，按提示完成信任即可。"
            }
        }));
    }

    // ② 驱动
    let driver: Vec<Value> = if cfg!(windows) {
        vec![
            json!({"title": "下载 VB-CABLE", "detail": "VB-Audio 官方虚拟声卡，免费版即可满足需求（安装后可能需要重启）。",
                   "url": "https://vb-audio.com/Cable/"}),
            json!({"title": "解压并以管理员身份运行 VBCABLE_Setup_x64.exe", "detail": "右键 → 以管理员身份运行，然后点 Install Driver。"}),
            json!({"title": "重启电脑", "detail": "驱动需重启后生效。", "need_restart": true}),
            json!({"title": "返回本页并点击「重新检测」", "detail": "检测到 CABLE 设备后即可开启麦克风模式。"}),
        ]
    } else if cfg!(target_os = "macos") {
        vec![
            json!({"title": "安装 BlackHole", "detail": "macOS 的虚拟音频驱动，2ch 版本即可。",
                   "command": "brew install blackhole-2ch --cask"}),
            json!({"title": "未安装 Homebrew 时手动下载安装", "detail": "下载安装包，双击安装，按提示输入密码。",
                   "url": "https://existential.audio/blackhole/download/"}),
            json!({"title": "重启 Mac", "detail": "音频驱动必须重启后才会被系统加载。", "need_restart": true}),
            json!({"title": "返回本页并点击「重新检测」", "detail": "检测到 BlackHole 后即可开启麦克风模式。"}),
        ]
    } else {
        vec![
            json!({"title": "安装 PipeWire 的 PulseAudio 兼容层", "detail": "多数发行版已自带；Debian/Ubuntu 可用下方命令安装，Fedora/Arch 通常自带。",
                   "command": "sudo apt install pipewire-pulse"}),
            json!({"title": "执行一键创建", "detail": "Linux 无需安装任何驱动：程序会自动创建名为「语音键盘麦克风」的虚拟麦克风。",
                   "action": "setup_mic"}),
        ]
    };
    steps.extend(driver);

    json!({
        "platform": platform_name(),
        "can_auto_setup": cfg!(all(unix, not(target_os = "macos"))),
        "tls_on": tls_on,
        "steps": steps,
    })
}

/// 尝试自动建虚拟设备（只有 Linux 做得到）
pub fn setup() -> Result<String, String> {
    imp::setup()
}
