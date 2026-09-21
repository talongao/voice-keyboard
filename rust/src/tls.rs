//! 自签证书：麦克风模式的硬门槛。
//!
//! 为什么非要有它：浏览器只在**安全上下文**（https 或 localhost）下才给麦克风。
//! 手机访问 `http://192.168.x.x:8765` 属于"不安全"，`getUserMedia` 会被直接拒绝——
//! 这不是我们的 bug，是浏览器的规矩，绕不过去。
//!
//! 设计（一句话）：**没证书就全走 HTTP，有证书就全走 HTTPS。**
//! 证书只在用户开启麦克风模式时生成，存在程序旁边（和 status.json / 日志同目录），
//! 之后每次启动只要发现证书就自动启用 HTTPS。想强制回 HTTP：`--http`。
//!
//! SAN 里会写进本机所有局域网 IP：self-signed 证书必须"名字对得上"，
//! 用 IP 访问就得有 IP SAN，否则即使信任了证书，浏览器照样报错。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 证书存放目录（启动时设一次，和 status.json / 日志同源）
static BASE: OnceLock<PathBuf> = OnceLock::new();

pub fn set_base(p: PathBuf) {
    let _ = BASE.set(p);
}

pub fn base() -> PathBuf {
    BASE.get().cloned().unwrap_or_else(std::env::temp_dir)
}

/// 证书在不在（= 用户开过麦克风模式；这也是"该不该用 HTTPS"的唯一判据）
pub fn cert_exists() -> bool {
    exists(&base())
}

#[derive(Clone)]
pub struct Tls {
    pub cert_pem: Vec<u8>,
    pub key_pem: Vec<u8>,
    /// 证书里已包含的 IP（拿来做"换了网络要不要重新签"的判断）
    pub ips: Vec<String>,
}

fn dir(base: &Path) -> PathBuf {
    base.join("tls")
}

pub fn cert_path(base: &Path) -> PathBuf {
    dir(base).join("cert.pem")
}

pub fn key_path(base: &Path) -> PathBuf {
    dir(base).join("key.pem")
}

/// 证书是否已经存在（= 用户开过麦克风模式）
pub fn exists(base: &Path) -> bool {
    cert_path(base).is_file() && key_path(base).is_file()
}

/// SAN 里都写了哪些名字（存在证书旁边，判断"要不要重新签"用）
pub fn sans_file(base: &Path) -> PathBuf {
    dir(base).join("sans.txt")
}

fn read_sans(base: &Path) -> Vec<String> {
    std::fs::read_to_string(sans_file(base))
        .map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

/// 证书能复用就复用，**别没事重新签**。
///
/// 为什么重要：自签证书的信任是"认这张证书"的——重新签一张，手机上装好的信任就失效了，
/// 用户得再信任一次。所以只有两种情况才重新签：
///   ① 还没有证书  ② 本机 IP 变了（SAN 覆盖不到，按 IP 访问会报名字不匹配）
///
/// 返回 (证书, 是否重新签了)
pub fn generate_or_reuse(base: &Path, ips: &[String]) -> Result<(Tls, bool), String> {
    let want: Vec<String> = {
        let mut v = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        for ip in ips {
            if !v.contains(ip) {
                v.push(ip.clone());
            }
        }
        v
    };

    if exists(base) {
        let old = read_sans(base);
        let covered = !old.is_empty() && want.iter().all(|w| old.contains(w));
        if covered {
            // 现有证书够用：原样返回，手机不需要重新信任
            return Ok((load(base)?, false));
        }
    }
    let t = generate(base, ips)?;
    Ok((t, true))
}

/// 生成一份自签证书，SAN 覆盖所有本机地址
pub fn generate(base: &Path, extra_ips: &[String]) -> Result<Tls, String> {
    let mut sans: Vec<String> = vec!["localhost".into(), "127.0.0.1".into()];
    for ip in extra_ips {
        if !sans.contains(ip) {
            sans.push(ip.clone());
        }
    }

    let ck = rcgen::generate_simple_self_signed(sans.clone())
        .map_err(|e| format!("生成证书失败：{e}"))?;
    let cert_pem = ck.cert.pem();
    let key_pem = ck.key_pair.serialize_pem();

    let d = dir(base);
    std::fs::create_dir_all(&d).map_err(|e| format!("创建 {} 失败：{e}", d.display()))?;
    std::fs::write(cert_path(base), &cert_pem).map_err(|e| format!("写证书失败：{e}"))?;
    std::fs::write(key_path(base), &key_pem).map_err(|e| format!("写私钥失败：{e}"))?;

    // 记下这张证书覆盖了哪些名字，下次据此判断能不能复用
    let _ = std::fs::write(sans_file(base), sans.join("\n"));

    // 私钥文件收紧权限（Windows 上忽略失败）
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(key_path(base), std::fs::Permissions::from_mode(0o600));
    }

    Ok(Tls {
        cert_pem: cert_pem.into_bytes(),
        key_pem: key_pem.into_bytes(),
        ips: sans,
    })
}

/// 读已有的证书
pub fn load(base: &Path) -> Result<Tls, String> {
    let cert_pem = std::fs::read(cert_path(base)).map_err(|e| format!("读证书失败：{e}"))?;
    let key_pem = std::fs::read(key_path(base)).map_err(|e| format!("读私钥失败：{e}"))?;
    Ok(Tls {
        cert_pem,
        key_pem,
        ips: Vec::new(),
    })
}

/// 删掉证书（回到 HTTP）
pub fn remove(base: &Path) -> Result<(), String> {
    let _ = std::fs::remove_file(cert_path(base));
    let _ = std::fs::remove_file(key_path(base));
    let _ = std::fs::remove_file(sans_file(base));
    let _ = std::fs::remove_dir(dir(base));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuse_existing_certificate() {
        let base = std::env::temp_dir().join(format!("vk-tls-reuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let ips = vec!["192.168.1.100".to_string()];

        let (a, made_a) = generate_or_reuse(&base, &ips).unwrap();
        assert!(made_a, "第一次应当新签");
        // 再点一次"开启 HTTPS"：不能换证书，否则手机上的信任会失效
        let (b, made_b) = generate_or_reuse(&base, &ips).unwrap();
        assert!(!made_b, "证书可用时必须复用");
        assert_eq!(a.cert_pem, b.cert_pem, "复用时证书内容必须一模一样");

        // IP 变了 → 必须重签（否则按新 IP 访问会报名字不匹配）
        let (c, made_c) = generate_or_reuse(&base, &["192.168.1.200".to_string()]).unwrap();
        assert!(made_c);
        assert_ne!(a.cert_pem, c.cert_pem);

        remove(&base).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn generate_then_load_roundtrip() {
        let base = std::env::temp_dir().join(format!("vk-tls-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        assert!(!exists(&base));

        let t = generate(&base, &["192.168.1.100".to_string()]).unwrap();
        assert!(t.cert_pem.starts_with(b"-----BEGIN CERTIFICATE"));
        assert!(t.key_pem.starts_with(b"-----BEGIN PRIVATE KEY"));
        assert!(exists(&base));

        let loaded = load(&base).unwrap();
        assert_eq!(loaded.cert_pem, t.cert_pem);

        remove(&base).unwrap();
        assert!(!exists(&base));
        let _ = std::fs::remove_dir_all(&base);
    }
}
