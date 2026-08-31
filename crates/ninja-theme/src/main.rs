//! ninja-theme：官方示例插件（q3）——推完整色板换宿主主题，独立进程。
//!
//! 产品定位（PRODUCT.md「颜色」原语，用户产品决策 2026-08-29）：宿主
//! 内置 One Dark Pro 为不可卸基线配色，**主题切换走插件原语**。本插件
//! 就是那个原语的最小官方示范：连上宿主 → 推一条 `theme.set`（携带
//! 完整色板：背景/前景/光标/选区/分隔条/ANSI 16）→ 常驻等宿主 EOF。
//! 官方不特权——与社区插件走同一套 ADE 协议，只经 Unix socket 交换
//! JSON 帧（ninja-protocol），永不链宿主内部 API。
//!
//! 生命周期（v0 协议子集，未知 `v` 必须退出不猜）：
//!
//! ```text
//! 宿主 spawn（env NINJA_ADE_SOCK；启用即拉起）
//! → connect → theme.set（连接后即推）
//! ← hit          （回 hit.ignore：主题插件不认领命中）
//! ← 其它消息      （一律忽略）
//! ← EOF          （宿主退出/禁用 → 本进程正常收尾退出码 0）
//! ```
//!
//! 宿主侧语义：`theme.set` 应用即全屏重画；本插件连接死亡/被禁用时
//! 宿主回退内置 One Dark Pro 基线（与宿主收层同语义）。
//!
//! 内置色板（One Dark Pro 之外，证明「换主题」真实可换；色值来源注在
//! 各色板定义处）：`one-light` / `solarized-dark` / `solarized-light`。
//! 色板选择：命令行参数 `ninja-theme <name>` 优先，缺省读环境变量
//! `NINJA_THEME`，再缺省 `solarized-dark`。

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use ninja_protocol::frame::{FrameDecoder, encode_frame};
use ninja_protocol::{DecodeError, Hit, HitIgnore, Message, ThemeSet};

/// socket 路径环境变量（宿主拉起时注入；约定见 ninja-protocol 文档）。
const SOCK_ENV: &str = "NINJA_ADE_SOCK";
/// 色板选择环境变量（命令行参数缺省时的回退；测试/E2E 用）。
const THEME_ENV: &str = "NINJA_THEME";
/// 缺省色板。
const DEFAULT_PALETTE: &str = "solarized-dark";

fn main() {
    let code = run();
    std::process::exit(code);
}

