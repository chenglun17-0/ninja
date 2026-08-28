//! p4 系统默认打开：命中无插件认领（或未启用插件）时交给系统默认
//! 处理器。URL/OSC-8 直接 `NSWorkspace openURL`；Path 做 `~` 展开、
//! 剥 `file:line:col` 后缀、相对路径按 shell 的 OSC 7 pwd（缺省 HOME）
//! 拼成绝对路径再转 file URL。**全程没有任何「请安装插件」类提示**——
//! 无插件时的行为与普通终端里 `open` 一致。
//!
//! 取证钩子（非产品功能，仓内惯例）：`NINJA_OPEN_PROBE=<path>` 设置时
//! 不真开，把将打开的 URL 一行追加写进该文件（同 `NINJA_DUMP_ATLAS`
//! 的可注入出口，E2E 断言用）。

use ninja_protocol::HitKind;
use objc2::rc::Retained;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSURL, NSString};

/// OSC 7 pwd（vt 的 `pwd()` 返回**完整 URI**，如 `file://host/Users/jal`）
/// → 文件系统路径（剥 scheme/authority + 百分号解码）。非 file:// 或
/// 解不出以 `/` 起首的路径 → None。p5 修订：hit.cwd 与系统默认回退
/// 都走这里（此前 open.rs 直接把 URI 当路径基，p4 未踩中过 OSC 7）。
pub fn osc7_to_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    // authority 到首个 '/'；无 '/'（只有 host）→ 无路径。split_once 的
    // 右段不含分隔符，补回前导 '/'。
    let path = format!("/{}", rest.split_once('/')?.1);
    if path == "/" {
        return Some(path);
    }
    // 百分号解码（%XX；坏序列保守放回原样）。
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    let decoded = String::from_utf8(out).ok()?;
    if decoded.starts_with('/') {
        Some(decoded)
    } else {
        None
    }
}

/// 把命中交给系统默认处理器。
///
/// `pwd`：shell 经 OSC 7 报告的工作目录（相对路径的解析基；None 用
/// HOME，再没有就放弃相对路径）。
pub fn open_hit_target(kind: HitKind, text: &str, pwd: Option<&str>) {
    let url = match kind {
        HitKind::Url | HitKind::Osc8 => NSURL::URLWithString(&NSString::from_str(text)),
        HitKind::Path => file_url_of_path(text, pwd),
    };
    let Some(url) = url else { return };
    if let Some(probe) = std::env::var_os("NINJA_OPEN_PROBE") {
        let line = url
            .absoluteString()
            .map(|s| s.to_string())
            .unwrap_or_default();
        append_probe_line(&probe, &line);
        return;
    }
    NSWorkspace::sharedWorkspace().openURL(&url);
}

/// Path 文本 → 绝对 file URL：剥 `:line[:col]`、`~` 展开、相对路径
/// 按 pwd（缺省 $HOME）拼绝对。
fn file_url_of_path(text: &str, pwd: Option<&str>) -> Option<Retained<NSURL>> {
    let path = strip_line_col(text);
    let expanded = expand_tilde(path);
    let absolute = if expanded.starts_with('/') {
        expanded
    } else {
        let base = pwd
            .map(str::to_string)
            .or_else(|| std::env::var_os("HOME").map(|h| h.to_string_lossy().into_owned()))
            .filter(|b| !b.is_empty())?;
        // base 去尾斜杠再拼（避免 base//rel）。
        let base = base.trim_end_matches('/');
        format!("{base}/{expanded}")
    };
    if absolute.is_empty() {
        return None;
    }
    Some(NSURL::fileURLWithPath(&NSString::from_str(&absolute)))
}

/// `~/x` → `$HOME/x`；`~` → `$HOME`；`~user/x` 不展开（原样）。
fn expand_tilde(path: &str) -> String {
    expand_tilde_with(path, std::env::var_os("HOME").as_deref())
}

