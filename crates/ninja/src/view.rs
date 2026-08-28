//! NSView 子类：一个终端面（p2：一 pane = PTY + vt + Metal 视图，
//! 多 pane 各自独立）。键盘（`interpretKeyEvents` + `key::Encoder`）、
//! IME（`NSTextInputClient`）、鼠标选区（selection gesture 状态机）、滚轮、
//! resize → cols/rows 换算 → `Terminal::resize` + PTY winsize。
//!
//! 线程模型：view 与 `TermState` 只在主线程（NSView 本就 main-thread-only）；
//! 各自的 PTY 读写线程经 per-pane 的 `CFRunLoopSource` 唤醒主 runloop，
//! perform 回调里 drain → `vt_write` → 重画。生命周期：shell 退出（EOF）
//! 时 view 收尾自己，然后通知壳（`shell::handle_pane_eof`）把自己从
//! pane 树里拆掉或关窗。
//!
//! 可变状态包在 `RefCell<State>`（objc2 0.6 ivars 无 `&mut` 访问），
//! 纪律：任何跨 `interpretKeyEvents`/AppKit 回调的调用前必须放掉 borrow。


#![allow(non_snake_case)] // ObjC selector 方法名
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use libghostty_vt::ffi::SizeReportSize;
use libghostty_vt::key::{Key, Mods};
use libghostty_vt::render::{CursorVisualStyle, Dirty};
use libghostty_vt::screen::CellWide;
use ninja_protocol::{Hit as ProtocolHit, Modifier};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{define_class, msg_send, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSEvent, NSPasteboard, NSPasteboardTypeString, NSResponder, NSScreen, NSView,
    NSTextInputClient,
};
use objc2_core_foundation::{
    CFRetained, CFRunLoop, CFRunLoopSource, CFRunLoopSourceContext, kCFRunLoopCommonModes,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSNotFound, NSRange, NSRangePointer,
    NSPoint, NSRect, NSSize, NSString, NSTimer,
};
use objc2_quartz_core::CAMetalLayer;

use crate::atlas::GlyphAtlas;
use crate::config::Config;
use crate::font::Font;
use crate::keymap;
use crate::layer::{self, LayerGeom};
use crate::link;
use crate::open;
use crate::plugins;
use crate::pty::{self, Pty};
use crate::renderer::{Renderer, Theme};
use crate::select;
use crate::term::{Frame, Marked, Rgb, TermState};

// ---------------------------------------------------------------------------
// PTY → 主线程数据泵（p2：per-pane 一个 runloop source，不再全局单例）
// ---------------------------------------------------------------------------

/// 压在堆上的 per-pane 唤醒上下文：perform 回调从它取回 view。
/// `dead` 在 shutdown 首位置 true；之后 perform 直接返回（view 可能
/// 已释放）。上下文本身【故意泄漏】（每 pane ~几十字节，随 pane 数
/// 有界）：runloop 对已 signal 但未 fire 的 source 可能持有快照引用，
/// 摘除/释放后仍可能回调 perform——若 info 已 free 则 UAF（p2 实测
/// 关窗 SEGFAULT 的根因）。泄漏换取拆除窗口内任意时序的安全。
struct WakeInfo {
    view: *const TerminalView,
    dead: bool,
}

/// 拆除顺序约束：shutdown 先 drop PTY（join 读写线程 → 不再有人调
/// 唤醒闭包），再从 runloop 摘 source、free info。
struct WakeReg {
    source: CFRetained<CFRunLoopSource>,
    runloop: CFRetained<CFRunLoop>,
    info: *mut WakeInfo,
}

