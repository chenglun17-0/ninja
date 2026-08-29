//! T-主题：One Dark Pro —— 唯一内置默认主题。
//!
//! 用户钉死：默认且只有一个主题（VS Code「Atom Family」那套 One Dark
//! Pro）。不做主题系统、不做运行时主题切换，色值直接钉进代码；全部
//! 取自 One Dark Pro 扩展官方主题源
//! `~/.vscode/extensions/zhuangtongfa.material-theme-3.19.0/themes/OneDark-Pro.json`
//! （binaryify/One Dark Pro 发布包），每条常量注明来源键。
//!
//! 收口范围：vt 默认前景/背景/光标 + ANSI 16 色（含 bright，钉进 vt
//! 调色板 0-15）、渲染器选区/光标、pane 容器底色与分隔条。ninja-preview
//! 层像素色（surface_draw.rs）同源同注释。
//!
//! 运行时仍可被程序改写的是 vt 语义、不是主题切换：OSC 10/11 改默认
//! 前景/背景、OSC 4 改调色板、DECSCUSR 改光标样式。渲染跳帧（D-C）的
//! fg/bg 对比兜底比较的是「值变了没有」，不关心值是什么，不受影响。

use libghostty_vt::style::{PaletteIndex, RgbColor};
use libghostty_vt::Terminal;

use crate::term::Rgb;

fn rgb(c: Rgb) -> RgbColor {
    RgbColor { r: c.0, g: c.1, b: c.2 }
}

/// editor.background：`#282c34`
pub const BACKGROUND: Rgb = Rgb(0x28, 0x2C, 0x34);
/// editor.foreground / terminal.foreground：`#abb2bf`
pub const FOREGROUND: Rgb = Rgb(0xAB, 0xB2, 0xBF);
/// editorCursor.foreground：`#528bff`
pub const CURSOR: Rgb = Rgb(0x52, 0x8B, 0xFF);

/// terminal.selectionBackground：`#abb2bf30`——官方是带 alpha 的前景
/// 色。渲染管线本就开 alpha 混合（SourceAlpha/OneMinusSourceAlpha），
/// 选区 quad 用前景色 + 这个 alpha 盖在背景上，所见即官方；
/// 等效不透明合成色 ≈ `#41454E`（shot_text.swift 的选区探针取该值）。
pub const SELECTION_BG: Rgb = Rgb(0xAB, 0xB2, 0xBF);
/// `0x30` / 255。
pub const SELECTION_ALPHA: f32 = 48.0 / 255.0;

/// panel.border / focusBorder / editorGroup.border：`#3e4452`（分隔条
/// 1px 线；容器底仍用 [BACKGROUND]）。
pub const DIVIDER: Rgb = Rgb(0x3E, 0x44, 0x52);

/// ANSI 16 色（terminal.ansi*），下标即调色板 index 0-15：
/// [black, red, green, yellow, blue, magenta, cyan, white,
///  bright black, …, bright white]。
pub const ANSI: [Rgb; 16] = [
    Rgb(0x3F, 0x44, 0x51), // terminal.ansiBlack         #3f4451
    Rgb(0xE0, 0x55, 0x61), // terminal.ansiRed           #e05561
    Rgb(0x8C, 0xC2, 0x65), // terminal.ansiGreen         #8cc265
    Rgb(0xD1, 0x8F, 0x52), // terminal.ansiYellow        #d18f52
    Rgb(0x4A, 0xA5, 0xF0), // terminal.ansiBlue          #4aa5f0
    Rgb(0xC1, 0x62, 0xDE), // terminal.ansiMagenta       #c162de
    Rgb(0x42, 0xB3, 0xC2), // terminal.ansiCyan          #42b3c2
    Rgb(0xD7, 0xDA, 0xE0), // terminal.ansiWhite         #d7dae0
    Rgb(0x4F, 0x56, 0x66), // terminal.ansiBrightBlack   #4f5666
    Rgb(0xFF, 0x61, 0x6E), // terminal.ansiBrightRed     #ff616e
    Rgb(0xA5, 0xE0, 0x75), // terminal.ansiBrightGreen   #a5e075
    Rgb(0xF0, 0xA4, 0x5D), // terminal.ansiBrightYellow  #f0a45d
    Rgb(0x4D, 0xC4, 0xFF), // terminal.ansiBrightBlue    #4dc4ff
    Rgb(0xDE, 0x73, 0xFF), // terminal.ansiBrightMagenta #de73ff
    Rgb(0x4C, 0xD1, 0xE0), // terminal.ansiBrightCyan    #4cd1e0
    Rgb(0xE6, 0xE6, 0xE6), // terminal.ansiBrightWhite   #e6e6e6
];

