//! 双击原生 tab 就地改名（Ghostty TabTitleEditor 同路：NSTabButton 上叠 NSTextField）。
//! 找不到 tab 按钮时才退回对话框。

#![allow(non_snake_case)]

use std::cell::RefCell;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::runtime::Bool;
use objc2::{define_class, msg_send, sel, ClassType, MainThreadMarker, MainThreadOnly, Message as _};
use objc2_app_kit::{
    NSButton, NSControl, NSEvent, NSEventMask, NSFocusRingType, NSResponder, NSTextField, NSView,
    NSWindow,
};
use objc2_foundation::{NSNotification, NSPoint, NSRect, NSSize, NSString};

use crate::pane::container_of;
use crate::shell;

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

struct Session {
    field: Retained<TabTitleField>,
    target: Retained<NSWindow>,
    labels: Vec<(Retained<NSView>, bool)>,
    button_title: Option<(Retained<NSButton>, String)>,
}

pub fn install() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let block = block2::RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        let e = unsafe { event.as_ref() };
        if handle_double_click(mtm, e) {
            std::ptr::null_mut()
        } else {
            event.as_ptr()
        }
    });
    let mon = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::LeftMouseDown, &block)
    };
    std::mem::forget(block);
    std::mem::forget(mon);
}

/// 菜单 / Ghostty `prompt_title:tab`：优先就地编辑，失败才让调用方弹窗。
pub fn begin_inline(w: &NSWindow) -> bool {
    finish(true);
    let Some(button) = tab_button_for_window(w) else {
        return false;
    };
    begin_on_button(w, &button)
}

fn handle_double_click(mtm: MainThreadMarker, event: &NSEvent) -> bool {
    if event.clickCount() != 2 {
        return false;
    }
    let Some(src) = event.window(mtm) else {
        return false;
    };
    if src.tabbingIdentifier().to_string() != "ninja-terminal" {
        return false;
    }
    let screen = src.convertPointToScreen(event.locationInWindow());
    let Some((idx, button)) = tab_button_at_screen(&src, screen) else {
        return false;
    };
    let Some(target) = tab_window_at(&src, idx) else {
        return false;
    };
    finish(true);
    begin_on_button(&target, &button);
    true
}

fn begin_on_button(target: &NSWindow, button: &NSView) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let current = current_title(target);
    let mut labels = Vec::new();
    collect_text_fields(button, &mut labels);
    for (label, _) in &labels {
        label.setHidden(true);
    }
    let button_title = button.downcast_ref::<NSButton>().map(|btn| {
        let t = btn.title().to_string();
        btn.setTitle(&NSString::from_str(""));
        (btn.retain(), t)
    });

    let b = button.bounds();
    let inset = 6.0;
    let field = TabTitleField::new(
        mtm,
        NSRect::new(
            NSPoint::new(inset, 1.0),
            NSSize::new((b.size.width - inset * 2.0).max(24.0), (b.size.height - 2.0).max(14.0)),
        ),
        &current,
    );
    let nsfield: &NSTextField = (*field).as_super();
    button.addSubview(nsfield);
    let control: &NSControl = nsfield.as_super();
    let view: &NSView = control.as_super();
    let responder: &NSResponder = view.as_super();
    let _ = target.makeFirstResponder(Some(responder));
    unsafe { field.selectText(None) };

    SESSION.replace(Some(Session {
        field,
        target: target.retain(),
        labels,
        button_title,
    }));
    true
}

fn finish(commit: bool) {
    let Some(sess) = SESSION.take() else {
        return;
    };
    let title = sess.field.stringValue().to_string();
    let _: () = unsafe { msg_send![&*sess.field, setDelegate: std::ptr::null::<AnyObject>()] };
    sess.field.removeFromSuperview();
    for (label, hidden) in sess.labels {
        label.setHidden(hidden);
    }
    if let Some((btn, t)) = sess.button_title {
        btn.setTitle(&NSString::from_str(&t));
    }
    if commit {
        apply_title(&sess.target, title);
    }
    restore_focus(&sess.target);
}

fn apply_title(w: &NSWindow, title: String) {
    let t = title.trim().to_string();
    if let Some(c) = container_of(w) {
        if t.is_empty() {
            c.set_title_override(None);
        } else {
            c.set_title_override(Some(t));
        }
        return;
    }
    if !t.is_empty() {
        w.setTitle(&NSString::from_str(&t));
        shell::suppress_titlebar_sampling(w);
    }
}

fn current_title(w: &NSWindow) -> String {
    if let Some(c) = container_of(w) {
        c.title_override()
            .unwrap_or_else(|| w.title().to_string())
    } else {
        w.title().to_string()
    }
}

fn restore_focus(w: &NSWindow) {
    if let Some(c) = container_of(w)
        && let Some(leaf) = c.leaves().first()
    {
        let _ = w.makeFirstResponder(Some(crate::surface::as_responder(leaf)));
        return;
    }
    if let Some(cv) = w.contentView() {
        let _ = w.makeFirstResponder(Some(&cv));
    }
}

