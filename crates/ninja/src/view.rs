//! NSView 子类：单终端面。键盘（`interpretKeyEvents` + `key::Encoder`）、
//! IME（`NSTextInputClient`）、鼠标选区（selection gesture 状态机）、滚轮、
//! resize → cols/rows 换算 → `Terminal::resize` + PTY winsize。
//!
//! 线程模型：view 与 `TermState` 只在主线程（NSView 本就 main-thread-only）；
//! PTY 读写线程经 [`crate::pty::set_wake_hook`] 唤醒主 runloop 的
//! `CFRunLoopSource`，perform 回调里 drain → `vt_write` → 重画。
//!
//! 可变状态包在 `RefCell<State>`（objc2 0.6 ivars 无 `&mut` 访问），
//! 纪律：任何跨 `interpretKeyEvents`/AppKit 回调的调用前必须放掉 borrow。


#![allow(non_snake_case)] // ObjC selector 方法名
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use libghostty_vt::ffi::SizeReportSize;
use libghostty_vt::key::{Key, Mods};
use libghostty_vt::render::{CursorVisualStyle, Dirty};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{define_class, msg_send, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSEvent, NSPasteboard, NSPasteboardTypeString, NSResponder, NSScreen, NSView,
    NSTextInputClient,
};
use objc2_core_foundation::{
    CFRunLoop, CFRunLoopSource, CFRunLoopSourceContext, kCFRunLoopCommonModes,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSNotFound, NSRange, NSRangePointer,
    NSPoint, NSRect, NSSize, NSString, NSTimer,
};
use objc2_quartz_core::CAMetalLayer;

use crate::atlas::GlyphAtlas;
use crate::font::Font;
use crate::keymap;
use crate::pty::{self, Pty};
use crate::renderer::Renderer;
use crate::select;
use crate::term::{Frame, Marked, Rgb, TermState};

// ---------------------------------------------------------------------------
// PTY → 主线程数据泵（p1 单窗口：三个全局指针 + 一个 runloop source）
// ---------------------------------------------------------------------------

/// 主线程 view 裸指针（只在主线程写；perform 回调在主 runloop 上读）。
static MAIN_VIEW: AtomicUsize = AtomicUsize::new(0);
/// 唤醒 source 裸指针（pty 读/写线程经 wake_hook 信号它）。
static WAKE_SOURCE: AtomicUsize = AtomicUsize::new(0);
/// 主 runloop 裸指针。
static MAIN_RUNLOOP: AtomicUsize = AtomicUsize::new(0);

/// `pty::set_wake_hook` 的钩子：信号 source + 唤醒主 runloop。
/// null 检查容忍注册前/注销后的调用。
fn wake_hook() {
    let src = WAKE_SOURCE.load(Ordering::Acquire) as *const CFRunLoopSource;
    if !src.is_null() {
        unsafe { (*src).signal() };
    }
    let rl = MAIN_RUNLOOP.load(Ordering::Acquire) as *const CFRunLoop;
    if !rl.is_null() {
        unsafe { (*rl).wake_up() };
    }
}

/// runloop source 的 perform 回调（主线程）：drain PTY → 喂 vt → 重画。
unsafe extern "C-unwind" fn source_perform(_info: *mut std::ffi::c_void) {
    let ptr = MAIN_VIEW.load(Ordering::Acquire) as *const TerminalView;
    if !ptr.is_null() {
        // SAFETY: 指针由 view 在主线程写入（view 活着）且 perform 与 view
        // 同在主 runloop；shutdown 先清 MAIN_VIEW 再放 view。
        let view: &TerminalView = unsafe { &*ptr };
        view.on_pty_data();
    }
}

// ---------------------------------------------------------------------------
// ivars
// ---------------------------------------------------------------------------

