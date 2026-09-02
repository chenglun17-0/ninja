//! q1 SurfaceHostView：宿主自建 NSView，`nsview` 交 libghostty 挂 Metal 层
//!（layer-hosting），叶节点 = 嵌入 surface。
//!
//! 职责（q1 验收面）：
//! - **resize 全链**：`setFrameSize` → `surface_set_size(px)` +
//!   `set_content_scale(backingScale)` + 同步 `layer.contentsScale`（分屏
//!   relayout / 窗口缩放共用）；`viewDidMoveToWindow` 补挂窗后的首推；
//!   `viewDidChangeBackingProperties` / 窗口换屏补跨屏 scale 变化。
//!   SIZE_LIMIT/INITIAL_SIZE action 存进本视图，由 [`crate::host`] 换算成
//!   窗口 min/max/初始尺寸。
//! - **焦点链**：become/resign first responder → `surface_set_focus`；
//!   放大态隐藏 → `surface_set_occlusion(false)`（渲染线程停画，数据照喂）。
//! - **键盘**：`keyDown/keyUp/flagsChanged` → NSEvent →
//!   `ghostty_input_key_s`（见 [`crate::keymap`]）；IME 走
//!   `interpretKeyEvents` → `insertText`（提交，key 事件带 text /
//!   键序列外直接 `surface_key(keycode=0)`）与 `setMarkedText`（预编辑 →
//!   `surface_preedit` + `surface_ime_point` 定位候选窗）——对齐 macOS
//!   Ghostty 本尊（SurfaceView_AppKit.swift）与 v1 view.rs 的 IME 经验。
//! - **鼠标**：view points、原点在上（本视图 isFlipped=true，convert 后
//!   直传）；button/pos/scroll 全套。

#![allow(non_snake_case)] // ObjC selector 方法名

use std::cell::{Cell, RefCell};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSEvent, NSFocusRingType, NSResponder, NSTextInputClient, NSTrackingArea,
    NSTrackingAreaOptions, NSView,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSPoint, NSRect, NSSize, NSString,
};
use objc2_quartz_core::CATransaction;

use ghostty_sys::*;

use crate::host;
use crate::keymap;

pub struct Ivars {
    /// 嵌入 surface 句柄（null = 未建/已拆）。建/拆只经 [`crate::host`]。
    pub surface: Cell<ghostty_surface_t>,
    /// 稳定 pane id（zoom dump 取证用；与 surface 指针解耦）。
    pub pane_id: Cell<u32>,
    // IME 预编辑状态
    marked: RefCell<String>,
    marked_selected: Cell<(usize, usize)>,
    /// keyDown 的 interpretKeyEvents 累积文本（Swift keyTextAccumulator 同款）。
    key_texts: RefCell<Vec<String>>,
    in_key_event: Cell<bool>,
    /// PWD action 记录（新面 working-directory 继承由 ghostty
    /// inherited_config 自管，这里只留观察值）。
    pub pwd: RefCell<Option<String>>,
    // 窗口约束（host 的 action 分发改写；viewDidMoveToWindow 时应用）
    min_px: Cell<Option<(u32, u32)>>,
    max_px: Cell<Option<(u32, u32)>>,
    /// INITIAL_SIZE（逻辑 points；仅首面/未显示窗口应用一次）。
    pub initial_pt: Cell<Option<(u32, u32)>>,
    /// CELL_SIZE（px；窗口约束/取证用。q3 hit 的像素→
    /// cell 换算改用网格占比（见 plugins.rs point_to_cell 的取舍记录），
    /// 本字段只服务窗口定尺寸）。
    pub cell_px: Cell<Option<(u32, u32)>>,
    /// 上次 push_size 推过的视图尺寸（points；变化 = resize → q3 收层）。
    pub last_pushed_pt: Cell<Option<(f64, f64)>>,
    /// OSC 标题：Ghostty 75ms 合并，避免回车中间态把顶栏刷成 `~`。
    pub pending_title: RefCell<Option<String>>,
    /// ⌘F 搜索栏（叠在 Metal 面上）。
    pub search_bar: RefCell<Option<Retained<crate::search::SearchBarView>>>,
}

