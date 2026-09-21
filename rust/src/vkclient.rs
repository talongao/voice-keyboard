//! 壳子用的小工具。
//!
//! GUI 和 server 在同一个进程里，所以探活/读状态直接读内存就行，
//! 这里只需要系统的部分。将来做 `--headless` 拉起和进程管理时再加。

/// 写进系统剪贴板。（Windows 上托盘的「复制配对链接」用它。）
#[cfg_attr(not(windows), allow(dead_code))]
pub fn copy_text(text: &str) -> bool {
    match arboard::Clipboard::new() {
        Ok(mut cb) => cb.set_text(text.to_string()).is_ok(),
        Err(e) => {
            crate::say_err!("  ! 剪贴板不可用：{e}");
            false
        }
    }
}