/// 全部可变状态。objc2 0.6 ivars 只给 `&`，用 RefCell 提供内部可变性；
/// 所有 AppKit 重入点（interpretKeyEvents、makeFirstResponder、setTitle…）
/// 调用时不得持有 borrow。
pub struct State {
    term: TermState,
    font: Font,
    atlas: GlyphAtlas,
    renderer: Option<Renderer>,
    pty: Option<Box<Pty>>,
    /// on_size（XTWINOPS 应答）用的 cell 像素尺寸，resize 时同步。
    cell_px_shared: Arc<Mutex<(u32, u32)>>,
    /// IME 预编辑串。
    marked: Option<Marked>,
    /// 光标闪烁相位与开关（帧的 cursor_blinking 驱动 timer）。
    blink_on: bool,
    blink_active: bool,
    /// interpretKeyEvents 的事件上下文（doCommandBySelector 里取，
    /// AppKit 不会把 NSEvent 透传给命令回调）。
    cur_key: Option<Key>,
    cur_mods: Mods,
    cur_utf8: Option<String>,
    last_title: String,
    /// 复用的帧缓冲（cells vec 免每帧分配）。
    frame_buf: Frame,
}

pub struct Ivars {
    state: RefCell<State>,
    blink_timer: Cell<Option<Retained<NSTimer>>>,
}