fn tab_bar(window: &NSWindow) -> Option<Retained<NSView>> {
    let root = unsafe { window.contentView()?.superview()? };
    first_descendant(&root, "NSTabBar")
}

fn tab_buttons_visual(window: &NSWindow) -> Vec<Retained<NSView>> {
    let Some(bar) = tab_bar(window) else {
        return Vec::new();
    };
    let mut buttons = Vec::new();
    collect_named(&bar, "NSTabButton", &mut buttons);
    buttons.sort_by(|a, b| {
        a.frame()
            .origin
            .x
            .partial_cmp(&b.frame().origin.x)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    buttons
}

fn tab_button_for_window(w: &NSWindow) -> Option<Retained<NSView>> {
    let host = tab_bar_host(w)?;
    let buttons = tab_buttons_visual(&host);
    let windows = tab_windows(&host);
    let idx = windows.iter().position(|x| &**x == w)?;
    buttons.get(idx).cloned()
}

fn tab_button_at_screen(host: &NSWindow, screen: NSPoint) -> Option<(usize, Retained<NSView>)> {
    let bar_host = tab_bar_host(host)?;
    let buttons = tab_buttons_visual(&bar_host);
    for (i, btn) in buttons.into_iter().enumerate() {
        if point_in_view(&btn, screen) {
            return Some((i, btn));
        }
    }
    None
}

fn tab_window_at(host: &NSWindow, idx: usize) -> Option<Retained<NSWindow>> {
    tab_windows(host).into_iter().nth(idx)
}

fn tab_windows(host: &NSWindow) -> Vec<Retained<NSWindow>> {
    match host.tabbedWindows() {
        Some(arr) => arr.iter().map(|w| w.retain()).collect(),
        None => vec![host.retain()],
    }
}

fn tab_bar_host(w: &NSWindow) -> Option<Retained<NSWindow>> {
    if tab_bar(w).is_some() {
        return Some(w.retain());
    }
    if let Some(arr) = w.tabbedWindows() {
        for tw in arr.iter() {
            if tab_bar(&tw).is_some() {
                return Some(tw.retain());
            }
        }
    }
    None
}

fn point_in_view(view: &NSView, screen: NSPoint) -> bool {
    let Some(win) = view.window() else {
        return false;
    };
    let in_win = win.convertPointFromScreen(screen);
    let p = view.convertPoint_fromView(in_win, None);
    let b = view.bounds();
    p.x >= b.origin.x
        && p.y >= b.origin.y
        && p.x <= b.origin.x + b.size.width
        && p.y <= b.origin.y + b.size.height
}

fn class_name(v: &NSView) -> String {
    let s: Retained<NSString> = unsafe { msg_send![v, className] };
    s.to_string()
}

fn first_descendant(root: &NSView, name: &str) -> Option<Retained<NSView>> {
    if class_name(root) == name {
        return Some(root.retain());
    }
    for sub in root.subviews().iter() {
        if let Some(hit) = first_descendant(&sub, name) {
            return Some(hit);
        }
    }
    None
}

fn collect_named(root: &NSView, name: &str, out: &mut Vec<Retained<NSView>>) {
    if class_name(root) == name {
        out.push(root.retain());
    }
    for sub in root.subviews().iter() {
        collect_named(&sub, name, out);
    }
}

fn collect_text_fields(root: &NSView, out: &mut Vec<(Retained<NSView>, bool)>) {
    if class_name(root) == "NSTextField" {
        out.push((root.retain(), root.isHidden()));
    }
    for sub in root.subviews().iter() {
        collect_text_fields(&sub, out);
    }
}

struct TabTitleIvars;

define_class!(
    #[unsafe(super(NSTextField))]
    #[thread_kind = MainThreadOnly]
    #[ivars = TabTitleIvars]
    struct TabTitleField;

    impl TabTitleField {
        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn control_do_command(
            &self,
            _control: &AnyObject,
            _text_view: &AnyObject,
            command: Sel,
        ) -> Bool {
            if command == sel!(insertNewline:) {
                finish(true);
                return Bool::from(true);
            }
            if command == sel!(cancelOperation:) {
                finish(false);
                return Bool::from(true);
            }
            Bool::from(false)
        }

        #[unsafe(method(controlTextDidEndEditing:))]
        fn control_text_did_end_editing(&self, _n: &NSNotification) {
            finish(true);
        }
    }

    unsafe impl NSObjectProtocol for TabTitleField {}
);

impl TabTitleField {
    fn new(mtm: MainThreadMarker, frame: NSRect, title: &str) -> Retained<Self> {
        let this = TabTitleField::alloc(mtm).set_ivars(TabTitleIvars);
        let field: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        field.setBezeled(false);
        field.setBordered(false);
        field.setDrawsBackground(false);
        field.setFocusRingType(NSFocusRingType::None);
        field.setStringValue(&NSString::from_str(title));
        field.setAlignment(objc2_app_kit::NSTextAlignment::Center);
        let _: () = unsafe { msg_send![&*field, setDelegate: &*field] };
        field
    }
}
