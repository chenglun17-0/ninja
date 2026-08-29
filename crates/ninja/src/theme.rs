//! T-主题：One Dark Pro —— 唯一内置默认主题。
//!
//! 用户钉死（2026-08-29）：默认且只有一个主题（VS Code「Atom Family」
//! 那套 One Dark Pro）。不做内置主题系统、不做宿主侧主题切换 UI，
//! 色值直接钉进代码；全部取自 One Dark Pro 扩展官方主题源
//! `~/.vscode/extensions/zhuangtongfa.material-theme-3.19.0/themes/OneDark-Pro.json`
//! （binaryify/One Dark Pro 发布包），每条常量注明来源键。
//!
//! 收口范围：vt 默认前景/背景/光标 + ANSI 16 色（含 bright，钉进 vt
//! 调色板 0-15）、渲染器选区/光标、pane 容器底色与分隔条。ninja-preview
//! 层像素色（surface_draw.rs）同源同注释。
//!
//! T2（2026-08，同一轮用户产品决策）：主题切换走**插件原语**（协议
//! `theme.set`，见 ninja-protocol「版本与演化规则」第 6 条）——本模块
//! 新增运行时色板覆盖点 [`current`]：无插件覆盖 = 纯 ODP 基线；插件
//! （如官方 ninja-theme）连上后推 theme.set 换 [`Palette`]；插件连接
//! 死亡/禁用时回退基线（与 p6 收层同语义，plugins.rs 接线）。内置基线
//! 不可卸不可改：ODP 常量永远在，回退永远有落点。
//!
//! 运行时仍可被程序改写的是 vt 语义、不是主题切换：OSC 10/11 改默认
//! 前景/背景、OSC 4 改调色板、DECSCUSR 改光标样式。渲染跳帧（D-C）的
//! fg/bg 对比兜底比较的是「值变了没有」，不关心值是什么，不受影响；
//! T2 换色板时 vt 侧强制 Full 脏（见 term.rs），全屏颜色变化不会被
//! 跳帧吃掉。

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

/// 把色板钉进 vt 核（`TermState::new` 建核后调一次；T2 起换色板时对
/// 每个存活 TermState 重调）：默认前景/背景/光标 + ANSI 16 调色板。
/// 之后程序仍可用 OSC 10/11/4 与 DECSCUSR 覆盖（vt 语义）。全链任一
/// 步失败返回 false（调用方只警告，不拒绝启动——主题是视觉问题，
/// 不是启动门禁）。
pub fn apply_to_terminal(terminal: &mut Terminal<'_, '_>) -> bool {
    apply_palette_to(terminal, &current())
}