define_class!(
    // SAFETY:
    // - NSView 子类化要求（observation/subclassing checklist）无强约束方法，
    //   覆写的方法（acceptsFirstResponder/drawRect/setFrameSize/…）均先走 super。
    // - 本类不实现 Drop；ivars 在 set_ivars 后只经 RefCell/Cell 访问。
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct TerminalView;

    impl TerminalView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            // SAFETY: 标准 super 调用，返回值类型正确。
            let ok: bool = unsafe { msg_send![super(self), becomeFirstResponder] };
            self.send_focus(true);
            ok
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            // SAFETY: 同上。
            let ok: bool = unsafe { msg_send![super(self), resignFirstResponder] };
            self.send_focus(false);
            ok
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true // 左上原点，与渲染器/CellIterator 一致
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.render_now();
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            // SAFETY: 标准 super 调用。
            let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
            self.grid_changed();
        }

        #[unsafe(method(viewDidMoveToWindow))]
        fn view_did_move_to_window(&self) {
            // SAFETY: 标准 super 调用。
            let _: () = unsafe { msg_send![super(self), viewDidMoveToWindow] };
            if let Some(w) = self.window() {
                let responder: &NSResponder = self.as_super().as_super();
                let _ = w.makeFirstResponder(Some(responder));
            }
        }

        // ---- 键盘 ----

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let flags = event.modifierFlags();
            let mods = keymap::mods_from_flags(flags.0 as u64);
            let code = event.keyCode();
            let chars = event.charactersIgnoringModifiers().map(|s| s.to_string());

            // Cmd 组合：菜单键（Cmd+C/V/A/Q/W…）已被 AppKit 菜单系统截走，
            // 到这里的都是没被接住的 → 按 SUPER 修饰编码直发 PTY。
            if mods.contains(Mods::SUPER) {
                let utf8 = chars.as_deref().and_then(keymap::sanitize_utf8);
                self.encode_and_send(keymap::key_from_code(code), mods, utf8.as_deref());
                return;
            }

            // 其余（含 IME 输入态）走 interpretKeyEvents：
            // 文本 → insertText:；编辑键 → doCommandBySelector:。
            {
                let mut st = self.state();
                st.cur_key = keymap::key_from_code(code);
                st.cur_mods = mods;
                st.cur_utf8 = chars.as_deref().and_then(keymap::sanitize_utf8);
            }
            let array = NSArray::from_slice(&[event]);
            // SAFETY: NSResponder 的 interpretKeyEvents:，参数 NSArray<NSEvent>。
            unsafe {
                let _: () = msg_send![self, interpretKeyEvents: &*array];
            }
        }

        // ---- 滚轮 ----

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let dy = event.scrollingDeltaY();
            if dy == 0.0 {
                return;
            }
            let lines = {
                let st = self.state();
                if event.hasPreciseScrollingDeltas() {
                    // 触控板/精确滚轮：像素 → 行。
                    let cell_h = st.font.metrics.cell_h.max(1.0);
                    (dy / cell_h).round() as isize
                } else {
                    // 传统滚轮：一格一行（AppKit 已折进自然滚动方向）。
                    dy as isize
                }
            };
            if lines == 0 {
                return;
            }
            let out = {
                let mut st = self.state();
                st.term.scroll(-lines)
            };
            if !out.is_empty() {
                if let Some(p) = &self.state().pty {
                    p.inner.write(out);
                }
            }
            self.render_now();
        }

        // ---- 鼠标选区（libghostty-vt selection gesture 状态机）----

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let (px, py) = self.point_of_event(event);
            let cell = self.cell_of_point(px, py);
            let mut press = match libghostty_vt::selection::gesture::PressEvent::new() {
                Ok(p) => p,
                Err(_) => return,
            };
            let _ = press.set_position(px, py);
            let _ = press.set_time(Duration::from_secs_f64(event.timestamp().max(0.0)));
            let _ = press.set_repeat_distance(6.0);
            let _ = press.set_repeat_interval(Duration::from_millis(400));

            {
                let mut st = self.state();
                let TermState {
                    gesture, terminal, ..
                } = &mut st.term;
                if let Ok(g) = terminal.grid_ref(viewport_point(cell.0, cell.1)) {
                    if let Ok(Some(sel)) = press.apply(gesture, terminal, g) {
                        let _ = terminal.set_selection(Some(&sel));
                    }
                }
            }
            self.render_now();
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let (px, py) = self.point_of_event(event);
            let cell = self.cell_of_point(px, py);
            let rectangle = event.modifierFlags().0 as u64 & 0x0008_0000 != 0; // Option
            let mut drag = match libghostty_vt::selection::gesture::DragEvent::new() {
                Ok(d) => d,
                Err(_) => return,
            };
            let _ = drag.set_position(px, py);
            let _ = drag.set_rectangle(rectangle);

            {
                let mut st = self.state();
                let (cw_px, ch_px) = *st.cell_px_shared.lock().unwrap();
                let geometry = libghostty_vt::selection::gesture::Geometry {
                    columns: u32::from(st.term.cols()),
                    cell_width: cw_px.max(1),
                    padding_left: 0,
                    screen_height: ch_px.max(1) * u32::from(st.term.rows()),
                };
                let TermState {
                    gesture, terminal, ..
                } = &mut st.term;
                if let Ok(g) = terminal.grid_ref(viewport_point(cell.0, cell.1)) {
                    if let Ok(Some(sel)) = drag.apply(gesture, terminal, g, geometry) {
                        let _ = terminal.set_selection(Some(&sel));
                    }
                }
            }
            self.render_now();
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let (px, py) = self.point_of_event(event);
            let cell = self.cell_of_point(px, py);
            let mut release = match libghostty_vt::selection::gesture::ReleaseEvent::new() {
                Ok(r) => r,
                Err(_) => return,
            };
            {
                let mut st = self.state();
                let TermState {
                    gesture, terminal, ..
                } = &mut st.term;
                let g = terminal.grid_ref(viewport_point(cell.0, cell.1)).ok();
                let _ = release.apply(gesture, terminal, g);
            }
            self.render_now();
        }

        // ---- 菜单动作（Edit 菜单 Cmd+C/V/A 沿响应链到 first responder）----

        #[unsafe(method(copy:))]
        fn copy_action(&self, _sender: Option<&AnyObject>) {
            self.copy_selection();
        }

        #[unsafe(method(paste:))]
        fn paste_action(&self, _sender: Option<&AnyObject>) {
            self.paste_clipboard();
        }

        #[unsafe(method(selectAll:))]
        fn select_all_action(&self, _sender: Option<&AnyObject>) {
            self.select_all();
        }

        // ---- 光标闪烁（runloop timer；预编辑/非闪烁时跳过）----

        #[unsafe(method(ninjaBlinkTick:))]
        fn blink_tick(&self, _timer: Option<&AnyObject>) {
            let active = {
                let st = self.state();
                st.blink_active && st.marked.is_none()
            };
            if !active {
                return;
            }
            {
                let mut st = self.state();
                st.blink_on = !st.blink_on;
            }
            self.needs_render();
        }
    }

    unsafe impl NSTextInputClient for TerminalView {
        // IME 提交 / 文本键：unmark + 原样写 PTY（文本不是键事件，无需编码）。
        #[unsafe(method(insertText:replacementRange:))]
        fn insertText_replacementRange(&self, string: &AnyObject, _range: NSRange) {
            let Some(text) = text_from_object(string) else {
                return;
            };
            if text.is_empty() {
                return;
            }
            {
                let mut st = self.state();
                st.marked = None;
                st.blink_on = true; // 输入即重置闪烁相位
                if let Some(p) = &st.pty {
                    p.inner.write(text.into_bytes());
                }
            }
            self.render_now();
        }

        // 编辑/功能键命令：映射回逻辑键再编码（DECCKM/kitty 协议状态由编码器管）。
        #[unsafe(method(doCommandBySelector:))]
        fn doCommandBySelector(&self, selector: Sel) {
            match selector.name().to_bytes() {
                b"copy:" => self.copy_selection(),
                b"paste:" => self.paste_clipboard(),
                b"selectAll:" => self.select_all(),
                b"noop:" => {}
                bytes => {
                    let name = std::str::from_utf8(bytes).unwrap_or("");
                    let (key_hint, mods, utf8) = {
                        let st = self.state();
                        (st.cur_key, st.cur_mods, st.cur_utf8.clone())
                    };
                    let key = keymap::key_from_command_selector(name).or(key_hint);
                    self.encode_and_send(key, mods, utf8.as_deref());
                }
            }
        }

        // IME 预编辑：存串 + 重画（渲染器画下划线落格）。
        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        fn setMarkedText_selectedRange_replacementRange(
            &self,
            string: &AnyObject,
            selected_range: NSRange,
            _replacement_range: NSRange,
        ) {
            let text = text_from_object(string).unwrap_or_default();
            {
                let mut st = self.state();
                if text.is_empty() {
                    st.marked = None;
                } else {
                    st.marked = Some(Marked {
                        text,
                        selected: (selected_range.location, selected_range.length),
                        x: 0,
                        y: 0,
                    });
                }
                st.blink_on = true;
            }
            self.render_now();
        }

        #[unsafe(method(unmarkText))]
        fn unmarkText(&self) {
            let had = {
                let mut st = self.state();
                st.marked.take().is_some()
            };
            if had {
                self.render_now();
            }
        }

        #[unsafe(method(selectedRange))]
        fn selectedRange(&self) -> NSRange {
            match &self.state().marked {
                Some(m) => NSRange::new(m.selected.0, m.selected.1),
                None => NSRange::new(0, 0),
            }
        }

        #[unsafe(method(markedRange))]
        fn markedRange(&self) -> NSRange {
            match &self.state().marked {
                Some(m) => NSRange::new(0, m.text.chars().count()),
                None => NSRange::new(NSNotFound as usize, 0),
            }
        }

        #[unsafe(method(hasMarkedText))]
        fn hasMarkedText(&self) -> bool {
            self.state().marked.is_some()
        }

        // 终端没有文档文本存储：给空（AppKit 只在特殊路径要它）。
        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        fn attributedSubstringForProposedRange_actualRange(
            &self,
            _range: NSRange,
            _actual_range: NSRangePointer,
        ) -> Option<Retained<NSAttributedString>> {
            None
        }

        #[unsafe(method_id(validAttributesForMarkedText))]
        fn validAttributesForMarkedText(&self) -> Retained<NSArray<NSAttributedStringKey>> {
            NSArray::new()
        }

        // 候选窗定位：光标 cell 的屏幕矩形。
        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        fn firstRectForCharacterRange_actualRange(
            &self,
            _range: NSRange,
            _actual_range: NSRangePointer,
        ) -> NSRect {
            self.cursor_screen_rect()
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn characterIndexForPoint(&self, point: NSPoint) -> usize {
            let p = self.convertPoint_fromView(point, None);
            let cell = self.cell_of_point(p.x, p.y);
            let cols = self.state().term.cols();
            usize::from(cell.1) * usize::from(cols) + usize::from(cell.0)
        }
    }
);

