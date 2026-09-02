//! ⌘F 搜索栏：接 libghostty `start_search` / `search:` / `navigate_search`。

#![allow(non_snake_case)]

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObjectProtocol, Sel};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSButton, NSColor, NSControlTextEditingDelegate, NSEvent, NSImage, NSTextAlignment,
    NSTextField, NSTextFieldDelegate, NSView,
};
use objc2_foundation::{NSNotification, NSPoint, NSRect, NSSize, NSString};

use crate::host;
use crate::surface::SurfaceHostView;

const BAR_W: f64 = 328.0;
const BAR_H: f64 = 32.0;
const BTN: f64 = 26.0;

pub struct Ivars {
    pane_id: Cell<u32>,
    field: RefCell<Option<Retained<NSTextField>>>,
    label: RefCell<Option<Retained<NSTextField>>>,
    selected: Cell<Option<i64>>,
    total: Cell<Option<i64>>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct SearchBarView;

    impl SearchBarView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, _n: &NSNotification) {
            self.push_needle();
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let code = event.keyCode();
            let shift = event.modifierFlags().contains(objc2_app_kit::NSEventModifierFlags::Shift);
            if code == 53 {
                self.close();
                return;
            }
            if code == 36 || code == 76 {
                if shift {
                    self.navigate("navigate_search:previous");
                } else {
                    self.navigate("navigate_search:next");
                }
                return;
            }
            let _: () = unsafe { msg_send![super(self), keyDown: event] };
        }

        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn control_do_command(
            &self,
            _control: &AnyObject,
            _text_view: &AnyObject,
            command: Sel,
        ) -> Bool {
            if command == sel!(insertNewline:) {
                let prev = MainThreadMarker::new().is_some_and(|mtm| {
                    objc2_app_kit::NSApplication::sharedApplication(mtm)
                        .currentEvent()
                        .is_some_and(|e| {
                            e.modifierFlags()
                                .contains(objc2_app_kit::NSEventModifierFlags::Shift)
                        })
                });
                self.navigate(if prev {
                    "navigate_search:previous"
                } else {
                    "navigate_search:next"
                });
                return Bool::from(true);
            }
            if command == sel!(cancelOperation:) {
                self.close();
                return Bool::from(true);
            }
            Bool::from(false)
        }

        #[unsafe(method(ninjaSearchUp:))]
        fn ninja_search_up(&self, _sender: Option<&AnyObject>) {
            self.navigate("navigate_search:next");
            self.focus_field();
        }

        #[unsafe(method(ninjaSearchDown:))]
        fn ninja_search_down(&self, _sender: Option<&AnyObject>) {
            self.navigate("navigate_search:previous");
            self.focus_field();
        }

        #[unsafe(method(ninjaSearchClose:))]
        fn ninja_search_close(&self, _sender: Option<&AnyObject>) {
            self.close();
        }
    }

    unsafe impl NSObjectProtocol for SearchBarView {}
    unsafe impl NSControlTextEditingDelegate for SearchBarView {}
    unsafe impl NSTextFieldDelegate for SearchBarView {}
);

impl SearchBarView {
    fn new(mtm: MainThreadMarker, pane_id: u32, needle: &str) -> Retained<Self> {
        let this = SearchBarView::alloc(mtm).set_ivars(Ivars {
            pane_id: Cell::new(pane_id),
            field: RefCell::new(None),
            label: RefCell::new(None),
            selected: Cell::new(None),
            total: Cell::new(None),
        });
        let view: Retained<Self> = unsafe {
            msg_send![super(this), initWithFrame: NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(BAR_W, BAR_H),
            )]
        };
        view.setWantsLayer(true);
        let (r, g, b) = host::bg_rgb();
        if let Some(layer) = view.layer() {
            layer.setBackgroundColor(Some(&host::bg_color().CGColor()));
            layer.setCornerRadius(6.0);
            layer.setBorderWidth(0.0);
        }

