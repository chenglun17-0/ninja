//! `Terminal` + `RenderState` 的薄封装：字节进（PTY），帧出（渲染器）。
//!
//! 所有方法只在主线程调用（libghostty-vt 非线程安全）。
//! vt 的 effects（on_pty_write / on_size / on_title_changed）由 view 侧注册，
//! 回调捕获 `Arc` 共享状态，因此 `Terminal<'static, 'static>` 可以搬进 ivars。

use libghostty_vt::error::Result;
use libghostty_vt::render::{CellIterator, CursorVisualStyle, Dirty, RenderState, RowIterator};
use libghostty_vt::screen::{CellWide, Screen};
use libghostty_vt::selection::gesture::Gesture;
use libghostty_vt::style::{RgbColor, Style, Underline};
use libghostty_vt::terminal::{Mode, Point, PointCoordinate, ScrollViewport};
use libghostty_vt::{key, Terminal, TerminalOptions};

/// IME marked text（预编辑串）的渲染要素。view 从光标位置填 `x`/`y`，
/// 渲染器从该 cell 起按 codepoint 宽度逐字落格、画下划线。
#[derive(Clone, Debug, Default)]
pub struct Marked {
    pub text: String,
    /// marked 内选区（char 起始位置, char 长度）。
    pub selected: (usize, usize),
    /// 落点（viewport cell 坐标）。
    pub x: u16,
    pub y: u16,
}

/// 一帧的可渲染快照（渲染器吃的全部数据，无 vt 类型泄漏）。
#[derive(Debug)]
pub struct Frame {
    pub cols: u16,
    pub rows: u16,
    /// 默认前景/背景（OSC 10/11 可被程序改写）。
    pub fg: Rgb,
    pub bg: Rgb,
    /// 视口内可见光标（viewport 坐标，cell 单位）。
    pub cursor: Option<CursorView>,
    pub cursor_style: CursorVisualStyle,
    pub cursor_blinking: bool,
    /// 本帧是否需要重画（Clean 时渲染器可跳过）。
    pub dirty: Dirty,
    pub cells: Vec<FrameCell>,
    /// IME 预编辑串（None = 无输入中）。
    pub marked: Option<Marked>,
}

#[derive(Clone, Copy, Debug)]
pub struct CursorView {
    pub x: u16,
    pub y: u16,
    pub at_wide_tail: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl From<RgbColor> for Rgb {
    fn from(c: RgbColor) -> Self {
        Rgb(c.r, c.g, c.b)
    }
}

/// 一个 cell 的渲染要素。
#[derive(Clone, Debug, Default)]
pub struct FrameCell {
    /// grapheme cluster（空 = 空白格）。
    pub text: String,
    pub wide: CellWideKind,
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub inverse: bool,
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub selected: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CellWideKind {
    #[default]
    Narrow,
    Wide,
    /// 宽字形后的占位：不渲染。
    SpacerTail,
    /// 软换行行尾的占位：不渲染。
    SpacerHead,
}

impl From<CellWide> for CellWideKind {
    fn from(w: CellWide) -> Self {
        match w {
            CellWide::Wide => CellWideKind::Wide,
            CellWide::SpacerTail => CellWideKind::SpacerTail,
            CellWide::SpacerHead => CellWideKind::SpacerHead,
            CellWide::Narrow => CellWideKind::Narrow,
        }
    }
}

/// `Terminal` + 迭代器 + 编码器 + 选区手势机的宿主。全部用 NULL
/// allocator（'static 生命周期），可以整体搬进 view ivars。
pub struct TermState {
    pub terminal: Terminal<'static, 'static>,
    render_state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    pub key_encoder: key::Encoder<'static>,
    pub gesture: Gesture<'static>,
}

impl TermState {
    pub fn new(cols: u16, rows: u16, max_scrollback: usize) -> Result<Self> {
        let terminal = Terminal::new(TerminalOptions {
            cols,
            rows,
            max_scrollback,
        })?;
        Ok(Self {
            key_encoder: key::Encoder::new()?,
            gesture: Gesture::new()?,
            render_state: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            terminal,
        })
    }

    /// PTY 字节 → 终端状态（vt_write 永不失败）。
    pub fn feed(&mut self, data: &[u8]) {
        self.terminal.vt_write(data);
    }