// ---------------------------------------------------------------------------
// 普通 Rust 接口（非 ObjC 方法）
// ---------------------------------------------------------------------------

const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;
const MAX_SCROLLBACK: usize = 10_000;
const FONT_SIZE_PT: f64 = 13.0;
const BLINK_INTERVAL_SECS: f64 = 0.53;

fn viewport_point(x: u16, y: u16) -> libghostty_vt::terminal::Point {
    libghostty_vt::terminal::Point::Viewport(libghostty_vt::terminal::PointCoordinate {
        x,
        y: u32::from(y),
    })
}

impl TerminalView {
    /// 创建 view：字体度量 → cell/atlas/renderer → vt effects 注册 → PTY。
    /// `command`：None = `$SHELL`（缺省 `/bin/bash`）。
    pub fn new(mtm: MainThreadMarker, command: Option<&str>) -> Retained<Self> {
        let scale = NSScreen::mainScreen(mtm)
            .map(|s| s.backingScaleFactor())
            .unwrap_or(2.0)
            .max(1.0);

        let mut term = TermState::new(DEFAULT_COLS, DEFAULT_ROWS, MAX_SCROLLBACK)
            .expect("Terminal init");
        let font = Font::new(FONT_SIZE_PT, scale);
        let cell_w_px = (font.metrics.cell_w * scale).ceil() as u32;
        let cell_h_px = (font.metrics.cell_h * scale).ceil() as u32;
        let baseline_px = font.baseline_offset() * scale;
        let atlas = GlyphAtlas::new(cell_h_px);

        // vt effects（回调只捕获 'static 共享句柄）：
        // - on_pty_write：DECRQM/DSR 应答直接写 PTY。
        let pty = Pty::spawn(command, DEFAULT_COLS, DEFAULT_ROWS).expect("spawn shell");
        let pty_write = pty.inner.clone();
        let _ = term
            .terminal
            .on_pty_write(move |_t, data| pty_write.write(data.to_vec()));

        // - on_size：XTWINOPS（CSI 14/16/18 t）应答 cols/rows + cell 像素。
        let cell_px_shared = Arc::new(Mutex::new((cell_w_px, cell_h_px)));
        let size_cell = cell_px_shared.clone();
        let _ = term.terminal.on_size(move |t| {
            let (w, h) = *size_cell.lock().unwrap();
            Some(SizeReportSize {
                rows: t.rows().unwrap_or(DEFAULT_ROWS),
                columns: t.cols().unwrap_or(DEFAULT_COLS),
                cell_width: w,
                cell_height: h,
            })
        });

        // - on_clipboard_write：OSC 52 → 系统剪贴板（只认 text/* 表示）。
        let _ = term.terminal.on_clipboard_write(move |_t, w| {
            for c in w.contents() {
                if c.mime == "text/plain" || c.mime.starts_with("text/") {
                    let pb = NSPasteboard::generalPasteboard();
                    pb.clearContents();
                    pb.setString_forType(
                        &NSString::from_str(c.data),
                        unsafe { NSPasteboardTypeString },
                    );
                    return Ok(());
                }
            }
            Err(libghostty_vt::terminal::ClipboardWriteError::Unsupported)
        });

        // Metal 层。
        let layer = CAMetalLayer::new();
        layer.setContentsScale(scale);
        let renderer = Renderer::new(
            layer.clone(),
            atlas.edge(),
            (f64::from(cell_w_px), f64::from(cell_h_px), baseline_px),
        )
        .expect("Metal renderer init");

        let state = State {
            term,
            font,
            atlas,
            renderer: Some(renderer),
            pty: Some(Box::new(pty)),
            cell_px_shared,
            marked: None,
            blink_on: true,
            blink_active: false,
            cur_key: None,
            cur_mods: Mods::empty(),
            cur_utf8: None,
            last_title: String::new(),
            frame_buf: empty_frame(),
        };
        let size = NSSize {
            width: state.font.metrics.cell_w * f64::from(DEFAULT_COLS),
            height: state.font.metrics.cell_h * f64::from(DEFAULT_ROWS),
        };
        let ivars = Ivars {
            state: RefCell::new(state),
            blink_timer: Cell::new(None),
        };

        // 两阶段初始化：先放 ivars，再走 NSView 的 initWithFrame:。
        let this = TerminalView::alloc(mtm).set_ivars(ivars);
        let frame = NSRect {
            origin: NSPoint::new(0.0, 0.0),
            size,
        };
        // SAFETY: super 的 initWithFrame:；ivars 已就位。
        let view: Retained<TerminalView> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.setWantsLayer(true);
        view.setLayer(Some(layer.as_super()));

        install_wake(&view);
        pty::set_wake_hook(Some(wake_hook));
        install_blink_timer(&view);
        view.grid_changed();
        view
    }