/// 返回进程退出码：0 = 正常（socket EOF / 宿主退出）；2 = 环境错；
/// 78 = 协议版本不支持（必须退出、不猜，见 ninja-protocol 契约）。
fn run() -> i32 {
    let Some(sock) = std::env::var_os(SOCK_ENV) else {
        eprintln!("ninja-theme: 缺 {SOCK_ENV}（应由宿主拉起）");
        return 2;
    };
    let name = std::env::args()
        .nth(1)
        .or_else(|| std::env::var(THEME_ENV).ok())
        .unwrap_or_else(|| DEFAULT_PALETTE.to_string());
    let Some(palette) = palette_by_name(&name) else {
        eprintln!(
            "ninja-theme: 未知色板 {name:?}（可用：{}；或设 {THEME_ENV}）",
            PALETTE_NAMES.join(" / ")
        );
        return 2;
    };

    // 宿主先绑 socket 再 spawn 本进程（plugins.rs 顺序），连接重试只兜
    // 调度抖动（同 ninja-preview）。
    let mut stream = None;
    for _ in 0..100 {
        match UnixStream::connect(&sock) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
    let Some(mut stream) = stream else {
        eprintln!("ninja-theme: 连不上 ADE socket {sock:?}");
        return 2;
    };
    eprintln!("ninja-theme: 已连接宿主（v0），推色板 {name:?}");

    // 连接后即推：宿主在分发/泵的任一读窗内消化（plugins.rs）。
    let msg = Message::ThemeSet(palette());
    let frame = match encode_frame(&msg) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("ninja-theme: theme.set 编码失败：{e}");
            return 2;
        }
    };
    if stream.write_all(&frame).is_err() {
        eprintln!("ninja-theme: theme.set 写失败（宿主已断？）");
        return 2;
    }

    // 常驻等 EOF（宿主退出/禁用 = 正常收尾）。宿主发来的消息一律不
    // 认领：hit 回 ignore，其余忽略——主题插件只做一件事。
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return 0, // 宿主退出：正常收尾
            Ok(n) => {
                if decoder.extend(&buf[..n]).is_err() {
                    eprintln!("ninja-theme: 帧缓冲超限，断开");
                    return 2;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                eprintln!("ninja-theme: socket 读失败：{e}");
                return 2;
            }
        }
        while let Some(payload) = decoder.pop() {
            let payload = match payload {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("ninja-theme: 帧级违规（{e}），断开");
                    return 2;
                }
            };
            match Message::decode_plugin(&payload) {
                Ok(Message::Hit(Hit { id, .. })) => {
                    let reply = encode_frame(&Message::HitIgnore(HitIgnore::new(id)))
                        .expect("hit.ignore 编码");
                    if stream.write_all(&reply).is_err() {
                        return 0;
                    }
                }
                Ok(_) => {} // 其余消息：主题插件不关心
                Err(DecodeError::UnsupportedVersion { got, supported }) => {
                    // 契约：不支持的 v 必须立即退出，不猜。
                    eprintln!(
                        "ninja-theme: 协议版本 v{got} 不支持（本实现 v{supported}），退出"
                    );
                    return 78;
                }
                Err(e) => {
                    // 同版本内的坏消息：拒收这一条，不断连。
                    eprintln!("ninja-theme: 丢弃无法解码的消息：{e}");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 内置色板
// ---------------------------------------------------------------------------

/// 全部内置色板名（错误提示/测试用）。
pub const PALETTE_NAMES: &[&str] = &["one-light", "solarized-dark", "solarized-light"];

/// 按名取色板构造器（None = 未知名字）。
fn palette_by_name(name: &str) -> Option<fn() -> ThemeSet> {
    match name {
        "one-light" => Some(one_light),
        "solarized-dark" => Some(solarized_dark),
        "solarized-light" => Some(solarized_light),
        _ => None,
    }
}

/// 16 个 `#rrggbb` 的速记（数组长度由类型钉死）。
fn ansi16(v: [&str; 16]) -> [String; 16] {
    v.map(String::from)
}

/// One Light（Atom One Light，atom.io 官方 one-light-syntax/ui 色板；
/// bright 段为同名 iterm2 预设的亮化变体）。
fn one_light() -> ThemeSet {
    ThemeSet::new(
        "one-light",
        "#fafafa",
        "#383a42",
        "#526fff",
        "#e5e5e5",
        0x99,
        "#d7dae0",
        ansi16([
            "#383a42", "#e45649", "#50a14f", "#c18401", "#4078f2", "#a626a4", "#0184bc",
            "#fafafa", "#4f5666", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd",
            "#56b6c2", "#ffffff",
        ]),
    )
}

/// Solarized Dark（Ethan Schoonover 官方色板 ethanschoonover.com/solarized；
/// ANSI 映射 = 官方 xterm 16 色映射：black=base02、bright 黑段
/// base03/橙/base01/base00/base0/violet/base1/base3）。
fn solarized_dark() -> ThemeSet {
    ThemeSet::new(
        "solarized-dark",
        "#002b36",
        "#839496",
        "#93a1a1",
        "#073642",
        0x66,
        "#586e75",
        ansi16([
            "#073642", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198",
            "#eee8d5", "#002b36", "#cb4b16", "#586e75", "#657b83", "#839496", "#6c71c4",
            "#93a1a1", "#fdf6e3",
        ]),
    )
}

/// Solarized Light（同上官方色板的浅色形态：bg=base3、fg=base00，
/// ANSI 16 色映射黑白段对调）。
fn solarized_light() -> ThemeSet {
    ThemeSet::new(
        "solarized-light",
        "#fdf6e3",
        "#657b83",
        "#586e75",
        "#eee8d5",
        0x66,
        "#93a1a1",
        ansi16([
            "#eee8d5", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198",
            "#002b36", "#fdf6e3", "#cb4b16", "#93a1a1", "#657b83", "#839496", "#6c71c4",
            "#586e75", "#002b36",
        ]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ninja_protocol::PROTOCOL_VERSION;
    use ninja_protocol::frame::FrameDecoder;

    /// 每个内置色板必须是**可直接上线**的 theme.set：v=0、全字段可被
    /// 宿主端解析（这里用同 crate 的编解码 + 宿主同款格式校验重演一
    /// 遍：#rrggbb 恰 6 位、alpha ≤255、恰好 16 色）。
    #[test]
    fn built_in_palettes_are_wire_valid() {
        for name in PALETTE_NAMES {
            let f = palette_by_name(name).expect("名字表与分派一致");
            let m = f();
            assert_eq!(m.v, PROTOCOL_VERSION);
            for field in [
                &m.bg, &m.fg, &m.cursor, &m.selection_bg, &m.divider,
            ] {
                assert_wire_color(name, field);
            }
            assert!(m.selection_alpha <= 255, "{name}: alpha 越界");
            assert_eq!(m.ansi.len(), 16, "{name}: 必须 16 色");
            for c in &m.ansi {
                assert_wire_color(name, c);
            }
            // 与宿主解码器往返（插件发的帧宿主必须能解）。
            let frame = encode_frame(&Message::ThemeSet(m)).unwrap();
            let mut dec = FrameDecoder::new();
            dec.extend(&frame).unwrap();
            let payload = dec.pop().unwrap().unwrap();
            assert_eq!(
                Message::decode_host(&payload).unwrap(),
                Message::ThemeSet(f())
            );
        }
    }

    fn assert_wire_color(name: &str, s: &str) {
        let ok = s.len() == 7
            && s.starts_with('#')
            && s[1..].bytes().all(|b| b.is_ascii_hexdigit());
        assert!(ok, "{name}: {s:?} 不是 #rrggbb");
    }

    /// 换主题必须真实可换：三套色板的背景互不相同，且都不是 ODP 基线
    /// #282c34（宿主像素探针的可判据）。
    #[test]
    fn palettes_actually_differ() {
        let bgs: Vec<String> = PALETTE_NAMES
            .iter()
            .map(|n| palette_by_name(n).unwrap()().bg)
            .collect();
        for i in 0..bgs.len() {
            for j in i + 1..bgs.len() {
                assert_ne!(bgs[i], bgs[j], "色板背景不得撞色");
            }
            assert_ne!(
                bgs[i], "#282c34",
                "内置色板不得与 ODP 基线同背景"
            );
        }
    }

    /// 名字分派：已知名命中、缺省名存在、未知名 None。
    #[test]
    fn palette_dispatch() {
        assert!(palette_by_name(DEFAULT_PALETTE).is_some());
        assert!(palette_by_name("one-dark-pro").is_none(), "ODP 是宿主基线，插件不带");
    }
}