/// [`apply_to_terminal`] 的实现核心（色板可注入；T2 运行时换色走同一
/// 函数，保证「启动钉入」与「运行时覆盖」行为一致）。调色板 16-255
/// 的色立方/灰阶不动（ODP 只定义 ANSI 16，16-255 沿用内置，xterm 256
/// 兼容，vim/htop 依赖）。
pub fn apply_palette_to(terminal: &mut Terminal<'_, '_>, p: &Palette) -> bool {
    let ok = terminal
        .set_default_fg_color(Some(rgb(p.fg)))
        .and_then(|t| t.set_default_bg_color(Some(rgb(p.bg))))
        .and_then(|t| t.set_default_cursor_color(Some(rgb(p.cursor))))
        .is_ok();
    match terminal.default_color_palette() {
        Ok(mut palette) => {
            for (i, c) in p.ansi.iter().enumerate() {
                palette.set(PaletteIndex(i as u8), rgb(*c));
            }
            ok && terminal.set_default_color_palette(Some(palette)).is_ok()
        }
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// T2 运行时色板覆盖点：插件 theme.set → 全局生效色板
// ---------------------------------------------------------------------------

/// 一套完整色板（内置基线与插件覆盖同一形态）。字段即协议 `theme.set`
/// 的语义集：背景/前景/光标/选区/分隔条 + ANSI 16。
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    /// 色板名（日志/取证用）。
    pub name: String,
    pub bg: Rgb,
    pub fg: Rgb,
    pub cursor: Rgb,
    pub selection_bg: Rgb,
    /// 选区不透明度 0-255（渲染时 /255）。
    pub selection_alpha: u8,
    pub divider: Rgb,
    pub ansi: [Rgb; 16],
}

/// 内置基线：One Dark Pro（不可卸、不可改；常量见上）。
pub fn one_dark_pro() -> Palette {
    Palette {
        name: "one-dark-pro".into(),
        bg: BACKGROUND,
        fg: FOREGROUND,
        cursor: CURSOR,
        selection_bg: SELECTION_BG,
        selection_alpha: 0x30,
        divider: DIVIDER,
        ansi: ANSI,
    }
}

/// 生效中的插件覆盖（`owner` = 拥有者插件连接 id；该连接死亡/禁用时
/// 回退基线，plugins.rs 接线）。只在主线程碰（与插件读写同一套线程
/// 纪律）；static Mutex 仅为满足 static 要求。
struct Override {
    palette: Palette,
    owner: u64,
}

static OVERRIDE: std::sync::Mutex<Option<Override>> = std::sync::Mutex::new(None);

/// 测试互斥：OVERRIDE 全局，改它/断言它的测试串行。
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// **当前生效色板**（T2 覆盖点：渲染/容器/vt 钉入统一读这里，不再读
/// 编译期常量）。无插件覆盖 = 纯 One Dark Pro 基线。锁毒化（不可能
/// 发生）保守回基线：主题不构成门禁。
pub fn current() -> Palette {
    OVERRIDE
        .lock()
        .ok()
        .and_then(|o| o.as_ref().map(|ov| ov.palette.clone()))
        .unwrap_or_else(one_dark_pro)
}

/// 是否有插件色板生效（泵 timer 的启停依据之一，见 plugins.rs）。
pub fn override_active() -> bool {
    OVERRIDE.lock().map(|o| o.is_some()).unwrap_or(false)
}

/// 插件 theme.set 落地（last-writer-wins，覆盖者也换 owner）。返回
/// 是否真的变了（变了才值得重画）。只在插件帧处置路径调，宿主内部
/// 不调。
pub fn apply_plugin(palette: Palette, owner: u64) -> bool {
    match OVERRIDE.lock() {
        Ok(mut slot) => {
            let changed = slot
                .as_ref()
                .map(|ov| ov.palette != palette)
                .unwrap_or(true);
            *slot = Some(Override { palette, owner });
            changed
        }
        Err(_) => false,
    }
}

/// 指定连接的覆盖回收（连接死亡/禁用；非 owner 不动——别的插件可能
/// 已接手）。返回是否发生了回退（变了才值得重画）。
pub fn revoke_owner(owner: u64) -> bool {
    match OVERRIDE.lock() {
        Ok(mut slot) => match slot.take() {
            Some(ov) if ov.owner == owner => true,
            Some(ov) => {
                *slot = Some(ov); // 非本人：放回去
                false
            }
            None => false,
        },
        Err(_) => false,
    }
}

/// 全量回收（同会话禁用/宿主退出：所有连接都在断，覆盖必回基线）。
pub fn revoke_all() -> bool {
    OVERRIDE.lock().map(|mut s| s.take().is_some()).unwrap_or(false)
}

/// 协议 `#rrggbb`（恰好 6 位十六进制，大小写均可）→ [`Rgb`]。不收
/// `#abc` 短写/`0x` 前缀（协议钉死 6 位；宿主解析失败 = 载荷语义坏，
/// plugins.rs 整条忽略）。
pub fn parse_hex_color(s: &str) -> Option<Rgb> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let hi = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some(Rgb(hi(0)?, hi(2)?, hi(4)?))
}

