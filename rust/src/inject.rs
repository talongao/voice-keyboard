//! 字符注入 —— 这就是 Python 版里那个 `Injector` 类。
//!
//! 好处是 enigo 已经把三个平台统一了：Windows 走 `SendInput` +
//! `KEYEVENTF_UNICODE`（和我 Python 版逐行对应），macOS 走
//! `CGEventKeyboardSetUnicodeString`，Linux 走 XTEST（纯 Rust，不需要
//! 再 subprocess 去调 xdotool）。
//!
//! `enigo::Keyboard::text()` 内部已经处理了 `\n` → Return、`\t` → Tab，
//! 以及 UTF-16 代理对（emoji），所以这里不用特判。

use enigo::{Axis, Direction, Enigo, Key, Keyboard, Mouse, Settings};

#[derive(Debug)]
pub struct InjectError(pub String);

impl std::fmt::Display for InjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InjectError {}

fn engine() -> Result<Enigo, InjectError> {
    Enigo::new(&Settings::default())
        .map_err(|e| InjectError(format!("初始化输入模拟失败：{e:?}")))
}

/// 注入一段文本。`dryrun` 时只打印（无图形环境调试用）。
pub fn type_text(text: &str, dryrun: bool) -> Result<(), InjectError> {
    if text.is_empty() {
        return Ok(());
    }
    if dryrun {
        crate::say!("  [dryrun] 将输入: {text:?}");
        return Ok(());
    }
    let mut e = engine()?;
    e.text(text)
        .map_err(|e| InjectError(format!("注入失败：{e:?}")))
}

pub enum KeyName {
    Enter,
    Backspace,
    Tab,
    Esc,
    Up,
    Down,
    Left,
    Right,
}

impl KeyName {
    /// 给 HTTP 用的名字（手机端 `/api/key` 传的就是这些字符串）。
    pub fn parse(s: &str) -> Option<KeyName> {
        Some(match s {
            "enter" => KeyName::Enter,
            "backspace" => KeyName::Backspace,
            "tab" => KeyName::Tab,
            "esc" => KeyName::Esc,
            "up" => KeyName::Up,
            "down" => KeyName::Down,
            "left" => KeyName::Left,
            "right" => KeyName::Right,
            _ => return None,
        })
    }

    fn name(&self) -> &'static str {
        match self {
            KeyName::Enter => "enter",
            KeyName::Backspace => "backspace",
            KeyName::Tab => "tab",
            KeyName::Esc => "esc",
            KeyName::Up => "up",
            KeyName::Down => "down",
            KeyName::Left => "left",
            KeyName::Right => "right",
        }
    }
}
/// 连按某个键若干次。
pub fn press(key: KeyName, times: usize, dryrun: bool) -> Result<(), InjectError> {
    if times == 0 {
        return Ok(());
    }
    if dryrun {
        crate::say!("  [dryrun] 将按下 {} x{times}", key.name());
        return Ok(());
    }
    let mut e = engine()?;
    let k = match key {
        KeyName::Enter => Key::Return,
        KeyName::Backspace => Key::Backspace,
        KeyName::Tab => Key::Tab,
        KeyName::Esc => Key::Escape,
        KeyName::Up => Key::UpArrow,
        KeyName::Down => Key::DownArrow,
        KeyName::Left => Key::LeftArrow,
        KeyName::Right => Key::RightArrow,
    };
    for _ in 0..times {
        e.key(k, Direction::Click)
            .map_err(|e| InjectError(format!("按键失败：{e:?}")))?;
        // 有些应用处理不过来连击
        std::thread::sleep(std::time::Duration::from_millis(12));
    }
    Ok(())
}

/// 滚轮。`dx`/`dy` 单位是「格」（一格 = 鼠标滚轮咔一下）。
///
/// 方向：`dy` 正数 = 往下滚，负数 = 往上；`dx` 正数 = 往右，负数 = 往左。
/// 三个平台都是这个含义——Windows 的 `MOUSEEVENTF_WHEEL` 正负跟直觉是反的，
/// enigo 已经帮我们翻过来了（见 `win_impl.rs` 里那个 `-length`）。
///
/// Linux/X11 上是拿 button 4/5 和 6/7 模拟的，所以水平方向在
/// 部分老程序里可能没反应——那是 X11 的死疾，不是这里写错了。
pub fn scroll(dx: i32, dy: i32, dryrun: bool) -> Result<(), InjectError> {
    if dx == 0 && dy == 0 {
        return Ok(());
    }
    if dryrun {
        crate::say!("  [dryrun] 将滚动 dx={dx} dy={dy}");
        return Ok(());
    }
    let mut e = engine()?;
    ensure_pointer_over_foreground(&mut e); // Windows 上才有效，见函数注释
    if dy != 0 {
        e.scroll(dy, Axis::Vertical)
            .map_err(|e| InjectError(format!("滚动失败：{e:?}")))?;
    }
    if dx != 0 {
        e.scroll(dx, Axis::Horizontal)
            .map_err(|e| InjectError(format!("横向滚动失败：{e:?}")))?;
    }
    Ok(())
}

/// 滚轮是**跟着鼠标位置**走的：鼠标事件给的是「指针底下那个窗口」，
/// 跟键盘焦点没关系（X11 和 Windows 都是这个规矩）。
///
/// 手机上没鼠标，所以 Windows 上先看一眼：指针不在前台窗口里的话，
/// 就把它挪到那个窗口中间。不然用户摇杆摇半天什么都没滚——
/// 因为他的鼠标正停在任务栏或者别的窗口上。
///
/// 只在「确实不在里面」时才动，正常用着的时候指针不会自己跳。
/// 其它平台暂时没做（要拿 X11 的 _NET_ACTIVE_WINDOW + 几何信息，
/// 得再引一层依赖；Linux 上请把鼠标停在要滚的窗口上）。
#[cfg(windows)]
fn ensure_pointer_over_foreground(e: &mut Enigo) {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect};
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return;
        }
        let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        if GetWindowRect(hwnd, &mut r) == 0 {
            return;
        }
        let (cx, cy) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
        let Ok((px, py)) = e.location() else { return };
        if px >= r.left && px < r.right && py >= r.top && py < r.bottom {
            return; // 已经在里头了，别乱动用户的鼠标
        }
        if e.move_mouse(cx, cy, enigo::Coordinate::Abs).is_ok() {
            if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                crate::log::line(format!(
                    "滚动前把鼠标挪到了前台窗口中间 ({cx},{cy})——滚轮跟着鼠标位置走"
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

#[cfg(not(windows))]
fn ensure_pointer_over_foreground(_e: &mut Enigo) {}