/// 唤醒上下文：跨线程（PTY 读线程）持有的两个裸指针。字段只在
/// [`WakeCtx::wake`] 内解引用——闭包经方法调用整体捕获，不精确穿透到
/// 裸指针字段（保持 Send/Sync 标注）。指针在拆除前常活（WakeReg 顺序）。
struct WakeCtx {
    src: *const CFRunLoopSource,
    rl: *const CFRunLoop,
}
unsafe impl Send for WakeCtx {}
unsafe impl Sync for WakeCtx {}
impl WakeCtx {
    /// SAFETY（类型不变量）：指针在 shutdown 摘除 source 前常活，
    /// 且只调线程安全的 CFRunLoopSourceSignal / CFRunLoopWakeUp。
    fn wake(&self) {
        unsafe {
            (*self.src).signal();
            (*self.rl).wake_up();
        }
    }
}

/// runloop source 的 perform 回调（主线程）：drain PTY → 喂 vt → 重画。
/// info 指向该 pane 的 WakeInfo；dead 置位后（view 将释放）直接返回。
unsafe extern "C-unwind" fn source_perform(info: *mut std::ffi::c_void) {
    if info.is_null() {
        return;
    }
    // SAFETY: info 由 install_wake 在主线程 Box::into_raw，永不 free
    //（见 WakeInfo 泄漏说明）；dead 在 view 释放前置 true。
    let wi = unsafe { &*(info.cast::<WakeInfo>()) };
    if wi.dead || wi.view.is_null() {
        return;
    }
    let view: &TerminalView = unsafe { &*wi.view };
    view.on_pty_data();
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
    /// 本 pane 的稳定 id（进程内递增；Hit.pane 用）。
    pane_id: u32,
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
    /// per-pane 唤醒注册（source + runloop + info；见 WakeReg 拆除顺序）。
    wake: Cell<Option<WakeReg>>,
    /// 上一次 setFrameSize 的尺寸（点）：AppKit 在窗口装配/居中阶段会
    /// 重复投递同尺寸事件；同尺寸 = 几何未变 = 不收层（见 set_frame_size）。
    last_size: Cell<(f64, f64)>,
    /// D-C 取证钩子状态：NINJA_FRAME_STATS 路径 + 上次落盘时刻
    ///（无 env 时零开销；Instant 非 Copy，套 RefCell）。
    stats_probe: RefCell<Option<(std::ffi::OsString, std::time::Instant)>>,
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
            // 焦点指示环（pane 壳画）跟随：只碰自己所在的窗口，
            // 不做全局遍历（关窗瞬间别的窗口可能正被拆除，全局 walk
            // 会触已释放对象——p2 实测关窗 SIGSEGV 根因）。
            crate::shell::sync_focus_ring_for(self);
            ok
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            // SAFETY: 同上。
            let ok: bool = unsafe { msg_send![super(self), resignFirstResponder] };
            self.send_focus(false);
            crate::shell::sync_focus_ring_for(self);
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
            // p5：resize 后层矩形必然错位——收层（通知插件）。p6：只在
            // 尺寸真变时收——AppKit 装配/居中阶段会重复投递同尺寸事件，
            // 无脑收会把恰在装配尾音上开的层无端拆掉（E2E 实测竞态）。
            let same = self.ivars().last_size.get() == (size.width, size.height);
            self.ivars().last_size.set((size.width, size.height));
            if same {
                return;
            }
            plugins::host_close_layers_of_pane(self.pane_id());
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
            // p5 层前台分支：本 pane 有插件层时键盘先给插件（协议语义：
            // input.key）。Esc 例外——PRODUCT「任何插件层都能立刻关掉」：
            // 宿主直接关层（摘层 + 通知插件 layer.close），不依赖插件
            // 响应速度；焦点（键盘路由）随之回终端。
            let pane = self.pane_id();
            if layer::foreground(pane).is_some() {
                let flags = event.modifierFlags();
                let mods = keymap::mods_from_flags(flags.0 as u64);
                let code = event.keyCode();
                let chars = event
                    .charactersIgnoringModifiers()
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                if keymap::key_from_code(code) == Some(Key::Escape) && !mods.contains(Mods::SUPER)
                {
                    plugins::host_close_layers_of_pane(pane);
                    self.needs_render();
                    return;
                }
                // 其余键转 input.key（键名：命名集优先，退回单字符文本）。
                let key_name = keymap::key_from_code(code)
                    .and_then(keymap::protocol_key_name)
                    .or_else(|| {
                        let c = chars.chars().next()?;
                        (c.is_ascii_graphic() && !c.is_whitespace()).then(|| {
                            c.to_ascii_lowercase().to_string()
                        })
                    });
                if let Some(k) = key_name {
                    let text = chars.chars().filter(|c| c.is_ascii_graphic()).collect::<String>();
                    plugins::forward_input_key(pane, &k, &text, modifier_list(mods));
                }
                return; // 层前台：不进 PTY/IME
            }

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

            // D-B：Ctrl 组合不能走 interpretKeyEvents——AppKit 键绑定表会把
            // Ctrl+字母翻译成编辑命令（^a→moveToBeginningOfParagraph:，
            // 终端要的是 0x01），未绑定的 ^c 在 IME 输入源下被整体吞掉
            //（interpretKeyEvents 零回调，复现取证见
            // tests/ctrl_c_interrupts.rs）；而控制字符又过不了 sanitize
            // 文本路径（C0 被剥）。唯一正确路径：按 vt 键 + CTRL 修饰编码
            // 出 C0 字节（^C→0x03，同 Ghostty 语义；⇧^C 同归 0x03，
            // Ctrl+方向键归 CSI 修饰序列）。
            if keymap::ctrl_bypasses_interpret(code, mods) {
                let utf8 = chars.as_deref().and_then(keymap::ctrl_key_utf8);
                // Ctrl 打断 IME 预编辑（同 insertText: 的清理语义）。
                {
                    let mut st = self.state();
                    if st.marked.take().is_some() {
                        st.blink_on = true;
                    }
                }
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
            // p4：Cmd+点击 = 命中分发（Ghostty 惯例）。不带 Cmd 的普通
            // 点击保持选区语义不变。
            let flags = event.modifierFlags().0 as u64;
            let mods = keymap::mods_from_flags(flags);
            if mods.contains(Mods::SUPER) {
                let (px, py) = self.point_of_event(event);
                let (col, row) = self.cell_of_point(px, py);
                self.cmd_click(col, row, mods);
                return;
            }

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
const BLINK_INTERVAL_SECS: f64 = 0.53;

fn viewport_point(x: u16, y: u16) -> libghostty_vt::terminal::Point {
    libghostty_vt::terminal::Point::Viewport(libghostty_vt::terminal::PointCoordinate {
        x,
        y: u32::from(y),
    })
}

impl TerminalView {
    /// 创建一个 pane：字体度量 → cell/atlas/renderer → vt effects 注册 →
    /// PTY → per-pane 唤醒链。`config`：shell/字体/主题色（p2 TOML）。
    pub fn new(mtm: MainThreadMarker, config: &Config) -> Retained<Self> {
        let scale = NSScreen::mainScreen(mtm)
            .map(|s| s.backingScaleFactor())
            .unwrap_or(2.0)
            .max(1.0);

        let mut term = TermState::new(DEFAULT_COLS, DEFAULT_ROWS, MAX_SCROLLBACK)
            .expect("Terminal init");
        let font = Font::with_family(
            config.font_size_pt,
            scale,
            config.font_family.as_deref(),
        );
        let cell_w_px = (font.metrics.cell_w * scale).ceil() as u32;
        let cell_h_px = (font.metrics.cell_h * scale).ceil() as u32;
        let baseline_px = font.baseline_offset() * scale;
        let atlas = GlyphAtlas::new(cell_h_px);

        // vt effects（回调只捕获 'static 共享句柄）：
        // - on_pty_write：DECRQM/DSR 应答直接写 PTY。
        let pty = Pty::spawn(config.shell.as_deref(), DEFAULT_COLS, DEFAULT_ROWS)
            .expect("spawn shell");
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
        let mut renderer = Renderer::new(
            layer.clone(),
            atlas.edge(),
            (f64::from(cell_w_px), f64::from(cell_h_px), baseline_px),
        )
        .expect("Metal renderer init");
        // 主题色从 p2 配置注入（p1 硬编码 → TOML）。
        renderer.theme = Theme {
            selection_bg: config.selection_bg,
            cursor: config.cursor,
        };

        let state = State {
            term,
            font,
            atlas,
            renderer: Some(renderer),
            pty: Some(Box::new(pty)),
            pane_id: next_pane_id(),
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
            wake: Cell::new(None),
            last_size: Cell::new((0.0, 0.0)),
            stats_probe: RefCell::new(None),
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

        // per-pane 唤醒链：source 挂主 runloop，闭包注册进 PTY 核。
        let pty_inner = view
            .state()
            .pty
            .as_ref()
            .expect("pty alive")
            .inner
            .clone();
        install_wake(&view, &pty_inner);
        // D1 修复（启动期唤醒注册竞态，p2 保留）：Pty::spawn 在上面、
        // 唤醒闭包注册在 install_wake 里，中间隔着 Renderer::new 的
        // 运行时着色器编译——快 shell 的首批 PTY 字节可能在闭包未就位时
        // 就入队 rx（读线程 wake_main 空转丢信号），字节滞留队列、
        // vt 永远收不到（空闲 shell 无后续字节补发信号）。
        // 注册完立即补一次信号：runloop 起转后 source_perform →
        // on_pty_data 会把窗口期内到达的字节全部 drain 进 vt。rx 为空
        // 时只是多一帧空画。之后到达的字节走正常路径，无丢失窗口。
        signal_wake(&view);
        install_blink_timer(&view);
        view.grid_changed();
        view
    }

    /// RefCell 借用助手。调用点纪律：不跨 AppKit 重入调用持有。
    fn state(&self) -> std::cell::RefMut<'_, State> {
        self.ivars().state.borrow_mut()
    }

    /// 本 pane 的稳定 id（Hit.pane 用；进程内递增，首个 = 1）。
    pub fn pane_id(&self) -> u32 {
        self.state().pane_id
    }

    // ---- p4 命中分发（Cmd+点击；也供 NINJA_P4_HIT 取证钩直调）----

    /// Cmd+点击路径：识别 → 构造 Hit → 插件分发 → claim 或系统默认。
    /// p5：认领时携带层几何（IOSurface 尺寸/位置由宿主定）。
    /// 主线程；分发用同步短超时（见 plugins.rs），不新增线程。
    pub fn cmd_click(&self, col: u16, row: u16, mods: Mods) {
        // 1) 行扫描（借用在块内放掉——分发/打开不再碰 view 状态）。
        //    OSC-7 pwd 是完整 URI，解码成路径（open::osc7_to_path）。
        let (cells, osc8, pwd) = {
            let st = self.state();
            let (cells, osc8) = scan_row(&st.term, row, col);
            let pwd_uri = st.term.terminal.pwd().unwrap_or("").trim().to_string();
            let pwd = open::osc7_to_path(&pwd_uri).unwrap_or_default();
            (cells, osc8, pwd)
        };
        // 2) 纯函数识别（OSC-8 优先，否则行内扩展 + 分类）。
        let Some(found) = link::recognize(&cells, usize::from(col), osc8.as_deref()) else {
            return; // 点在不可点的东西上：什么都不都不做（保持普通终端行为）
        };
        // 3) 构造 Hit（含 cwd：相对路径的解析基，p5 协议修订）并广播
        //    （未启用插件 → NoPlugins → 系统默认）。
        let pane = self.pane_id();
        let hit = ProtocolHit::new(
            plugins::next_hit_id(),
            found.kind,
            found.text.clone(),
            pwd.clone(),
            u32::from(row),
            u32::from(col),
            pane,
            modifier_list(mods),
        );
        // 4) 层几何（IOSurface/纹理/重画所需；仅插件可能认领时收集）。
        let geom = self.layer_geom(pane);
        match plugins::dispatch_hit(&hit, geom.as_ref()) {
            plugins::DispatchOutcome::Claimed { .. } => {
                // 有插件认领：层握手已在 dispatch 内完成（open→ready→
                // present），系统默认不触发。到此为止。
            }
            plugins::DispatchOutcome::NoPlugins
            | plugins::DispatchOutcome::AllIgnored => {
                // 无插件 / 全不认领：系统默认打开，绝不弹安装提示。
                let pwd = if pwd.is_empty() { None } else { Some(pwd.as_str()) };
                open::open_hit_target(found.kind, &found.text, pwd);
            }
        }
    }

    /// 层几何快照（cmd_click 用；renderer 存活才有）。None = 无渲染器
    /// （headless 取证钩）→ 分发时不开层。
    fn layer_geom(&self, pane: u32) -> Option<LayerGeom> {
        let st = self.state();
        let r = st.renderer.as_ref()?;
        let scale = r.layer.contentsScale().max(1.0);
        Some(LayerGeom {
            pane,
            cell_px: (r.cell_px.0, r.cell_px.1),
            view_px: r.drawable_size,
            scale,
            device: r.device.clone(),
            view: std::ptr::from_ref(self) as usize,
            conn: 0, // 层握手时换成认领连接 id
        })
    }

    /// 层内容重画请求（layer::present 回调；主线程）。直接画一帧而不
    /// 只标记脏（层宿主视图是 Metal 自绘：不依赖 AppKit display cycle
    /// 的时序，present 后当帧可见）。
    pub fn layer_needs_display(&self) {
        self.render_now();
    }

    /// 目标行是否还没有任何文本（取证钩子 NINJA_P4_HIT 的内容门控用：
    /// shell 首行没落定前不点击）。主线程；只读 vt 网格。
    pub fn row_is_blank(&self, row: u16) -> bool {
        let st = self.state();
        let (cells, _) = scan_row(&st.term, row, 0);
        cells
            .iter()
            .all(|c| matches!(c, crate::link::RowCell::Blank | crate::link::RowCell::Cont))
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
        let (eof, new_title, p_still_pending) = {
            let mut st = self.state();
            let Some(p) = &st.pty else { return };
            let (chunks, eof) = p.inner.drain();
            // 洪峰合帧 peek（见函数尾）：队里还有字节 = 后续 perform 会重画。
            let pending = p.inner.has_pending();
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
            (eof, new_title, pending)
        };
        if let Some(title) = new_title {
            if let Some(w) = self.window() {
                w.setTitle(&NSString::from_str(&title));
            }
        }
        if eof {
            // shell 退出（p2）：本 pane 收尾自己，然后请壳把自己从
            // pane 树拆掉；若是窗口最后一个 pane → 关窗（最后一个窗口关
            // 才退出，由 applicationShouldTerminateAfterLastWindowClosed 汇聚）。
            self.shutdown();
            crate::shell::handle_pane_eof(self);
            return;
        }
        // D-C 洪峰合帧：本次 perform 期间读线程又压入了字节 → 中间态
        // 不值得画，等下一个 perform（必会到某：push 先于 signal）
        // 带最新状态再画。稳态（零星输出）队列空，照常重画，无延迟。
        if p_still_pending {
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
        let pane = st.pane_id;
        let layers = layer::draw_list(pane);
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
        let Some(r) = renderer.as_mut() else { return; };
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
        r.draw(frame_buf, atlas, font, &layers);
        // D-C 取证钩子：NINJA_FRAME_STATS=<path> 时周期性（≥200ms）落盘
        // 画帧/跳帧计数（E2E dirty_skip 回归用；无 env 时零开销）。
        if let Some(path) = std::env::var_os("NINJA_FRAME_STATS") {
            let now = std::time::Instant::now();
            let mut probe = self.ivars().stats_probe.borrow_mut();
            if let Some(p) = probe.as_mut() {
                if now.duration_since(p.1) >= std::time::Duration::from_millis(200) {
                    p.1 = now;
                    let _ = std::fs::write(
                        &p.0,
                        format!(
                            "{{\"drawn\":{},\"skipped\":{},\"dirty\":{:?}\"}}\n",
                            r.frames_drawn, r.frames_skipped, frame_buf.dirty
                        ),
                    );
                }
            } else {
                *probe = Some((path.clone(), now));
                let _ = std::fs::write(
                    &path,
                    format!(
                        "{{\"drawn\":{},\"skipped\":{},\"dirty\":{:?}\"}}\n",
                        r.frames_drawn, r.frames_skipped, frame_buf.dirty
                    ),
                );
            }
        }
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

    /// 本 pane 收尾（幂等；EOF / 关窗 / 关 pane 时调用）：
    /// 0. info 标 dead（此后 perform 回调直接返回——view 即将释放，
    ///    而已 signal 的 source 可能在摘除后仍被 runloop 快照触发）；
    /// 1. 停闪烁 timer；2. drop PTY（SIGHUP + join 读写线程——此后不再
    ///    有人调本 pane 的唤醒闭包）；3. 摘 runloop source（info 与
    ///    source 本体泄漏不 free，见 WakeInfo）；4. 放渲染器。
    pub fn shutdown(&self) {
        // p5：pane 收尾先收层（还有插件连接可通知；指针纪律：层注册表
        // 里的 view 指针自此不再被解引用）。
        plugins::host_close_layers_of_pane(self.pane_id());
        let reg = self.ivars().wake.take();
        // 先标 dead，再做任何释放。首行执行保证任何后续 perform 都安全。
        if let Some(reg) = &reg {
            // SAFETY: info 指针来自 install_wake 的 Box::into_raw，主线程。
            unsafe { (*reg.info).dead = true };
        }
        let timer = self.ivars().blink_timer.take();
        if let Some(t) = timer {
            t.invalidate();
        }
        {
            let mut st = self.state();
            // 先断开唤醒钩子（vt 的 on_pty_write 闭包还握着 PtyInner 的
            // Arc，靠这里保证它引用不到已拆除的 source）。
            if let Some(p) = &st.pty {
                p.inner.set_wake(None);
            }
            // Pty::drop：inner.shutdown（SIGHUP + 关 master）+ join 线程。
            drop(st.pty.take());
            st.renderer.take();
        }
        if let Some(reg) = reg {
            // 摘除 source（之后的 signal 不再触发 fire）；WakeReg 的
            // Retained drop 释放我们那份引用。info 故意泄漏（防 runloop
            // 快照 perform 的 UAF）——每 pane 常量级，见 WakeInfo 文档。
            // SAFETY: 指针由 install_wake 注册，同在主线程；PTY 线程已
            // join，无并发唤醒。
            unsafe {
                reg.runloop
                    .remove_source(Some(&reg.source), kCFRunLoopCommonModes);
            }
            let _info = reg.info; // 有意泄漏（防 runloop 快照 perform 的 UAF）
            drop(reg.source);
        }
    }
}

/// 预编辑文本的占格宽计数（末尾定位用）。
fn count_width_cells(c: char) -> u32 {
    u32::from(libghostty_vt::unicode::codepoint_width(c).max(1))
}

// ---------------------------------------------------------------------------
// p4 命中分发：Cmd+点击 → 识别 → 广播插件 → claim/系统默认
// ---------------------------------------------------------------------------

/// pane id 发号器（进程内递增，首个 pane = 1）。
fn next_pane_id() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// vt `Mods` → 协议 `Modifier` 列表（顺序固定：shift/ctrl/alt/cmd）。
fn modifier_list(mods: Mods) -> Vec<Modifier> {
    let mut v = Vec::new();
    if mods.contains(Mods::SHIFT) {
        v.push(Modifier::Shift);
    }
    if mods.contains(Mods::CTRL) {
        v.push(Modifier::Ctrl);
    }
    if mods.contains(Mods::ALT) {
        v.push(Modifier::Alt);
    }
    if mods.contains(Mods::SUPER) {
        v.push(Modifier::Cmd);
    }
    v
}

/// 从 vt 网格取点击行：逐列 grapheme 文本 + 点击 cell 的 OSC-8 URI。
/// 只在点击路径调用（grid_ref 不适合渲染循环，见 vt 文档）。
fn scan_row(
    term: &TermState,
    row: u16,
    click_col: u16,
) -> (Vec<link::RowCell>, Option<String>) {
    let cols = term.cols();
    let mut cells = Vec::with_capacity(usize::from(cols));
    let mut cbuf = ['\0'; 8];
    let mut ubuf = [0u8; 2048];
    let mut osc8: Option<String> = None;
    for x in 0..cols {
        let Ok(g) = term.grid_ref_viewport(x, row) else {
            cells.push(link::RowCell::Blank);
            continue;
        };
        let text = match g.graphemes(&mut cbuf) {
            Ok(0) => String::new(),
            Ok(n) => cbuf[..n].iter().collect::<String>(),
            Err(_) => String::new(),
        };
        // 宽字形尾巴 / 软换行占位：无文本但不是 token 边界。
        let wide = g
            .cell()
            .and_then(|c| c.wide())
            .unwrap_or(CellWide::Narrow);
        if x == click_col {
            // OSC-8：点击 cell 的 hyperlink URI（0 = 无）。
            match g.hyperlink_uri(&mut ubuf) {
                Ok(n) if n > 0 => {
                    osc8 = Some(String::from_utf8_lossy(&ubuf[..n]).into_owned())
                }
                _ => {}
            }
        }
        cells.push(match wide {
            CellWide::SpacerTail | CellWide::SpacerHead => link::RowCell::Cont,
            _ if text.is_empty() || text.chars().all(char::is_whitespace) => link::RowCell::Blank,
            _ => link::RowCell::Text(text),
        });
    }
    (cells, osc8)
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

/// PTY→主线程唤醒链安装（主线程，per-pane）：source 挂主 runloop
/// （common modes，live resize 期间也泵）；唤醒闭包注册进该 pane 的
/// PTY 核（读/写线程跨线程调用）。runloop 会 retain source，WakeReg
/// 另持一份所有权供拆除（摘除 + 释放 info）。
fn install_wake(view: &TerminalView, pty: &std::sync::Arc<pty::PtyInner>) {
    let info = Box::into_raw(Box::new(WakeInfo {
        view: view as *const TerminalView,
        dead: false,
    }));
    let mut context = CFRunLoopSourceContext {
        version: 0,
        info: info.cast(),
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
    unsafe { main.add_source(Some(&source), kCFRunLoopCommonModes) };

    // 唤醒闭包：PTY 读线程执行——只调线程安全的 signal/wake_up。
    // 指针在 shutdown 先 join PTY 线程再摘 source，无 UAF 窗口。
    let ctx = WakeCtx {
        src: std::ptr::from_ref(&*source),
        rl: std::ptr::from_ref(&*main),
    };
    pty.set_wake(Some(std::sync::Arc::new(move || ctx.wake())));

    view.ivars().wake.set(Some(WakeReg {
        source,
        runloop: main,
        info,
    }));
}

/// 主线程直接补一次信号（D1：启动期注册后补发 drain，见 new）。
fn signal_wake(view: &TerminalView) {
    let reg = view.ivars().wake.take();
    if let Some(reg) = reg {
        // SAFETY: source 常活（WakeReg 持引用，主线程）。
        reg.source.signal();
        view.ivars().wake.set(Some(reg));
    }
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