    /// RefCell 借用助手。调用点纪律：不跨 AppKit 重入调用持有。
    fn state(&self) -> std::cell::RefMut<'_, State> {
        self.ivars().state.borrow_mut()
    }

    // ---- 几何 ----

    fn point_of_event(&self, event: &NSEvent) -> (f64, f64) {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        (p.x, p.y)
    }

    /// 视图坐标（points，左上原点）→ cell（列, 行），夹在网格内。
    fn cell_of_point(&self, px: f64, py: f64) -> (u16, u16) {
        let (cols, rows, cw, ch) = {
            let st = self.state();
            (
                st.term.cols(),
                st.term.rows(),
                st.font.metrics.cell_w,
                st.font.metrics.cell_h,
            )
        };
        let x = ((px.max(0.0) / cw.max(0.001)).floor() as i64).clamp(0, i64::from(cols) - 1) as u16;
        let y = ((py.max(0.0) / ch.max(0.001)).floor() as i64).clamp(0, i64::from(rows) - 1) as u16;
        (x, y)
    }

    /// 光标 cell（viewport 坐标）→ 屏幕矩形（候选窗定位）。
    fn cursor_screen_rect(&self) -> NSRect {
        let (cw, ch, cx, cy) = {
            let st = self.state();
            let m = st.font.metrics;
            let pos = match &st.marked {
                // 预编辑中：贴着预编辑文本末端（IME 习惯）。
                Some(mk) => (
                    u32::from(mk.x) + mk.text.chars().map(count_width_cells).sum::<u32>(),
                    u32::from(mk.y),
                ),
                None => match st.frame_buf.cursor {
                    Some(c) => (u32::from(c.x), u32::from(c.y)),
                    None => (0, 0),
                },
            };
            (m.cell_w, m.cell_h, pos.0, pos.1)
        };
        let rect = NSRect {
            origin: NSPoint::new(f64::from(cx) * cw, f64::from(cy) * ch),
            size: NSSize {
                width: cw,
                height: ch,
            },
        };
        let in_window = self.convertRect_toView(rect, None);
        if let Some(w) = self.window() {
            return w.convertRectToScreen(in_window);
        }
        in_window
    }

    // ---- resize 链路：bounds → cols/rows → vt resize → PTY winsize ----

    fn grid_changed(&self) {
        let b = self.bounds();
        let (w_pt, h_pt) = (b.size.width.max(1.0), b.size.height.max(1.0));
        {
            let mut st = self.state();
            let scale = st
                .renderer
                .as_ref()
                .map(|r| r.layer.contentsScale())
                .unwrap_or(1.0)
                .max(1.0);
            let (cw_pt, ch_pt) = (st.font.metrics.cell_w, st.font.metrics.cell_h);
            let baseline_pt = st.font.baseline_offset();
            let cols = ((w_pt / cw_pt.max(0.001)).floor() as i64).clamp(10, 500) as u16;
            let rows = ((h_pt / ch_pt.max(0.001)).floor() as i64).clamp(4, 300) as u16;
            let cell_w_px = (cw_pt * scale).round() as u32;
            let cell_h_px = (ch_pt * scale).round() as u32;

            if cols != st.term.cols() || rows != st.term.rows() {
                st.term.resize(cols, rows, cell_w_px, cell_h_px);
                if let Some(p) = &st.pty {
                    p.resize(cols, rows, cell_w_px, cell_h_px);
                }
                *st.cell_px_shared.lock().unwrap() = (cell_w_px, cell_h_px);
                if let Some(r) = st.renderer.as_mut() {
                    r.cell_px = (
                        f64::from(cell_w_px),
                        f64::from(cell_h_px),
                        baseline_pt * scale,
                    );
                }
            }
            if let Some(r) = st.renderer.as_mut() {
                r.drawable_size = (w_pt * scale, h_pt * scale);
            }
        }
        self.render_now();
    }

    // ---- PTY 数据泵 ----

    fn on_pty_data(&self) {
        let (eof, new_title) = {
            let mut st = self.state();
            let Some(p) = &st.pty else { return };
            let (chunks, eof) = p.inner.drain();
            for c in chunks {
                st.term.feed(&c);
            }
            // 标题（OSC 0/2；上游 CHANGE_WINDOW_TITLE_STR 恒空不影响 title()）。
            let title = st.term.terminal.title().unwrap_or("").to_string();
            let new_title = if !title.is_empty() && title != st.last_title {
                st.last_title = title.clone();
                Some(title)
            } else {
                None
            };
            (eof, new_title)
        };
        if let Some(title) = new_title {
            if let Some(w) = self.window() {
                w.setTitle(&NSString::from_str(&title));
            }
        }
        if eof {
            // shell 退出：单窗口 p1 直接收尾（Pty::drop 发 SIGHUP + join 线程）。
            self.shutdown();
            if let Some(mtm) = MainThreadMarker::new() {
                NSApplication::sharedApplication(mtm).terminate(None);
            }
            return;
        }
        self.render_now();
    }

    // ---- 渲染 ----

    fn needs_render(&self) {
        self.setNeedsDisplay(true);
    }

    fn render_now(&self) {
        let mut st = self.state();
        let State {
            term,
            font,
            atlas,
            renderer,
            marked,
            blink_on,
            blink_active,
            frame_buf,
            ..
        } = &mut *st;
        let Some(r) = renderer.as_mut() else { return };
        if term.frame_into(frame_buf).is_err() {
            return;
        }
        *blink_active = frame_buf.cursor_blinking;
        // 光标闪烁相位（预编辑时暂停闪烁，方便对照候选窗）。
        if !*blink_on && frame_buf.cursor_blinking && marked.is_none() {
            frame_buf.cursor = None;
        }
        // IME 预编辑落点跟随光标。
        if let Some(m) = marked.as_mut() {
            match frame_buf.cursor {
                Some(c) => {
                    m.x = c.x;
                    m.y = c.y;
                }
                None => {
                    m.x = 0;
                    m.y = 0;
                }
            }
            frame_buf.marked = Some(m.clone());
        } else {
            frame_buf.marked = None;
        }
        r.draw(frame_buf, atlas, font);
    }

    // ---- 键盘/焦点/剪贴板 ----

    fn encode_and_send(&self, key: Option<Key>, mods: Mods, utf8: Option<&str>) {
        let Some(key) = key else { return };
        let mut out = Vec::new();
        {
            let mut st = self.state();
            let wrote = st.term.encode_key(key, mods, utf8, &mut out) && !out.is_empty();
            if wrote {
                if let Some(p) = &st.pty {
                    p.inner.write(out);
                }
                st.blink_on = true;
            }
        }
    }

    fn send_focus(&self, gained: bool) {
        let bytes = {
            let mut st = self.state();
            st.term.encode_focus(gained)
        };
        if let Some(bytes) = bytes {
            if let Some(p) = &self.state().pty {
                p.inner.write(bytes);
            }
        }
    }

    fn select_all(&self) {
        {
            let st = self.state();
            if let Ok(Some(sel)) = st.term.terminal.select_all() {
                let _ = st.term.terminal.set_selection(Some(&sel));
            }
        }
        self.render_now();
    }

    fn copy_selection(&self) {
        let text = {
            let st = self.state();
            select::selection_text(&st.term.terminal).unwrap_or_default()
        };
        if text.is_empty() {
            return;
        }
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        pb.setString_forType(&NSString::from_str(&text), unsafe {
            NSPasteboardTypeString
        });
    }

    fn paste_clipboard(&self) {
        let pb = NSPasteboard::generalPasteboard();
        let Some(s) = pb.stringForType(unsafe { NSPasteboardTypeString }) else {
            return;
        };
        let text = s.to_string();
        if text.is_empty() {
            return;
        }
        let bytes = {
            let st = self.state();
            select::paste_bytes(&st.term.terminal, &text)
        };
        if !bytes.is_empty() {
            if let Some(p) = &self.state().pty {
                p.inner.write(bytes);
            }
        }
    }

    // ---- 收尾 ----

    /// 清全局指针 + 停 timer + 关 PTY。幂等；EOF/退出时调用。
    pub fn shutdown(&self) {
        pty::set_wake_hook(None);
        MAIN_VIEW.store(0, Ordering::Release);
        let timer = self.ivars().blink_timer.take();
        if let Some(t) = timer {
            t.invalidate();
        }
        let mut st = self.state();
        if let Some(p) = st.pty.take() {
            p.inner.shutdown();
        }
        st.renderer.take();
    }
}