define_class!(
    // SAFETY:
    // - NSView 子类化无强约束方法；覆写的先走 super 或纯自算。
    // - 不实现 Drop；ivars 在 set_ivars 后只经 Cell/RefCell 访问。
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct SurfaceHostView;

    impl SurfaceHostView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true // 左上原点：ghostty mouse/ime 坐标同系，直传
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeKeyView))]
        fn can_become_key_view(&self) -> bool {
            true
        }

        #[unsafe(method(focusRingType))]
        fn focus_ring_type(&self) -> NSFocusRingType {
            NSFocusRingType::None
        }

        #[unsafe(method(drawFocusRingMask))]
        fn draw_focus_ring_mask(&self) {}

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            true
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            // SAFETY: 标准 super 调用。
            let ok: bool = unsafe { msg_send![super(self), becomeFirstResponder] };
            let s = self.ivars().surface.get();
            if !s.is_null() {
                unsafe { ghostty_surface_set_focus(s, true) };
            }
            ok
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            // SAFETY: 标准 super 调用。
            let ok: bool = unsafe { msg_send![super(self), resignFirstResponder] };
            let s = self.ivars().surface.get();
            if !s.is_null() {
                unsafe { ghostty_surface_set_focus(s, false) };
            }
            ok
        }

        #[unsafe(method(viewDidMoveToWindow))]
        fn view_did_move_to_window(&self) {
            self.push_size();
            self.apply_window_constraints();
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            // SAFETY: 标准 super 调用。
            let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
            self.push_size();
        }

        /// 跨屏（Retina ↔ 非 Retina / 不同缩放）：frame points 不变但
        /// backing scale 变了，setFrameSize 不会来——必须在这里重推，
        /// 否则 ghostty 的 layer.contentsScale 还是旧值，渲染尺寸按旧
        /// scale 算、合成器再缩一次 → 画面错位/只画一块。
        #[unsafe(method(viewDidChangeBackingProperties))]
        fn view_did_change_backing_properties(&self) {
            // SAFETY: 标准 super 调用。
            let _: () = unsafe { msg_send![super(self), viewDidChangeBackingProperties] };
            self.push_size();
        }

        /// windowDidChangeScreen 延后一拍的补推（见 [`Self::screen_changed`]）。
        #[unsafe(method(ninjaScreenChangedTick:))]
        fn ninja_screen_changed_tick(&self, _timer: Option<&AnyObject>) {
            self.push_size();
        }

        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            // SAFETY: 标准 super 调用。
            let _: () = unsafe { msg_send![super(self), updateTrackingAreas] };
            let opts = NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::MouseMoved
                | NSTrackingAreaOptions::ActiveAlways
                | NSTrackingAreaOptions::InVisibleRect;
            // owner 传 self（AppKit 弱引用）：NSView→NSResponder→NSObject→AnyObject。
            let owner: &AnyObject = self.as_super().as_super().as_super();
            // SAFETY: 类方法 alloc + 指定初始化器（NSTrackingArea 非
            // MainThreadOnly，无 mtm 版 alloc）；参数平凡。
            let ta: Retained<NSTrackingArea> = unsafe {
                let this: Allocated<NSTrackingArea> = msg_send![NSTrackingArea::class(), alloc];
                NSTrackingArea::initWithRect_options_owner_userInfo(
                    this,
                    NSRect::ZERO,
                    opts,
                    Option::Some(owner),
                    None,
                )
            };
            self.addTrackingArea(&ta);
        }

        // ---- 键盘 ----

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if std::env::var_os("NINJA_Q1_DEBUG").is_some() {
                eprintln!(
                    "ninja: keyDown code={} flags={:#x} chars={:?} unmod={:?}",
                    event.keyCode(),
                    event.modifierFlags().0,
                    event.characters().map(|c| c.to_string()),
                    event
                        .charactersByApplyingModifiers(objc2_app_kit::NSEventModifierFlags(0))
                        .map(|c| c.to_string())
                );
            }
            let Some(s) = self.surface_opt() else {
                // surface 已拆：只走 IME 通路，事件不进终端。
                self.interpret(event);
                return;
            };
            // q3：插件键盘路由先行——本 pane 有插件层（层前台）或已授予
            // 热键命中时，键事件转 input.key 给插件，不进终端。
            if crate::plugins::key_route(
                self,
                event.keyCode(),
                keymap::mods_from_flags(event.modifierFlags().0 as u64),
                event
                    .charactersIgnoringModifiers()
                    .map(|c| c.to_string()),
            ) {
                return;
            }
            let action = if event.isARepeat() {
                GHOSTTY_ACTION_REPEAT
            } else {
                GHOSTTY_ACTION_PRESS
            };
            // ⌘ 组合若不是 ghostty 键位绑定，macOS 本尊会重派发让 ⌘+key 进
            // 终端编码（v1 同语义：Cmd 组合直发）。ghostty 自己判绑定
            //（surface_key 内部），宿主只管把事件原样送进去。

            // interpretKeyEvents：文本键/IME 提交 → insertText 累积。
            let marked_before = !self.ivars().marked.borrow().is_empty();
            self.ivars().key_texts.borrow_mut().clear();
            self.ivars().in_key_event.set(true);
            self.interpret(event);
            self.ivars().in_key_event.set(false);
            let texts = std::mem::take(&mut *self.ivars().key_texts.borrow_mut());
            self.sync_preedit(marked_before);
            let composing = !self.ivars().marked.borrow().is_empty() || marked_before;

            if !texts.is_empty() {
                for text in texts {
                    // 组合中的裸控制字符归 IME，不进终端（Swift 同款）。
                    if composing && is_single_control(&text) {
                        continue;
                    }
                    self.key_action(s, action, event, Some(text), false);
                }
                return;
            }
            let text = event
                .characters()
                .map(|c| c.to_string())
                .and_then(|t| keymap::sanitize_text(&t));
            if composing && text.as_deref().is_some_and(is_single_control) {
                return;
            }
            self.key_action(s, action, event, text, composing);
        }

        #[unsafe(method(keyUp:))]
        fn key_up(&self, event: &NSEvent) {
            let Some(s) = self.surface_opt() else { return };
            self.key_action(s, GHOSTTY_ACTION_RELEASE, event, None, false);
        }

        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, event: &NSEvent) {
            let Some(s) = self.surface_opt() else { return };
            let Some(m) = keymap::mod_key_of_code(event.keyCode()) else {
                return;
            };
            if self.has_marked_text_impl() {
                return; // 预编辑中不动修饰
            }
            let mods = keymap::mods_from_flags(event.modifierFlags().0 as u64);
            // 按下（该位在当前 flags 里）= PRESS，松开 = RELEASE。
            let action = if mods & m != 0 {
                GHOSTTY_ACTION_PRESS
            } else {
                GHOSTTY_ACTION_RELEASE
            };
            let mut key = keymap::key_event(event, action, mods);
            key.keycode = u32::from(event.keyCode());
            unsafe { ghostty_surface_key(s, key) };
        }

        // ---- 鼠标 ----

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            if std::env::var_os("NINJA_Q1_DEBUG").is_some() {
                eprintln!(
                    "ninja: mouseDown pane={} loc={:?} win={:?}",
                    self.pane_id(),
                    self.point_of_event(event),
                    event.locationInWindow(),
                );
            }
            // 点击夺焦（macOS 本尊 SurfaceView.mouseDown 同款：纯 NSView
            // 子类不会被 AppKit 自动提为 first responder，须显式接管）。
            if let Some(w) = self.window() {
                w.makeFirstResponder(Some(as_responder(self)));
            }
            let Some(s) = self.surface_opt() else { return };
            let mods = keymap::mods_from_flags(event.modifierFlags().0 as u64);
            unsafe { ghostty_surface_mouse_button(s, GHOSTTY_MOUSE_PRESS, GHOSTTY_MOUSE_LEFT, mods) };
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let Some(s) = self.surface_opt() else { return };
            let mods = keymap::mods_from_flags(event.modifierFlags().0 as u64);
            // q3 hit：点击上下文先登记（OPEN_URL action 在下面的 release
            // 调用里同步重入时分发——链接源）。
            crate::plugins::click_begin(self, self.point_of_event(event), mods);
            unsafe { ghostty_surface_mouse_button(s, GHOSTTY_MOUSE_RELEASE, GHOSTTY_MOUSE_LEFT, mods) };
            unsafe { ghostty_surface_mouse_pressure(s, 0, 0.0) };
            // q3 hit：链接源没分发过且是 ⌘+click → 网格源路径识别。
            if let Some((_pane, row, col, proto_mods)) = crate::plugins::click_end(self) {
                crate::plugins::handle_grid_hit(self, row, col, proto_mods);
            }
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            let Some(s) = self.surface_opt() else {
                // SAFETY: 标准 super 调用。
                let _: () = unsafe { msg_send![super(self), rightMouseDown: event] };
                return;
            };
            let mods = keymap::mods_from_flags(event.modifierFlags().0 as u64);
            let consumed =
                unsafe { ghostty_surface_mouse_button(s, GHOSTTY_MOUSE_PRESS, GHOSTTY_MOUSE_RIGHT, mods) };
            if !consumed {
                // SAFETY: 标准 super 调用。
                let _: () = unsafe { msg_send![super(self), rightMouseDown: event] };
            }
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) {
            let Some(s) = self.surface_opt() else {
                // SAFETY: 标准 super 调用。
                let _: () = unsafe { msg_send![super(self), rightMouseUp: event] };
                return;
            };
            let mods = keymap::mods_from_flags(event.modifierFlags().0 as u64);
            let consumed =
                unsafe { ghostty_surface_mouse_button(s, GHOSTTY_MOUSE_RELEASE, GHOSTTY_MOUSE_RIGHT, mods) };
            if !consumed {
                // SAFETY: 标准 super 调用。
                let _: () = unsafe { msg_send![super(self), rightMouseUp: event] };
            }
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            let Some(s) = self.surface_opt() else { return };
            let mods = keymap::mods_from_flags(event.modifierFlags().0 as u64);
            let button = other_button(event.buttonNumber());
            unsafe { ghostty_surface_mouse_button(s, GHOSTTY_MOUSE_PRESS, button, mods) };
        }

        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, event: &NSEvent) {
            let Some(s) = self.surface_opt() else { return };
            let mods = keymap::mods_from_flags(event.modifierFlags().0 as u64);
            let button = other_button(event.buttonNumber());
            unsafe { ghostty_surface_mouse_button(s, GHOSTTY_MOUSE_RELEASE, button, mods) };
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, event: &NSEvent) {
            let Some(s) = self.surface_opt() else { return };
            let p = self.point_of_event(event);
            let mods = keymap::mods_from_flags(event.modifierFlags().0 as u64);
            unsafe { ghostty_surface_mouse_pos(s, p.x, p.y, mods) };
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, event: &NSEvent) {
            let Some(s) = self.surface_opt() else { return };
            // 拖拽中不报离开（macOS 本尊同款：拖拽事件继续来）。
            if NSEvent::pressedMouseButtons() != 0 {
                return;
            }
            let mods = keymap::mods_from_flags(event.modifierFlags().0 as u64);
            unsafe { ghostty_surface_mouse_pos(s, -1.0, -1.0, mods) };
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            self.mouse_moved_impl(event);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.mouse_moved_impl(event);
        }

        #[unsafe(method(rightMouseDragged:))]
        fn right_mouse_dragged(&self, event: &NSEvent) {
            self.mouse_moved_impl(event);
        }

        #[unsafe(method(otherMouseDragged:))]
        fn other_mouse_dragged(&self, event: &NSEvent) {
            self.mouse_moved_impl(event);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let Some(s) = self.surface_opt() else { return };
            let (mut x, mut y) = (event.scrollingDeltaX(), event.scrollingDeltaY());
            let precise = event.hasPreciseScrollingDeltas();
            if precise {
                // 触控板 2x 速度（macOS 本尊主观调参，同款）。
                x *= 2.0;
                y *= 2.0;
            }
            let mods = keymap::scroll_mods(precise, event.momentumPhase());
            unsafe { ghostty_surface_mouse_scroll(s, x, y, mods) };
        }

        // ---- Edit 菜单（⌘C/⌘V/⌘A 落到 first responder = 本视图）----

        #[unsafe(method(copy:))]
        fn copy_action(&self, _sender: Option<&AnyObject>) {
            self.binding_action("copy_to_clipboard");
        }

        #[unsafe(method(paste:))]
        fn paste_action(&self, _sender: Option<&AnyObject>) {
            self.binding_action("paste_from_clipboard");
        }

        #[unsafe(method(selectAll:))]
        fn select_all_action(&self, _sender: Option<&AnyObject>) {
            self.binding_action("select_all");
        }
    }

    unsafe impl NSTextInputClient for SurfaceHostView {
        // IME 提交 / 文本键。keyDown 中 → 累积（回 keyDown 后统一发
        // key 事件带 text）；keyDown 外（候选窗点选等）→ 提交文本直发。
        #[unsafe(method(insertText:replacementRange:))]
        unsafe fn insertText_replacementRange(&self, string: &AnyObject, _range: objc2_foundation::NSRange) {
            let Some(text) = text_from_object(string) else { return };
            if text.is_empty() {
                return;
            }
            let had_marked = self.has_marked_text_impl();
            self.clear_marked();
            if self.ivars().in_key_event.get() {
                self.ivars().key_texts.borrow_mut().push(text);
                return;
            }
            self.sync_preedit(true);
            let Some(s) = self.surface_opt() else { return };
            if had_marked {
                // 预编辑提交：keycode=0 的 key 事件（经键位解释，
                // macOS 本尊 committedPreeditTextAction 同款）。
                self.key_with_text_only(s, GHOSTTY_ACTION_PRESS, &text);
            } else {
                unsafe { ghostty_surface_text(s, text.as_ptr() as *const std::ffi::c_char, text.len()) };
            }
        }

        // 编辑/功能键命令：macOS 本尊为防 NSBeep 空实现——控制键语义
        // 已随 keyDown 的原事件进 surface_key，无需再映射。
        #[unsafe(method(doCommandBySelector:))]
        unsafe fn doCommandBySelector(&self, _selector: objc2::runtime::Sel) {}

        // IME 预编辑：存串；keyDown 外立即同步 ghostty preedit。
        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        unsafe fn setMarkedText_selectedRange_replacementRange(
            &self,
            string: &AnyObject,
            selected_range: objc2_foundation::NSRange,
            _replacement_range: objc2_foundation::NSRange,
        ) {
            let text = text_from_object(string).unwrap_or_default();
            self.ivars()
                .marked_selected
                .set((selected_range.location, selected_range.length));
            *self.ivars().marked.borrow_mut() = text;
            if !self.ivars().in_key_event.get() {
                self.sync_preedit(true);
            }
        }

        #[unsafe(method(unmarkText))]
        fn unmarkText(&self) {
            if self.clear_marked() {
                self.sync_preedit(true);
            }
        }

        #[unsafe(method(selectedRange))]
        fn selectedRange(&self) -> objc2_foundation::NSRange {
            let (loc, len) = self.ivars().marked_selected.get();
            objc2_foundation::NSRange::new(loc, len)
        }

        #[unsafe(method(markedRange))]
        fn markedRange(&self) -> objc2_foundation::NSRange {
            let n = self.ivars().marked.borrow().chars().count();
            if n == 0 {
                objc2_foundation::NSRange::new(objc2_foundation::NSNotFound as usize, 0)
            } else {
                objc2_foundation::NSRange::new(0, n)
            }
        }

        #[unsafe(method(hasMarkedText))]
        fn hasMarkedText(&self) -> bool {
            self.has_marked_text_impl()
        }

        // 终端无文档文本存储：给空（AppKit 特殊路径才要）。
        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        unsafe fn attributedSubstringForProposedRange_actualRange(
            &self,
            _range: objc2_foundation::NSRange,
            _actual_range: objc2_foundation::NSRangePointer,
        ) -> Option<Retained<NSAttributedString>> {
            None
        }

        #[unsafe(method_id(validAttributesForMarkedText))]
        fn validAttributesForMarkedText(&self) -> Retained<NSArray<NSAttributedStringKey>> {
            NSArray::new()
        }

        // 候选窗定位：ghostty ime_point（view points、原点在上；本视图
        // flipped，无需翻转 y）。
        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        unsafe fn firstRectForCharacterRange_actualRange(
            &self,
            _range: objc2_foundation::NSRange,
            _actual_range: objc2_foundation::NSRangePointer,
        ) -> NSRect {
            let Some(s) = self.surface_opt() else {
                return NSRect::ZERO;
            };
            let (mut x, mut y, mut w, mut h) = (0.0, 0.0, 0.0, 0.0);
            unsafe { ghostty_surface_ime_point(s, &mut x, &mut y, &mut w, &mut h) };
            let view = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h.max(1.0)));
            // SAFETY: convertPoint:toView: 标准调用（nil = window 坐标）。
            let win = self.convertPoint_toView(view.origin, None);
            let Some(window) = self.window() else {
                return view;
            };
            // SAFETY: NSWindow convertRectToScreen: 标准调用。
            unsafe { msg_send![&window, convertRectToScreen: NSRect::new(win, view.size)] }
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn characterIndexForPoint(&self, _point: NSPoint) -> usize {
            0
        }
    }
);