/// 同上，家目录由调用方给（单测不碰进程环境变量）。
fn expand_tilde_with(path: &str, home: Option<&std::ffi::OsStr>) -> String {
    let Some(home) = home else {
        return path.to_string();
    };
    let home = home.to_string_lossy().into_owned();
    expand_tilde_home(path, &home)
}

fn expand_tilde_home(path: &str, home: &str) -> String {
    if path == "~" {
        return home.to_string();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        let home = home.trim_end_matches('/');
        return format!("{home}/{rest}");
    }
    path.to_string()
}

/// 从尾部剥 `:digits`（最多两段：`:line` / `:line:col`）。
/// 与 link.rs 的同名逻辑一致（那边用于分类，这边用于打开）。
fn strip_line_col(s: &str) -> &str {
    let b = s.as_bytes();
    let mut end = b.len();
    for _ in 0..2 {
        let Some(colon) = b[..end].iter().rposition(|&c| c == b':') else {
            return &s[..end];
        };
        let digits = colon + 1..end;
        if digits.is_empty() || !b[digits].iter().all(u8::is_ascii_digit) {
            return &s[..end];
        }
        end = colon;
    }
    &s[..end]
}

/// 追加一行（多命中多次点击各留一行；建不出来就静默——取证钩子失败
/// 不该影响产品路径）。
fn append_probe_line(path: &std::ffi::OsStr, line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc7_uri_decoded_to_path() {
        // vt 契约：pwd() 返回完整 URI。
        assert_eq!(
            osc7_to_path("file://localhost/tmp/project"),
            Some("/tmp/project".to_string())
        );
        assert_eq!(osc7_to_path("file://somehost/"), Some("/".to_string()));
        // 中文/空格目录（shell 通常百分号编码）。
        assert_eq!(
            osc7_to_path("file://h/Users/jal/%E4%B8%AD%E6%96%87%20dir"),
            Some("/Users/jal/中文 dir".to_string())
        );
        // 非 file scheme / 无路径 / 空 → None。
        assert_eq!(osc7_to_path("https://x.io/a"), None);
        assert_eq!(osc7_to_path("file://onlyhost"), None);
        assert_eq!(osc7_to_path(""), None);
    }

    #[test]
    fn line_col_stripped_for_open() {
        assert_eq!(strip_line_col("/a/b/main.rs:42:13"), "/a/b/main.rs");
        assert_eq!(strip_line_col("/a/b/main.rs:42"), "/a/b/main.rs");
        assert_eq!(strip_line_col("/a/b:c/d.txt"), "/a/b:c/d.txt"); // 非数字尾不剥
    }

    #[test]
    fn tilde_expansion() {
        // 纯函数段：不碰进程环境变量（并行测试下 set_var 也不安全）。
        assert_eq!(expand_tilde_home("~", "/Users/test"), "/Users/test");
        assert_eq!(expand_tilde_home("~/x/y.txt", "/Users/test"), "/Users/test/x/y.txt");
        assert_eq!(expand_tilde_home("~/x", "/Users/test/"), "/Users/test/x");
        assert_eq!(expand_tilde_home("~other/x", "/Users/test"), "~other/x"); // 不猜别人家目录
        assert_eq!(expand_tilde_home("/abs/x", "/Users/test"), "/abs/x");
        // HOME 缺失：原样返回（后续拼不出绝对路径就放弃打开）。
        assert_eq!(expand_tilde_with("~/x", None), "~/x");
    }

    #[test]
    fn relative_resolves_against_pwd() {
        // 纯逻辑段（不碰 NSURL 字符串化细节）：与 file_url_of_path 同式。
        let pwd = Some("/Users/jal/my_repos/ninja");
        let text = "src/main.rs:42:13";
        let path = strip_line_col(text);
        let expanded = expand_tilde(path);
        let absolute = if expanded.starts_with('/') {
            expanded
        } else {
            let base = pwd.unwrap().trim_end_matches('/');
            format!("{base}/{expanded}")
        };
        assert_eq!(absolute, "/Users/jal/my_repos/ninja/src/main.rs");
    }
}
