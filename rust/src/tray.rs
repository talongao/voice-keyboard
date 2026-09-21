//! 托盘图标：Windows 和 macOS 都有，Linux 上是空壳。
//!
//! PC 端就两样东西——托盘 + 浏览器里的控制台页。托盘管三件事：
//! 打开控制台 / 复制配对链接 / 停止服务并退出。
//!
//! # 两条踩过的坑（都很难查，记在这儿）
//!
//! 1. **事件要自己线程阻塞等**，别指望在主循环里轮询：早期版本把动作塞队列、
//!    等 update() 来取，结果窗口收进托盘后 eframe 根本不进帧，菜单点了
//!    18 秒没反应。
//! 2. **主线程必须跑平台的事件循环**：Windows 上托盘/菜单都挂在隐藏窗口上，
//!    点击是发给那个窗口的消息，没人 `GetMessage/DispatchMessage` 连右键菜单
//!    都弹不出来；macOS 上得有 `NSApplication` 在跑，`NSStatusItem` 才存在。
//!    两条都是"图标在，但点什么都没反应"。

#[cfg(any(windows, target_os = "macos"))]
use std::time::Duration;

pub struct Tray {
    /// 真图标的所有权：**丢了托盘图标就没了**，所以得一直攥着
    /// （没有代码去读它，这个字段的存在本身就是作用）。
    #[cfg(any(windows, target_os = "macos"))]
    #[allow(dead_code)]
    icon: Option<tray_icon::TrayIcon>,
}

#[cfg(any(windows, target_os = "macos"))]
impl Tray {
    pub fn new(app: std::sync::Arc<crate::state::App>, rgba: Vec<u8>, w: u32, h: u32) -> Option<Tray> {

        use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
        use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

        crate::log::line("正在创建托盘图标…");
        let menu = Menu::new();
        let show = MenuItem::new("打开控制台", true, None);
        let copy = MenuItem::new("复制配对链接", true, None);
        let quit = MenuItem::new("停止服务并退出", true, None);
        if let Err(e) =
            menu.append_items(&[&show, &copy, &PredefinedMenuItem::separator(), &quit])
        {
            crate::log::line(format!("托盘：菜单项加不进去（{e:?}）"));
            return None;
        }
        let icon = match Icon::from_rgba(rgba, w, h) {
            Ok(i) => i,
            Err(e) => {
                crate::log::line(format!("托盘：图标数据不合法（{e:?}）"));
                return None;
            }
        };
        let mut builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("语音键盘")
            .with_icon(icon);
        // macOS 的菜单栏图标要用模板图标，系统才知道怎么在深浅色下画
        #[cfg(target_os = "macos")]
        {
            builder = builder.with_icon_as_template(true);
        }
        let tray = match builder.build() {
            Ok(t) => t,
            Err(e) => {
                crate::log::line(format!("托盘：图标建不起来（{e:?}）"));
                return None;
            }
        };

        // 这个线程收到事件就直接把活干完——不排队、不等谁轮询。
        // （历史教训：之前把动作塞队列、指望 update() 来取，结果窗口收进托盘后
        //   eframe 根本不进帧，菜单点了 18 秒没反应。）
        let (id_show, id_copy, id_quit) = (show.id().clone(), copy.id().clone(), quit.id().clone());
        let app2 = app.clone();
        std::thread::spawn(move || loop {
            if let Ok(ev) = MenuEvent::receiver().recv_timeout(Duration::from_millis(120)) {
                crate::log::line(format!("托盘事件：菜单项 id={:?}", ev.id.0));
                if ev.id == id_quit {
                    crate::log::line("托盘：停止服务并退出");
                    quit_now();
                } else if ev.id == id_copy {
                    crate::log::line("托盘：复制配对链接");
                    copy_pair_link(&app2);
                } else if ev.id == id_show {
                    crate::log::line("托盘：打开控制台");
                    app2.request_show(); // 主线程收到就去开浏览器
                }
            }
            while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = ev
                {
                    crate::log::line("托盘事件：左键点图标");
                    app2.request_show();
                }
            }
        });

        crate::log::line("托盘图标已创建（菜单：打开控制台 / 复制配对链接 / 停止服务并退出）");
        Some(Tray {
            icon: Some(tray),
        })
    }

}

/// 退出：清状态文件 + 结束进程（不经过任何窗口/事件循环）
#[cfg(any(windows, target_os = "macos"))]
fn quit_now() {
    crate::log::line("退出：停服务并结束进程");
    crate::status::clear();
    std::process::exit(0);
}