// ---------------------------------------------------------------------------
// Rust 接口
// ---------------------------------------------------------------------------

/// insertText:/setMarkedText: 的参数可能是 NSString 或 NSAttributedString。
fn text_from_object(obj: &AnyObject) -> Option<String> {
    // SAFETY: isKindOfClass: 任意 NSObject 可查。
    let is_attr: bool =
        unsafe { objc2::msg_send![obj, isKindOfClass: NSAttributedString::class()] };
    if is_attr {
        // SAFETY: 类型检查后的普通消息发送（string 属性）。
        let s: Retained<NSString> = unsafe { msg_send![obj, string] };
        return Some(s.to_string());
    }
    let is_str: bool = unsafe { objc2::msg_send![obj, isKindOfClass: NSString::class()] };
    if is_str {
        // SAFETY: 类型检查后的指针上转。
        let s: &NSString = unsafe { &*(std::ptr::from_ref(obj) as *const NSString) };
        return Some(s.to_string());
    }
    None
}

/// 单个 C0 控制字符（IME 组合期抑制，Swift shouldSuppressComposingControlInput）。
fn is_single_control(s: &str) -> bool {
    let mut it = s.chars();
    matches!(it.next(), Some(c) if (c as u32) < 0x20) && it.next().is_none()
}

/// 中键/侧键编号 → ghostty 按钮（NSEvent.buttonNumber）。
fn other_button(n: isize) -> ghostty_input_mouse_button_e {
    match n {
        2 => GHOSTTY_MOUSE_MIDDLE,
        3 => GHOSTTY_MOUSE_FOUR,
        4 => GHOSTTY_MOUSE_FIVE,
        // 越界键号按 0（unknown）——ghostty 枚举是 c_uint 别名。
        _ => GHOSTTY_MOUSE_UNKNOWN,
    }
}