    /// resize + reflow（主屏 reflow，副屏不 reflow，由 vt 核处理）。
    pub fn resize(&mut self, cols: u16, rows: u16, cell_w_px: u32, cell_h_px: u32) {
        let _ = self.terminal.resize(cols, rows, cell_w_px, cell_h_px);
    }

    pub fn cols(&self) -> u16 {
        self.terminal.cols().unwrap_or(80)
    }

    pub fn rows(&self) -> u16 {
        self.terminal.rows().unwrap_or(24)
    }

    pub fn on_alternate_screen(&self) -> bool {
        matches!(self.terminal.active_screen(), Ok(Screen::Alternate))
    }

    /// 滚轮：主屏滚视口；副屏改发方向键（less/vim 才能滚）。
    /// 返回需要写 PTY 的字节（主屏路径为空）。
    pub fn scroll(&mut self, lines: isize) -> Vec<u8> {
        if lines == 0 {
            return Vec::new();
        }
        if self.on_alternate_screen() {
            // 副屏：每行发一个方向键（滚轮一格 = 3 行，与常见终端一致）。
            let (key_code, n) = if lines < 0 {
                (key::Key::ArrowUp, (-lines) as usize)
            } else {
                (key::Key::ArrowDown, lines as usize)
            };
            let mut all = Vec::new();
            for _ in 0..n.min(24) {
                let mut out = Vec::new();
                self.encode_key(key_code, key::Mods::empty(), None, &mut out);
                all.extend_from_slice(&out);
            }
            all
        } else {
            self.terminal
                .scroll_viewport(ScrollViewport::Delta(lines as isize));
            Vec::new()
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        self.terminal.scroll_viewport(ScrollViewport::Bottom);
    }

    /// 焦点事件（mode 1004 开启才编码，由编码器/调用方判定）。
    pub fn encode_focus(&mut self, gained: bool) -> Option<Vec<u8>> {
        let enabled = self
            .terminal
            .mode(Mode::FOCUS_EVENT)
            .unwrap_or(false);
        if !enabled {
            return None;
        }
        let ev = if gained {
            libghostty_vt::focus::Event::Gained
        } else {
            libghostty_vt::focus::Event::Lost
        };
        let mut buf = [0u8; 8];
        let n = ev.encode(&mut buf).ok()?;
        Some(buf[..n].to_vec())
    }

    /// 把 `key::Key` + 修饰键编码成 PTY 字节（编码器选项每次从终端刷新，
    /// DECCKM / 小键盘 / kitty 协议状态才能跟上一条输出）。
    /// `utf8`：该键的无修饰文本（无则 None，让编码器用逻辑键）。
    pub fn encode_key(
        &mut self,
        key_code: key::Key,
        mods: key::Mods,
        utf8: Option<&str>,
        out: &mut Vec<u8>,
    ) -> bool {
        self.key_encoder.set_options_from_terminal(&self.terminal);
        let mut event = match key::Event::new() {
            Ok(e) => e,
            Err(_) => return false,
        };
        event
            .set_action(key::Action::Press)
            .set_key(key_code)
            .set_mods(mods);
        if let Some(text) = utf8 {
            event.set_utf8(Some(text));
        }
        match self.key_encoder.encode_to_vec(&event, out) {
            Ok(()) => !out.is_empty(),
            Err(_) => false,
        }
    }

    /// 刷新渲染快照，产出帧。调用方渲染后无需手动清 dirty（这里统一
    /// set_dirty(Clean) + 行 dirty 清零，下一次 update 重新累计）。
    pub fn frame_into(&mut self, frame: &mut Frame) -> Result<()> {
        {
            let snapshot = self.render_state.update(&self.terminal)?;
            let colors = snapshot.colors()?;
            frame.cols = snapshot.cols()?;
            frame.rows = snapshot.rows()?;
            frame.fg = colors.foreground.into();
            frame.bg = colors.background.into();
            frame.dirty = snapshot.dirty()?;
            frame.cursor_blinking = snapshot.cursor_blinking()?;
            frame.cursor_style = snapshot.cursor_visual_style()?;
            frame.cursor = snapshot.cursor_viewport()?.map(|c| CursorView {
                x: c.x,
                y: c.y,
                at_wide_tail: c.at_wide_tail,
            });

            frame.cells.clear();
            frame.cells.reserve(usize::from(frame.cols) * usize::from(frame.rows));

            let mut row_iter = self.rows.update(&snapshot)?;
            let mut row_index: usize = 0;
            while let Some(row) = row_iter.next() {
                let mut cell_iter = self.cells.update(row)?;
                let mut x: usize = 0;
                while let Some(cell) = cell_iter.next() {
                    let style: Style = cell.style().unwrap_or_default();
                    let text = {
                        let mut s = String::new();
                        cell.graphemes_utf8(&mut s).ok();
                        s
                    };
                    frame.cells.push(FrameCell {
                        text,
                        wide: cell.raw_cell().map(|c| c.wide().unwrap_or(CellWide::Narrow).into()).unwrap_or_default(),
                        fg: cell.fg_color().ok().flatten().map(Into::into),
                        bg: cell.bg_color().ok().flatten().map(Into::into),
                        inverse: style.inverse,
                        bold: style.bold,
                        italic: style.italic,
                        faint: style.faint,
                        underline: style.underline != Underline::None,
                        strikethrough: style.strikethrough,
                        selected: cell.is_selected().unwrap_or(false),
                    });
                    x += 1;
                    let _ = x;
                }
                let _ = row.set_dirty(false);
                row_index += 1;
                let _ = row_index;
            }
            snapshot.set_dirty(Dirty::Clean)?;
        }
        Ok(())
    }

    /// 视口坐标 → grid ref（选区用）。y 在视口内，x 夹在 [0, cols)。
    pub fn grid_ref_viewport(
        &self,
        x: u16,
        y: u16,
    ) -> Result<libghostty_vt::screen::GridRef<'_>> {
        self.terminal.grid_ref(Point::Viewport(PointCoordinate {
            x,
            y: u32::from(y),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_of(term: &mut TermState) -> Frame {
        let mut f = empty_frame();
        term.frame_into(&mut f).unwrap();
        f
    }

    fn empty_frame() -> Frame {
        Frame {
            cols: 0,
            rows: 0,
            fg: Rgb(255, 255, 255),
            bg: Rgb(0, 0, 0),
            cursor: None,
            cursor_style: CursorVisualStyle::Block,
            cursor_blinking: false,
            dirty: Dirty::Clean,
            cells: Vec::new(),
            marked: None,
        }
    }

    #[test]
    fn frame_reads_text_styles_and_cursor() {
        let mut term = TermState::new(20, 5, 100).unwrap();
        term.feed(b"\x1b[1;32mhi\x1b[0m \xE4\xBD\xA0\xE5\xA5\xBD!");
        let frame = frame_of(&mut term);
        assert_eq!(frame.cols, 20);
        assert_eq!(frame.cells.len(), 100);

        let hi = &frame.cells[0];
        assert_eq!(hi.text, "h");
        assert!(hi.bold);
        // 绿色：fg 是显式设置过的（非默认前景）。
        assert!(hi.fg.is_some(), "bold green should carry explicit fg");

        // 中文进 2 格：cells[3] 宽字形，cells[4] 是 spacer tail。
        assert_eq!(frame.cells[3].text, "你");
        assert_eq!(frame.cells[3].wide, CellWideKind::Wide);
        assert_eq!(frame.cells[4].wide, CellWideKind::SpacerTail);
        assert_eq!(frame.cells[5].text, "好");
        assert_eq!(frame.cells[5].wide, CellWideKind::Wide);

        let cursor = frame.cursor.expect("cursor visible");
        assert_eq!(cursor.x, 8);
        assert_eq!(cursor.y, 0);
    }

    #[test]
    fn resize_reflows_wrapped_line() {
        let mut term = TermState::new(10, 4, 200).unwrap();
        term.feed(b"0123456789ABCDEF"); // 10 列宽下软换行
        term.feed(b"\r\n");
        let before = frame_of(&mut term);
        assert_eq!(before.cells[10].text, "A"); // 第二行开头是 A

        // 拉宽到 20 列 → reflow 回一行。
        term.resize(20, 4, 10, 20);
        let after = frame_of(&mut term);
        assert_eq!(after.cols, 20);
        assert_eq!(after.cells[10].text, "A");
        assert_eq!(after.cells[0].text, "0");
        // 原软换行行尾标志（wrap continuation）应消失：第 0 行后续直接可接字。
        assert_eq!(after.cells[16].text, ""); // "0123456789ABCDEF" 共 16 字
    }

    #[test]
    fn alternate_screen_scroll_encodes_arrows() {
        let mut term = TermState::new(20, 5, 100).unwrap();
        // 主屏路径：滚视口，无 PTY 字节。
        assert!(term.scroll(2).is_empty());
        assert!(term.scroll(-3).is_empty());

        // 副屏（less/vim 同款）：改发方向键。
        term.feed(b"\x1b[?1049h");
        assert!(term.on_alternate_screen());
        let up = term.scroll(-3);
        assert_eq!(up, b"\x1b[A\x1b[A\x1b[A");
        let down = term.scroll(1);
        assert_eq!(down, b"\x1b[B");
    }

    #[test]
    fn key_encoding_tracks_cursor_application_mode() {
        let mut term = TermState::new(20, 5, 100).unwrap();
        let mut out = Vec::new();
        assert!(term.encode_key(key::Key::ArrowLeft, key::Mods::empty(), None, &mut out));
        assert_eq!(out, b"\x1b[D");
        out.clear();

        term.feed(b"\x1b[?1h"); // DECCKM
        assert!(term.encode_key(key::Key::ArrowLeft, key::Mods::empty(), None, &mut out));
        assert_eq!(out, b"\x1bOD");
    }

    #[test]
    fn ctrl_letter_encodes_c0_byte() {
        // D-B 回归：Ctrl+字母按 vt 键编码出 C0 控制字节（宿主 keyDown 的
        // Ctrl 直通路径，绕过 interpretKeyEvents/sanitize 文本路径）。
        // 交互程序（pi、bash）依赖 ^C=SIGINT（ISIG）、^A/^E/^U 行编辑。
        let mut term = TermState::new(80, 24, 100).unwrap();
        let mut out = Vec::new();
        assert!(term.encode_key(key::Key::C, key::Mods::CTRL, Some("c"), &mut out));
        assert_eq!(out, b"\x03", "^C 必须 0x03");

        // ⇧^C 同归 0x03：小写化后的未修饰文本，shift 不参与 C0 派生
        //（大写 "C" 会被编码器改产 CSI 99;5u）。终端惯例。
        out.clear();
        assert!(term.encode_key(
            key::Key::C,
            key::Mods::CTRL | key::Mods::SHIFT,
            Some("c"),
            &mut out
        ));
        assert_eq!(out, b"\x03");

        // 常用 C0 家族：^A/^H/^U/^Z/^Space。^H=0x08 与退格 0x7f 区分。
        for (key, text, want) in [
            (key::Key::A, "a", b"\x01".as_slice()),
            (key::Key::H, "h", b"\x08".as_slice()),
            (key::Key::U, "u", b"\x15".as_slice()),
            (key::Key::Z, "z", b"\x1a".as_slice()),
            (key::Key::Space, " ", b"\x00".as_slice()),
        ] {
            out.clear();
            assert!(term.encode_key(key, key::Mods::CTRL, Some(text), &mut out));
            assert_eq!(out, want, "{text:?}+Ctrl");
        }

        // Ctrl+方向键（PUA 文本剥成 None）：CSI 修饰序列（shell 词跳）。
        out.clear();
        assert!(term.encode_key(key::Key::ArrowLeft, key::Mods::CTRL, None, &mut out));
        assert_eq!(out, b"\x1b[1;5D");

        // Ctrl+Alt+字母：ESC 前缀的 meta+C0。
        out.clear();
        assert!(term.encode_key(key::Key::C, key::Mods::CTRL | key::Mods::ALT, Some("c"), &mut out));
        assert_eq!(out, b"\x1b\x03");
    }

    #[test]
    fn focus_encode_gated_by_mode() {
        let mut term = TermState::new(20, 5, 100).unwrap();
        assert!(term.encode_focus(true).is_none());
        term.feed(b"\x1b[?1004h");
        assert_eq!(term.encode_focus(true), Some(b"\x1b[I".to_vec()));
        assert_eq!(term.encode_focus(false), Some(b"\x1b[O".to_vec()));
    }
}