        let field = unsafe {
            let f: Retained<NSTextField> = msg_send![NSTextField::alloc(mtm), initWithFrame: NSRect::new(
                NSPoint::new(8.0, 6.0),
                NSSize::new(168.0, 20.0),
            )];
            f
        };
        field.setBezeled(false);
        field.setBordered(false);
        field.setDrawsBackground(true);
        field.setFocusRingType(objc2_app_kit::NSFocusRingType::None);
        let lift = if 0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b) > 0.5 {
            0.92
        } else {
            1.18
        };
        let fill = NSColor::colorWithCalibratedRed_green_blue_alpha(
            (f64::from(r) / 255.0 * lift).min(1.0),
            (f64::from(g) / 255.0 * lift).min(1.0),
            (f64::from(b) / 255.0 * lift).min(1.0),
            1.0,
        );
        field.setBackgroundColor(Some(&fill));
        field.setTextColor(Some(&NSColor::labelColor()));
        field.setStringValue(&NSString::from_str(needle));
        field.setPlaceholderString(Some(&NSString::from_str("Search")));
        let _: () = unsafe { msg_send![&*field, setDelegate: &*view] };
        view.addSubview(&field);

        let label = unsafe {
            let l: Retained<NSTextField> = msg_send![NSTextField::alloc(mtm), initWithFrame: NSRect::new(
                NSPoint::new(176.0, 6.0),
                NSSize::new(44.0, 20.0),
            )];
            l
        };
        label.setEditable(false);
        label.setBezeled(false);
        label.setBordered(false);
        label.setDrawsBackground(false);
        label.setAlignment(NSTextAlignment::Right);
        label.setTextColor(Some(&NSColor::secondaryLabelColor()));
        label.setStringValue(&NSString::from_str(""));
        view.addSubview(&label);

        let by = ((BAR_H - BTN) / 2.0).round();
        view.addSubview(&icon_button(
            mtm,
            NSPoint::new(226.0, by),
            "chevron.up",
            "↑",
            "Next Match",
            sel!(ninjaSearchUp:),
            &view,
        ));
        view.addSubview(&icon_button(
            mtm,
            NSPoint::new(254.0, by),
            "chevron.down",
            "↓",
            "Previous Match",
            sel!(ninjaSearchDown:),
            &view,
        ));
        view.addSubview(&icon_button(
            mtm,
            NSPoint::new(282.0, by),
            "xmark",
            "✕",
            "Close",
            sel!(ninjaSearchClose:),
            &view,
        ));

        *view.ivars().field.borrow_mut() = Some(field);
        *view.ivars().label.borrow_mut() = Some(label);
        view
    }

    fn push_needle(&self) {
        let Some(field) = self.ivars().field.borrow().clone() else {
            return;
        };
        let needle = field.stringValue().to_string();
        let Some(v) = host::view_by_pane_id(self.ivars().pane_id.get()) else {
            return;
        };
        v.binding_action(&format!("search:{needle}"));
    }

    fn navigate(&self, action: &str) {
        let Some(v) = host::view_by_pane_id(self.ivars().pane_id.get()) else {
            return;
        };
        v.binding_action(action);
    }

    fn close(&self) {
        let Some(v) = host::view_by_pane_id(self.ivars().pane_id.get()) else {
            return;
        };
        hide(&v);
    }

    fn set_counts(&self, selected: Option<i64>, total: Option<i64>) {
        let Some(label) = self.ivars().label.borrow().clone() else {
            return;
        };
        let text = match (selected, total) {
            (Some(s), Some(t)) if s >= 0 && t > 0 => format!("{}/{}", s + 1, t),
            (_, Some(t)) => format!("-/{t}"),
            _ => String::new(),
        };
        label.setStringValue(&NSString::from_str(&text));
    }

    fn focus_field(&self) {
        if let (Some(field), Some(w)) = (self.ivars().field.borrow().clone(), self.window()) {
            w.makeFirstResponder(Some(&field));
        }
    }
}

fn icon_button(
    mtm: MainThreadMarker,
    origin: NSPoint,
    symbol: &str,
    fallback: &str,
    tooltip: &str,
    action: Sel,
    target: &SearchBarView,
) -> Retained<NSButton> {
    let btn: Retained<NSButton> = unsafe {
        msg_send![NSButton::alloc(mtm), initWithFrame: NSRect::new(origin, NSSize::new(BTN, BTN))]
    };
    btn.setBordered(false);
    btn.setFocusRingType(objc2_app_kit::NSFocusRingType::None);
    btn.setRefusesFirstResponder(true);
    btn.setTitle(&NSString::from_str(fallback));
    if let Some(img) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(symbol),
        Some(&NSString::from_str(tooltip)),
    ) {
        img.setTemplate(true);
        img.setSize(NSSize::new(11.0, 11.0));
        btn.setImage(Some(&img));
        btn.setImagePosition(objc2_app_kit::NSCellImagePosition::ImageOnly);
    }
    btn.setContentTintColor(Some(&NSColor::labelColor()));
    let _: () = unsafe { msg_send![&*btn, setToolTip: &*NSString::from_str(tooltip)] };
    let _: () = unsafe { msg_send![&*btn, setTarget: target] };
    let _: () = unsafe { msg_send![&*btn, setAction: action] };
    btn
}

pub fn show(view: &SurfaceHostView, needle: Option<&str>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    hide_bar(view, false);
    let bar = SearchBarView::new(mtm, view.pane_id(), needle.unwrap_or(""));
    let parent_w = view.bounds().size.width;
    bar.setFrame(NSRect::new(
        NSPoint::new((parent_w - BAR_W - 8.0).max(8.0), 8.0),
        NSSize::new(BAR_W, BAR_H),
    ));
    view.addSubview(&bar);
    *view.ivars().search_bar.borrow_mut() = Some(bar.clone());
    bar.focus_field();
    if needle.map(|s| !s.is_empty()).unwrap_or(false) {
        bar.push_needle();
    }
}

pub fn hide(view: &SurfaceHostView) {
    hide_bar(view, true);
}

pub fn hide_from_action(view: &SurfaceHostView) {
    hide_bar(view, false);
}

fn hide_bar(view: &SurfaceHostView, send_end: bool) {
    if let Some(bar) = view.ivars().search_bar.borrow_mut().take() {
        bar.removeFromSuperview();
    }
    if send_end {
        view.binding_action("end_search");
    }
    if let Some(w) = view.window() {
        w.makeFirstResponder(Some(crate::surface::as_responder(view)));
    }
}

pub fn set_total(view: &SurfaceHostView, total: i64) {
    if let Some(bar) = view.ivars().search_bar.borrow().as_ref() {
        bar.ivars().total.set(Some(total));
        bar.set_counts(bar.ivars().selected.get(), Some(total));
    }
}

pub fn set_selected(view: &SurfaceHostView, selected: i64) {
    if let Some(bar) = view.ivars().search_bar.borrow().as_ref() {
        bar.ivars().selected.set(Some(selected));
        bar.set_counts(Some(selected), bar.ivars().total.get());
    }
}