impl SurfaceHostView {
    /// 建视图（surface 由 [`crate::host::attach_surface`] 后挂）。
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let pane_id = host::next_pane_id();
        let this = SurfaceHostView::alloc(mtm).set_ivars(Ivars {
            surface: Cell::new(std::ptr::null_mut()),
            pane_id: Cell::new(pane_id),
            marked: RefCell::new(String::new()),
            marked_selected: Cell::new((0, 0)),
            key_texts: RefCell::new(Vec::new()),
            in_key_event: Cell::new(false),
            pwd: RefCell::new(None),
            min_px: Cell::new(None),
            max_px: Cell::new(None),
            initial_pt: Cell::new(None),
            cell_px: Cell::new(None),
            last_pushed_pt: Cell::new(None),
            pending_title: RefCell::new(None),
            search_bar: RefCell::new(None),
        });
        // Ghostty AppKit SurfaceView 默认 800×600；window-width/height
        // 都 >0 时 INITIAL_SIZE 再校准。
        let frame = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(800.0, 600.0),
        );
        // SAFETY: super 的 initWithFrame:；ivars 已就位。
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.setFocusRingType(NSFocusRingType::None);
        view.setClipsToBounds(true);
        view
    }

    pub fn surface_opt(&self) -> Option<ghostty_surface_t> {
        let s = self.ivars().surface.get();
        (!s.is_null()).then_some(s)
    }

    pub fn pane_id(&self) -> u32 {
        self.ivars().pane_id.get()
    }

    /// 网格尺寸（zoom dump 取证；surface 未挂 = 0）。
    pub fn grid_size(&self) -> (u32, u32) {
        match self.surface_opt() {
            Some(s) => {
                let sz = unsafe { ghostty_surface_size(s) };
                (u32::from(sz.columns), u32::from(sz.rows))
            }
            None => (0, 0),
        }
    }

    /// 视口最下一个非空行（自底向上扫；zoom dump 的 last 字段，对齐
    /// v1 last_text_line 的取证用途——断言布局与内容）。
    pub fn last_text_line(&self) -> String {
        let Some(s) = self.surface_opt() else { return String::new() };
        let sz = unsafe { ghostty_surface_size(s) };
        if sz.rows == 0 || sz.columns == 0 {
            return String::new();
        }
        let all = crate::host::read_text(
            s,
            0,
            0,
            sz.columns as u32 - 1,
            sz.rows as u32 - 1,
        );
        for line in all.lines().rev() {
            let t = line.trim_end();
            if !t.is_empty() {
                return t.to_string();
            }
        }
        String::new()
    }

    /// resize 全链：窗口 backing scale + 像素尺寸推给 ghostty。
    /// 分屏 relayout / 窗口缩放 / 挂窗都走这里。
    pub fn push_size(&self) {
        let Some(s) = self.surface_opt() else { return };
        let b = self.bounds();
        if b.size.width <= 0.0 || b.size.height <= 0.0 {
            return;
        }
        let scale = self
            .window()
            .map(|w| w.backingScaleFactor())
            .unwrap_or(2.0)
            .max(1.0);
        if std::env::var_os("NINJA_Q1_DEBUG").is_some() {
            eprintln!(
                "ninja: push_size pane={} bounds=({:.1},{:.1}) scale={scale}",
                self.pane_id(),
                b.size.width,
                b.size.height
            );
        }
        // q3：几何变了 → 本 pane 的插件层回收（v0 层不跟随 resize 重排，
        // 收掉由插件下次 claim 重开）。
        let now = (b.size.width, b.size.height);
        if self.ivars().last_pushed_pt.get().is_some_and(|prev| prev != now) {
            crate::plugins::host_close_layers_of_pane(self.pane_id());
        }
        self.ivars().last_pushed_pt.set(Some(now));
        // libghostty 只在建 surface 时设一次 layer.contentsScale，之后由
        // 宿主同步（Swift 壳 viewDidChangeBackingProperties 同款）。它的
        // 渲染目标尺寸 = layer.bounds × contentsScale，不同步就与下面推的
        // px 对不上（帧被丢 / 合成器二次缩放）。关隐式动画避免缩放闪一下。
        if let Some(layer) = self.layer()
            && (layer.contentsScale() - scale).abs() > f64::EPSILON
        {
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            layer.setContentsScale(scale);
            CATransaction::commit();
        }
        // SAFETY: `s` 是本 view 持有的活 surface；主线程串行更新其尺寸。
        unsafe {
            ghostty_surface_set_content_scale(s, scale, scale);
            ghostty_surface_set_size(
                s,
                (b.size.width * scale).round() as u32,
                (b.size.height * scale).round() as u32,
            );
            // 主线程同步补一帧（resize 期间渲染线程外的文档化路径）。
            ghostty_surface_draw(s);
        }
    }

    /// 窗口换屏（NSWindowDidChangeScreen）：display id 给 ghostty（vsync
    /// 跟新屏刷新率）+ 重推 scale/尺寸。通知到达时 backingScaleFactor
    /// 可能还没更新（ghostty #2731），所以除了立即推，再延后一拍补推。
    pub fn screen_changed(&self) {
        if let Some(s) = self.surface_opt() {
            let display_id = self
                .window()
                .and_then(|w| w.screen())
                .map(|sc| sc.CGDirectDisplayID())
                .unwrap_or(0);
            // SAFETY: `s` 是本 view 持有的活 surface；display id 来自当前 NSScreen。
            unsafe { ghostty_surface_set_display_id(s, display_id) };
        }
        self.push_size();
        // SAFETY: ObjC 的 -self 按约定返回 retain 过的自身引用。
        let target: Retained<AnyObject> = unsafe { msg_send![self, self] };
        // SAFETY: scheduledTimer 平凡；一次性，触发即失效。
        let timer = unsafe {
            objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                0.0,
                &target,
                objc2::sel!(ninjaScreenChangedTick:),
                None,
                false,
            )
        };
        std::mem::forget(timer);
    }

    /// 隐藏/显示（放大态切换）：隐藏面停 occlusion，数据照喂不丢
    ///（v1 语义等价：surface 不销毁、网格冻结在分屏尺寸）。
    pub fn set_surface_occlusion(&self, visible: bool) {
        if let Some(s) = self.surface_opt() {
            unsafe { ghostty_surface_set_occlusion(s, visible) };
        }
    }

    /// SIZE_LIMIT action 存储（px）+ 立即应用。
    pub fn set_size_limit(&self, min: Option<(u32, u32)>, max: Option<(u32, u32)>) {
        if min.is_some() {
            self.ivars().min_px.set(min);
        }
        if max.is_some() {
            self.ivars().max_px.set(max);
        }
        self.apply_window_constraints();
    }

    /// 把 px 限制换算成窗口 contentMinSize/MaxSize（除以 backing scale）。
    fn apply_window_constraints(&self) {
        let Some(w) = self.window() else { return };
        let scale = w.backingScaleFactor();
        if let Some((mw, mh)) = self.ivars().min_px.get() {
            w.setContentMinSize(NSSize::new(mw as f64 / scale, mh as f64 / scale));
        }
        if let Some((mw, mh)) = self.ivars().max_px.get() {
            // 0 = 无上限（apprt 约定）。
            if mw > 0 && mh > 0 {
                w.setContentMaxSize(NSSize::new(mw as f64 / scale, mh as f64 / scale));
            }
        }
    }

    // ---- 内部 ----

    fn interpret(&self, event: &NSEvent) {
        let array = NSArray::from_slice(&[event]);
        // SAFETY: NSResponder interpretKeyEvents:（参数 NSArray<NSEvent>）。
        let _: () = unsafe { msg_send![self, interpretKeyEvents: &*array] };
    }

    fn mouse_moved_impl(&self, event: &NSEvent) {
        let Some(s) = self.surface_opt() else { return };
        let p = self.point_of_event(event);
        let mods = keymap::mods_from_flags(event.modifierFlags().0 as u64);
        unsafe { ghostty_surface_mouse_pos(s, p.x, p.y, mods) };
    }

    /// 窗口坐标 → 本视图坐标（flipped：左上原点，ghostty 同系直传）。
    fn point_of_event(&self, event: &NSEvent) -> NSPoint {
        self.convertPoint_fromView(event.locationInWindow(), None)
    }

    /// 预编辑同步（Swift syncPreedit 同款；clear_if_needed：曾有预编辑
    /// 且现在为空 → 清 ghostty 侧）。
    fn sync_preedit(&self, clear_if_needed: bool) {
        let Some(s) = self.surface_opt() else { return };
        let marked = self.ivars().marked.borrow();
        if !marked.is_empty() {
            // SAFETY: CString 生命周期在调用内。
            if let Ok(c) = std::ffi::CString::new(marked.as_str()) {
                unsafe {
                    ghostty_surface_preedit(
                        s,
                        c.as_ptr(),
                        c.as_bytes().len(),
                    )
                };
            }
        } else if clear_if_needed {
            unsafe { ghostty_surface_preedit(s, std::ptr::null(), 0) };
        }
    }

    fn clear_marked(&self) -> bool {
        let had = !self.ivars().marked.borrow().is_empty();
        self.ivars().marked.borrow_mut().clear();
        self.ivars().marked_selected.set((0, 0));
        had
    }

    fn has_marked_text_impl(&self) -> bool {
        !self.ivars().marked.borrow().is_empty()
    }

    /// keyDown/Up 统一出口：组 `ghostty_input_key_s`（translation mods 经
    /// ghostty 的 option-as-alt 等配置校正）+ 挂 text。
    fn key_action(
        &self,
        surface: ghostty_surface_t,
        action: ghostty_input_action_e,
        event: &NSEvent,
        text: Option<String>,
        composing: bool,
    ) -> bool {
        let raw = keymap::mods_from_flags(event.modifierFlags().0 as u64);
        // ghostty 侧翻译修饰（option-as-alt 等配置在此生效）。
        let translated = unsafe { ghostty_surface_key_translation_mods(surface, raw) };
        let mut key = keymap::key_event(event, action, translated);
        key.composing = composing;
        if let Some(t) = text
            && !t.is_empty()
        {
            // SAFETY: CString 生命周期在调用内。
            if let Ok(c) = std::ffi::CString::new(t.as_str()) {
                key.text = c.as_ptr();
                let consumed = unsafe { ghostty_surface_key(surface, key) };
                if std::env::var_os("NINJA_Q1_DEBUG").is_some() {
                    eprintln!(
                        "ninja: surface_key code={} text={:?} mods={:#x} consumed={}",
                        key.keycode, t, key.mods, consumed
                    );
                }
                return consumed;
            }
        }
        let consumed = unsafe { ghostty_surface_key(surface, key) };
        if std::env::var_os("NINJA_Q1_DEBUG").is_some() {
            eprintln!(
                "ninja: surface_key code={} text=None mods={:#x} consumed={}",
                key.keycode, key.mods, consumed
            );
        }
        consumed
    }

    /// 键序列外的纯文本提交（keycode=0；macOS 本尊 committedPreeditTextAction）。
    fn key_with_text_only(
        &self,
        surface: ghostty_surface_t,
        action: ghostty_input_action_e,
        text: &str,
    ) -> bool {
        let key = ghostty_input_key_s {
            action,
            mods: GHOSTTY_MODS_NONE,
            consumed_mods: GHOSTTY_MODS_NONE,
            keycode: 0,
            text: std::ptr::null(),
            unshifted_codepoint: 0,
            composing: false,
        };
        // SAFETY: CString 生命周期在调用内。
        if let Ok(c) = std::ffi::CString::new(text) {
            let mut key = key;
            key.text = c.as_ptr();
            return unsafe { ghostty_surface_key(surface, key) };
        }
        false
    }

    /// 按绑定动作名执行（Edit 菜单 copy/paste/selectAll 与 q2 菜单镜像项
    /// → ghostty 绑定动作；与键位同一条路径）。
    pub(crate) fn binding_action(&self, name: &str) {
        let Some(s) = self.surface_opt() else { return };
        unsafe {
            ghostty_surface_binding_action(
                s,
                name.as_ptr() as *const std::ffi::c_char,
                name.len(),
            )
        };
    }
}

/// SurfaceHostView → NSResponder 引用（makeFirstResponder 用）。
pub fn as_responder(v: &SurfaceHostView) -> &NSResponder {
    v.as_super().as_super()
}

