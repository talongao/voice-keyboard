//! 地址探测与二维码。

use std::net::UdpSocket;

/// 拿本机在默认出口上的局域网 IP。
///
/// 用一个 UDP socket 去 connect 一个外网地址——不发包，只是让内核按路由表
/// 挑一个源地址出来。比枚举网卡简单，也不用引依赖。
pub fn detect_lan_ip() -> Option<String> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    let ip = s.local_addr().ok()?.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        return None;
    }
    Some(ip.to_string())
}

/// 候选地址。目前只有局域网一个。
///
/// 留成 Vec 是因为手机端会「依次尝试」，将来多地址源不用改协议。
/// 本机的局域网地址列表（自签证书的 SAN 要用它，否则手机按 IP 访问会报名字不匹配）
pub fn local_ips() -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if let Some(ip) = detect_lan_ip() {
        v.push(ip);
    }
    v
}

pub fn endpoints(port: u16) -> Vec<String> {
    let scheme = crate::listener::scheme();
    match detect_lan_ip() {
        Some(ip) => vec![format!("{scheme}://{ip}:{port}")],
        None => Vec::new(),
    }
}

/// 把配对凭据塞进 URL hash —— 不发给服务器，也不进浏览记录。
///
/// 优先用 24 字节随机密钥，没有就退回 6 位 PIN。
pub fn pair_url(base: &str, key: Option<&str>, pin: Option<&str>) -> String {
    if let Some(k) = key.filter(|k| !k.is_empty()) {
        return format!("{base}/#k={k}");
    }
    if let Some(p) = pin.filter(|p| !p.is_empty()) {
        return format!("{base}/#pin={p}");
    }
    format!("{base}/")
}

// ═══════════════════════════════════════════════════════════════
//  二维码
// ═══════════════════════════════════════════════════════════════

pub fn matrix(data: &str) -> Option<Vec<Vec<bool>>> {
    let code = qrcode::QrCode::new(data.as_bytes()).ok()?;
    let w = code.width();
    let colors = code.to_colors();
    let mut m = vec![vec![false; w]; w];
    for (i, c) in colors.iter().enumerate() {
        m[i / w][i % w] = *c == qrcode::Color::Dark;
    }
    Some(m)
}

/// 生成 SVG 字符串，直接内联进 HTML。
pub fn svg(data: &str, module_px: u32) -> Option<String> {
    let m = matrix(data)?;
    let n = m.len();
    let quiet = 2usize; // 四周留白，扫码器需要
    let side = (n + quiet * 2) as u32 * module_px;

    let mut s = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{side}\" height=\"{side}\" \
         viewBox=\"0 0 {side} {side}\" shape-rendering=\"crispEdges\">\
         <rect width=\"{side}\" height=\"{side}\" fill=\"#fff\"/><path fill=\"#000\" d=\""
    );
    for (y, row) in m.iter().enumerate() {
        let mut x = 0usize;
        while x < n {
            if row[x] {
                let start = x;
                while x < n && row[x] {
                    x += 1;
                }
                let px = (start + quiet) as u32 * module_px;
                let py = (y + quiet) as u32 * module_px;
                let pw = (x - start) as u32 * module_px;
                s.push_str(&format!("M{px} {py}h{pw}v{module_px}h-{pw}z"));
            } else {
                x += 1;
            }
        }
    }
    s.push_str("\"/></svg>");
    Some(s)
}

/// 终端里打一个能扫的二维码（半块字符，两行并一行）。
pub fn print_terminal(data: &str) -> bool {
    let Some(m) = matrix(data) else {
        return false;
    };
    let n = m.len();
    // 上下各留一行空白，扫码器需要静区
    let blank = " ".repeat(n + 4);
    crate::say!("{blank}");
    for y in (0..n).step_by(2) {
        let mut line = String::from("  ");
        for x in 0..n {
            let top = m[y][x];
            let bot = if y + 1 < n { m[y + 1][x] } else { false };
            // 半块字符上下各一个模块：字符高 2 倍于宽，所以一个模块正好是正方
            line.push(match (top, bot) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        crate::say!("{line}");
    }
    crate::say!("{blank}");
    true
}