/// 预编辑文本的占格宽计数（末尾定位用）。
fn count_width_cells(c: char) -> u32 {
    u32::from(libghostty_vt::unicode::codepoint_width(c).max(1))
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

/// insertText:/setMarkedText: 的参数可能是 NSString 或 NSAttributedString。
fn text_from_object(obj: &AnyObject) -> Option<String> {
    // SAFETY: isKindOfClass: 是 NSObject 协议方法，任意对象可查；
    // 通过后指针重释为对应类型是 ObjC 对象布局保证的。
    unsafe {
        let is_str: bool = msg_send![obj, isKindOfClass: NSString::class()];
        if is_str {
            let s: &NSString = &*(std::ptr::from_ref(obj) as *const NSString);
            return Some(s.to_string());
        }
        let is_attr: bool = msg_send![obj, isKindOfClass: NSAttributedString::class()];
        if is_attr {
            let a: &NSAttributedString = &*(std::ptr::from_ref(obj) as *const NSAttributedString);
            return Some(a.string().to_string());
        }
    }
    None
}

/// PTY→主线程唤醒链安装（主线程）。source 挂主 runloop（common modes，
/// live resize 期间也泵）；runloop 会 retain source，view 不需要再持有。
fn install_wake(view: &TerminalView) {
    let mut context = CFRunLoopSourceContext {
        version: 0,
        info: std::ptr::null_mut(),
        retain: None,
        release: None,
        copyDescription: None,
        equal: None,
        hash: None,
        schedule: None,
        cancel: None,
        perform: Some(source_perform),
    };
    // SAFETY: context 布局正确；perform 回调只在主 runloop 上跑。
    let source = unsafe { CFRunLoopSource::new(None, 0, &raw mut context) }
        .expect("CFRunLoopSource create");
    let main = CFRunLoop::main().expect("main runloop");
    main.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
    WAKE_SOURCE.store(std::ptr::from_ref(&*source) as usize, Ordering::Release);
    MAIN_RUNLOOP.store(std::ptr::from_ref(&*main) as usize, Ordering::Release);
    MAIN_VIEW.store(std::ptr::from_ref(view) as usize, Ordering::Release);
    // runloop 已 retain source；这里 Retained drop 只是释放我们的引用。
    // 主 runloop 常驻，指针常有效。
    std::mem::forget(source);
}

/// 闪烁 timer：target 持 view（AppKit timer retains target；shutdown 时
/// invalidate 打破环）。
fn install_blink_timer(view: &Retained<TerminalView>) {
    // SAFETY: target 是 view（AnyObject 视图），selector 是本类方法。
    let timer = unsafe {
        let target: Retained<AnyObject> = Retained::cast_unchecked(view.clone());
        NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
            BLINK_INTERVAL_SECS,
            &target,
            Sel::register(c"ninjaBlinkTick:"),
            None,
            true,
        )
    };
    view.ivars().blink_timer.set(Some(timer));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_math() {
        // 无 GUI：验证换算公式本身（与 cell_of_point 同式）。
        let cw = 7.8;
        let ch = 16.9;
        let x = ((100.0f64.max(0.0) / cw).floor() as i64).clamp(0, i64::from(80u16) - 1) as u16;
        assert_eq!(x, 12); // 100/7.8 = 12.8 → 12
        let y = ((33.3f64.max(0.0) / ch).floor() as i64).clamp(0, i64::from(24u16) - 1) as u16;
        assert_eq!(y, 1);
        // 负坐标夹 0。
        let neg = ((-5.0f64.max(0.0) / cw).floor() as i64).clamp(0, 79) as u16;
        assert_eq!(neg, 0);
    }

    #[test]
    fn width_cells_cjk() {
        assert_eq!(count_width_cells('a'), 1);
        assert_eq!(count_width_cells('你'), 2);
        // 零宽/控制字符保守拉到 ≥1（终端网格无零宽格）。
        assert!(count_width_cells('\u{feff}') >= 1);
    }
}