/// 把配对链接写进剪贴板
#[cfg(any(windows, target_os = "macos"))]
fn copy_pair_link(app: &std::sync::Arc<crate::state::App>) {
    let url = crate::net::endpoints(app.port)
        .first()
        .map(|e| crate::net::pair_url(e, Some(&app.pair_key), app.pin.as_deref()))
        .unwrap_or_default();
    if url.is_empty() {
        return;
    }
    let ok = crate::vkclient::copy_text(&url);
    crate::log::line(if ok {
        "配对链接已复制到剪贴板"
    } else {
        "复制失败（剪贴板不可用）"
    });
}

/// Linux：没有托盘实现（要做就得引 GTK/AppIndicator 那一套），
/// `new()` 返回 None，服务照跑，界面用浏览器打开 `/console`。
#[cfg(not(any(windows, target_os = "macos")))]
impl Tray {
    pub fn new(_app: std::sync::Arc<crate::state::App>, _rgba: Vec<u8>, _w: u32, _h: u32) -> Option<Tray> {
        None
    }
}

/// 画个托盘图标。不引图片库，直接手搓 64×64 的 RGBA。
///
/// * macOS：菜单栏图标要用**模板图标**（系统只取 alpha，颜色/深浅交给系统），
///   所以画的是"黑色 + 形状在 alpha 里"的键盘轮廓。
/// * Windows：托盘是彩色的，蓝色圆角底 + 白色按键。
pub fn app_icon() -> (Vec<u8>, u32, u32) {
    #[cfg(target_os = "macos")]
    {
        macos_template_icon()
    }
    #[cfg(not(target_os = "macos"))]
    {
        color_icon()
    }
}

/// macOS 的模板图标：alpha = 形状（圆角边框 + 里面几颗按键），RGB 全 0。
/// 系统会按深浅色模式自己着色，所以别画颜色。
#[cfg(target_os = "macos")]
fn macos_template_icon() -> (Vec<u8>, u32, u32) {
    let n = 64usize;
    let mut rgba = vec![0u8; n * n * 4];
    let mut put = |x: usize, y: usize, a: u8| {
        let i = (y * n + x) * 4;
        rgba[i] = 0;
        rgba[i + 1] = 0;
        rgba[i + 2] = 0;
        rgba[i + 3] = a;
    };
    // 圆角矩形边框：距离边缘 6px，线宽 5px，圆角半径 12
    let (m, w, r) = (6.0f32, 5.0f32, 12.0f32);
    for y in 0..n {
        for x in 0..n {
            let fx = x as f32 + 0.5;
            let fy = y as f32 + 0.5;
            // 到内缩矩形边界的有符号距离
            let dx = (fx - 32.0).abs() - (32.0 - m - r);
            let dy = (fy - 32.0).abs() - (32.0 - m - r);
            let dist = if dx > 0.0 && dy > 0.0 {
                (dx * dx + dy * dy).sqrt() - r
            } else {
                dx.max(dy) - r
            };
            if dist.abs() <= w / 2.0 {
                put(x, y, 255);
            }
        }
    }
    // 里面三行小按键
    for (row, count) in [(0usize, 3usize), (1, 4), (2, 3)] {
        let y = 20 + row * 9;
        for i in 0..count {
            let x = 17 + i * 9;
            for yy in y..(y + 5) {
                for xx in x..(x + 6) {
                    if xx < n && yy < n {
                        put(xx, yy, 255);
                    }
                }
            }
        }
    }
    (rgba, n as u32, n as u32)
}

/// Windows 的彩色图标：圆角蓝底 + 三条白色横杠。
#[cfg(not(target_os = "macos"))]
fn color_icon() -> (Vec<u8>, u32, u32) {
    let n = 64usize;
    let mut rgba = vec![0u8; n * n * 4];
    let mut put = |x: usize, y: usize, c: [u8; 3], a: u8| {
        let i = (y * n + x) * 4;
        rgba[i] = c[0];
        rgba[i + 1] = c[1];
        rgba[i + 2] = c[2];
        rgba[i + 3] = a;
    };
    let r = 14.0f32;
    for y in 0..n {
        for x in 0..n {
            let fx = x as f32 + 0.5;
            let fy = y as f32 + 0.5;
            let dx = (fx - 32.0).abs() - (32.0 - r);
            let dy = (fy - 32.0).abs() - (32.0 - r);
            let inside = if dx > 0.0 && dy > 0.0 {
                (dx * dx + dy * dy).sqrt() <= r
            } else {
                true
            };
            if inside {
                put(x, y, [0x4c, 0x8d, 0xff], 255);
            }
        }
    }
    for (row, count) in [(1usize, 3usize), (2, 4), (3, 3)] {
        let y = 18 + row * 9;
        for i in 0..count {
            let x = 16 + i * 11;
            for yy in y..(y + 5) {
                for xx in x..(x + 8) {
                    if xx < n && yy < n {
                        put(xx, yy, [0xff, 0xff, 0xff], 235);
                    }
                }
            }
        }
    }
    (rgba, n as u32, n as u32)
}