/// 把主题钉进 vt 核（`TermState::new` 建核后调一次）：默认前景/背景/
/// 光标 + ANSI 16 色调色板。之后程序仍可用 OSC 10/11/4 与 DECSCUSR
/// 覆盖（vt 语义）。全链任一步失败返回 false（调用方只警告，不拒绝
/// 启动——主题是视觉问题，不是启动门禁）。
pub fn apply_to_terminal(terminal: &mut Terminal<'_, '_>) -> bool {
    let ok = terminal
        .set_default_fg_color(Some(rgb(FOREGROUND)))
        .and_then(|t| t.set_default_bg_color(Some(rgb(BACKGROUND))))
        .and_then(|t| t.set_default_cursor_color(Some(rgb(CURSOR))))
        .is_ok();
    // 调色板：从内置默认起步只改 0-15——One Dark Pro 只定义 ANSI 16，
    // 16-255 的色立方/灰阶沿用内置（xterm 256 兼容，vim/htop 依赖）。
    match terminal.default_color_palette() {
        Ok(mut palette) => {
            for (i, c) in ANSI.iter().enumerate() {
                palette.set(PaletteIndex(i as u8), rgb(*c));
            }
            ok && terminal.set_default_color_palette(Some(palette)).is_ok()
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 常量与官方色板的逐条钉死（防「顺手调一下」漂移）。来源键注在
    /// 常量定义处；这里只对十六进制做机械断言。
    #[test]
    fn constants_pin_official_one_dark_pro() {
        assert_eq!(BACKGROUND, Rgb(0x28, 0x2C, 0x34));
        assert_eq!(FOREGROUND, Rgb(0xAB, 0xB2, 0xBF));
        assert_eq!(CURSOR, Rgb(0x52, 0x8B, 0xFF));
        assert_eq!(SELECTION_BG, FOREGROUND, "官方选区 = 前景色 + alpha");
        assert!((SELECTION_ALPHA - 0x30 as f32 / 255.0).abs() < 1e-6);
        assert_eq!(DIVIDER, Rgb(0x3E, 0x44, 0x52));
        // ANSI 16 抽查首尾与 bright 边界。
        assert_eq!(ANSI[0], Rgb(0x3F, 0x44, 0x51));
        assert_eq!(ANSI[7], Rgb(0xD7, 0xDA, 0xE0));
        assert_eq!(ANSI[8], Rgb(0x4F, 0x56, 0x66));
        assert_eq!(ANSI[15], Rgb(0xE6, 0xE6, 0xE6));
        // 16 色齐全（数组长度即钉死，这里只防未来改形状）。
        assert_eq!(ANSI.len(), 16);
    }

    /// 回归（T-主题）：TermState 建核即 One Dark Pro——默认前景/背景
    /// 直接进帧，SGR 30-37/90-97 经调色板解析成官方 ANSI 色。
    /// 这是渲染像素颜色的唯一上游（cell.fg/bg 在 vt 侧已解析成 RGB）。
    #[test]
    fn term_state_boots_with_one_dark_pro() {
        let mut term = crate::term::TermState::new(20, 5, 100).unwrap();
        let mut f = crate::term::Frame {
            cols: 0,
            rows: 0,
            fg: Rgb(255, 255, 255),
            bg: Rgb(0, 0, 0),
            cursor: None,
            cursor_style: libghostty_vt::render::CursorVisualStyle::Block,
            cursor_blinking: false,
            dirty: libghostty_vt::render::Dirty::Clean,
            cells: Vec::new(),
            marked: None,
        };
        term.frame_into(&mut f).unwrap();
        assert_eq!(f.fg, FOREGROUND, "默认前景 = #ABB2BF");
        assert_eq!(f.bg, BACKGROUND, "默认背景 = #282C34");

        // SGR 普色 + bright：vt 经（我们钉的）调色板解析成 RGB。
        term.feed(b"\x1b[31mR\x1b[91mB\x1b[32mG\x1b[0m");
        term.frame_into(&mut f).unwrap();
        assert_eq!(f.cells[0].fg, Some(ANSI[1]), "SGR 31 -> ansiRed #e05561");
        assert_eq!(f.cells[1].fg, Some(ANSI[9]), "SGR 91 -> brightRed #ff616e");
        assert_eq!(f.cells[2].fg, Some(ANSI[2]), "SGR 32 -> ansiGreen #8cc265");
        // SGR 0 后回落默认前景（None = 用帧默认 #ABB2BF）。
        assert_eq!(f.cells[3].fg, None);

        // 背景路径同理：SGR 41 的 cell bg = ansiRed。
        term.feed(b"\r\x1b[41mX");
        term.frame_into(&mut f).unwrap();
        assert_eq!(f.cells[0].bg, Some(ANSI[1]), "SGR 41 -> ansiRed 背景");
    }

    /// 回归（T-主题验收项）：OSC 10/11 颜色查询应答新色。vt 核把应答
    /// 经 on_pty_write 写回 PTY（宿主在 view 接线时直接写 pty，见
    /// view.rs）；这里在同一层拦截应答字节，钉死格式与值——
    /// `rgb:RRRR/GGGG/BBBB`（每通道重复两遍）。
    #[test]
    fn osc_10_11_queries_answer_one_dark_pro() {
        use std::sync::{Arc, Mutex};

        let mut term = crate::term::TermState::new(20, 5, 100).unwrap();
        let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
        let sink = seen.clone();
        let _ = term
            .terminal
            .on_pty_write(move |_t, data| sink.lock().unwrap().extend_from_slice(data));
        term.feed(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\");
        let out = String::from_utf8(seen.lock().unwrap().clone()).unwrap();
        assert_eq!(
            out,
            "\x1b]10;rgb:abab/b2b2/bfbf\x1b\\\x1b]11;rgb:2828/2c2c/3434\x1b\\",
            "OSC 10 = 前景 #ABB2BF，OSC 11 = 背景 #282C34"
        );
    }
}