/// 协议 `theme.set` → [`Palette`]。色值格式坏/alpha 越界 = None
/// （调用方警告 + 忽略，不断连：坏的是值，不是协议）。
pub fn palette_from_wire(m: &ninja_protocol::ThemeSet) -> Option<Palette> {
    let a = m.selection_alpha;
    if a > 255 {
        return None;
    }
    let mut ansi = [Rgb(0, 0, 0); 16];
    for (dst, src) in ansi.iter_mut().zip(m.ansi.iter()) {
        *dst = parse_hex_color(src)?;
    }
    Some(Palette {
        name: m.name.clone(),
        bg: parse_hex_color(&m.bg)?,
        fg: parse_hex_color(&m.fg)?,
        cursor: parse_hex_color(&m.cursor)?,
        selection_bg: parse_hex_color(&m.selection_bg)?,
        selection_alpha: a as u8,
        divider: parse_hex_color(&m.divider)?,
        ansi,
    })
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
    /// T2 起与运行时覆盖测试共享全局 OVERRIDE：先拿串行锁。
    #[test]
    fn term_state_boots_with_one_dark_pro() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

    // ------------------------------------------------------------------
    // T2 运行时色板覆盖点（全局 OVERRIDE：串行）
    // ------------------------------------------------------------------

    /// 协议样例色板（solarized-dark，与 golden 同值）：构造 theme.set。
    fn wire_solarized_dark() -> ninja_protocol::ThemeSet {
        ninja_protocol::Message::sample_messages()
            .into_iter()
            .find_map(|m| match m {
                ninja_protocol::Message::ThemeSet(t) => Some(t),
                _ => None,
            })
            .expect("sample 集含 theme.set")
    }

    /// 基线即 ODP：空载（无覆盖）current() == one_dark_pro()，逐字段
    /// 等于 T1 钉死的常量。
    #[test]
    fn baseline_current_is_one_dark_pro() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert!(!override_active());
        let p = current();
        assert_eq!(p, one_dark_pro());
        assert_eq!(p.bg, Rgb(0x28, 0x2C, 0x34));
        assert_eq!(p.fg, Rgb(0xAB, 0xB2, 0xBF));
        assert_eq!(p.cursor, Rgb(0x52, 0x8B, 0xFF));
        assert_eq!(p.selection_bg, FOREGROUND);
        assert_eq!(p.selection_alpha, 0x30);
        assert_eq!(p.divider, Rgb(0x3E, 0x44, 0x52));
        assert_eq!(p.ansi, ANSI);
    }

    /// 覆盖与回退语义：apply 生效（current 跟随）、owner 死亡回基线、
    /// 非 owner 死亡不动（别的插件已接手）、revoke_all 兜底。
    #[test]
    fn plugin_override_apply_and_revert() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let pal = palette_from_wire(&wire_solarized_dark()).expect("golden 色板必须有效");
        assert!(apply_plugin(pal.clone(), 7), "首次落地应视为变化");
        assert!(override_active());
        assert_eq!(current(), pal, "current 跟随插件色板");

        // 同值重推：不再视为变化（重画可省）。
        assert!(!apply_plugin(pal.clone(), 7));

        // 非 owner 回收：不动（owner 7 还在）。
        assert!(!revoke_owner(9));
        assert_eq!(current(), pal);

        // 换 owner（last-writer-wins）：旧 owner 死亡不再回退。
        let pal2 = Palette { name: "x".into(), ..pal.clone() };
        assert!(apply_plugin(pal2.clone(), 9));
        assert!(!revoke_owner(7), "旧 owner 死亡不回退（9 已接手）");
        assert_eq!(current(), pal2);

        // owner 死亡：回基线。
        assert!(revoke_owner(9));
        assert!(!override_active());
        assert_eq!(current(), one_dark_pro());

        // revoke_all 兜底（禁用路径）：有覆盖时返回 true。
        assert!(apply_plugin(pal, 3));
        assert!(revoke_all());
        assert_eq!(current(), one_dark_pro());
    }

    /// 协议色板解析：格式坏/alpha 越界 → None（宿主整条忽略的依据）。
    #[test]
    fn palette_from_wire_rejects_bad_values() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // 合法（大小写混合十六进制也收，规范化由 from_hex 完成）。
        let mut m = wire_solarized_dark();
        m.bg = "#002B36".into();
        assert!(palette_from_wire(&m).is_some());

        // 坏格式：短写 / 无 # / 0x 前缀 / 非十六进制。
        for bad in ["#002b3", "002b36", "0x002b36", "#002g36", "#002b3gg", ""] {
            m.bg = bad.into();
            assert!(palette_from_wire(&m).is_none(), "bg={bad:?} 应拒收");
        }
        // alpha 越界（u32 线类型，语义上限 255）。
        let mut m2 = wire_solarized_dark();
        m2.selection_alpha = 256;
        assert!(palette_from_wire(&m2).is_none());
        m2.selection_alpha = 255;
        assert!(palette_from_wire(&m2).is_some());
        // ansi 里混一个坏值 → 整条拒。
        let mut m3 = wire_solarized_dark();
        m3.ansi[4] = "#12345".into();
        assert!(palette_from_wire(&m3).is_none());
    }

    /// 换色板对 vt 的完整链路（T2 核心）：TermState 换色后——帧默认
    /// fg/bg 换新、SGR 31 解析出新调色板红、OSC 11 查询应答新背景；
    /// 已在屏的 SGR 色 cell 也重解析（强制 Full 重解码，缓存不吃掉）。
    #[test]
    fn apply_effective_palette_re_resolves_cells_and_osc() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let pal = palette_from_wire(&wire_solarized_dark()).unwrap();
        let mut term = crate::term::TermState::new(20, 5, 100).unwrap();
        term.feed(b"\x1b[31mR");
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
        assert_eq!(f.bg, BACKGROUND, "建核 = ODP 基线");
        assert_eq!(f.cells[0].fg, Some(ANSI[1]), "SGR 31 = ODP red");

        // 插件覆盖 → 每个 pane 重钉 + 强制全量重解码。
        assert!(apply_plugin(pal.clone(), 1));
        term.apply_effective_palette();
        term.frame_into(&mut f).unwrap();
        assert_eq!(f.fg, pal.fg, "帧默认前景 = solarized fg");
        assert_eq!(f.bg, pal.bg, "帧默认背景 = solarized bg #002b36");
        assert_eq!(f.cells[0].fg, Some(pal.ansi[1]), "SGR 31 重解析 = solarized red #dc322f");
        assert_eq!(
            f.dirty, libghostty_vt::render::Dirty::Full,
            "换色板帧必须 Full 脏（跳帧不吃全屏换色）"
        );

        // OSC 11 查询应答新背景（真实链路经 on_pty_write 回 PTY）。
        use std::sync::{Arc, Mutex};
        let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
        let sink = seen.clone();
        let _ = term
            .terminal
            .on_pty_write(move |_t, data| sink.lock().unwrap().extend_from_slice(data));
        term.feed(b"\x1b]11;?\x1b\\");
        let out = String::from_utf8(seen.lock().unwrap().clone()).unwrap();
        assert_eq!(out, "\x1b]11;rgb:0000/2b2b/3636\x1b\\", "OSC 11 = #002b36");

        // 覆盖回收 → 回 ODP（同样全量重解析）。
        assert!(revoke_owner(1));
        term.apply_effective_palette();
        term.frame_into(&mut f).unwrap();
        assert_eq!(f.bg, BACKGROUND);
        assert_eq!(f.cells[0].fg, Some(ANSI[1]));
        assert_eq!(current(), one_dark_pro());
    }

}