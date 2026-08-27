//! AppKit 引导：NSApplication（Regular）+ 菜单（Quit/Edit）+ 单窗口 +
//! delegate（启动即建窗，关窗即退出）。空载不建任何插件相关的东西。


#![allow(non_snake_case)] // ObjC selector 方法名
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSEventModifierFlags, NSMenu, NSMenuItem, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2::runtime::NSObjectProtocol;
use objc2_foundation::{NSObject, NSNotification, NSString};

use crate::view::TerminalView;

use std::cell::RefCell;

struct Ivars {
    window: RefCell<Option<Retained<NSWindow>>>,
    view: RefCell<Option<Retained<TerminalView>>>,
}

define_class!(
    // SAFETY:
    // - NSObject 子类化无要求；不实现 Drop。
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct AppDelegate;

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn applicationDidFinishLaunching(&self, _notification: &NSNotification) {
            let mtm = MainThreadMarker::new().expect("delegate on main thread");
            let app = NSApplication::sharedApplication(mtm);

            // 单窗口：content 大小 = view 初始 80x24 cells。
            let view = TerminalView::new(mtm, None);
            let style = NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Miniaturizable
                | NSWindowStyleMask::Resizable;
            let content = view.bounds();
            // SAFETY: NSWindow 指定初始化器；参数平凡。
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    content,
                    style,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            window.setTitle(&NSString::from_str("ninja"));
            window.setContentView(Some(&view));
            window.center();
            window.makeKeyAndOrderFront(None);

            // 窗口关闭时收尾（PTY/全局指针）。weak 槽位，self 常驻保活。
            window.setDelegate(Some(&ProtocolObject::from_ref(self)));

            self.ivars().window.replace(Some(window));
            self.ivars().view.replace(Some(view));

            // 拉起就激活（无 user gesture 的冷启动也拉前台）。deprecated 但
            // 行为稳定（macOS 14 上 activate() 有无手势拉前台失败的坑）。
            #[allow(deprecated)]
            {
                app.activateIgnoringOtherApps(true);
            }
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn applicationShouldTerminateAfterLastWindowClosed(
            &self,
            _sender: &NSApplication,
        ) -> bool {
            true
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn applicationWillTerminate(&self, _notification: &NSNotification) {
            if let Some(v) = self.ivars().view.take() {
                v.shutdown();
            }
        }

    }

    unsafe impl NSWindowDelegate for AppDelegate {
        // 关窗前先收尾，防止 runloop source 在窗口拆一半时进 view。
        #[unsafe(method(windowWillClose:))]
        fn windowWillClose(&self, _notification: &NSNotification) {
            if let Some(v) = self.ivars().view.take() {
                v.shutdown();
            }
        }
    }

    unsafe impl NSObjectProtocol for AppDelegate {}
);

/// 建 Edit 菜单（Cmd+C/V/A 走响应链到终端 view；Quit 收尾）。
fn build_menu(mtm: MainThreadMarker, app: &NSApplication) {
    let main_menu = NSMenu::new(mtm);

    // App 菜单：Quit ninja ⌘Q。
    let app_menu = NSMenu::new(mtm);
    let quit = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Quit ninja"),
            Some(objc2::sel!(terminate:)),
            &NSString::from_str("q"),
        )
    };
    quit.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
    app_menu.addItem(&quit);
    let app_item = NSMenuItem::new(mtm);
    app_item.setSubmenu(Some(&app_menu));
    main_menu.addItem(&app_item);

    // Edit 菜单：Copy ⌘C / Paste ⌘V / Select All ⌘A（动作走 first responder）。
    let edit_menu = NSMenu::new(mtm);
    edit_menu.setTitle(&NSString::from_str("Edit"));
    let copy = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Copy"),
            Some(objc2::sel!(copy:)),
            &NSString::from_str("c"),
        )
    };
    copy.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
    edit_menu.addItem(&copy);
    let paste = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Paste"),
            Some(objc2::sel!(paste:)),
            &NSString::from_str("v"),
        )
    };
    paste.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
    edit_menu.addItem(&paste);
    let select_all = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Select All"),
            Some(objc2::sel!(selectAll:)),
            &NSString::from_str("a"),
        )
    };
    select_all.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
    edit_menu.addItem(&select_all);
    let edit_item = NSMenuItem::new(mtm);
    edit_item.setTitle(&NSString::from_str("Edit"));
    edit_item.setSubmenu(Some(&edit_menu));
    main_menu.addItem(&edit_item);

    app.setMainMenu(Some(&main_menu));
}

/// 进程入口：起 AppKit、上菜单、挂 delegate、跑 runloop。
/// 单窗口退出（关窗/⌘Q/shell 退出三路都汇聚 terminate）。
pub fn run() {
    let mtm = MainThreadMarker::new().expect("ninja must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    build_menu(mtm, &app);

    // 两阶段初始化（同 view）：先放 ivars 再走 NSObject 的 init。
    let this = AppDelegate::alloc(mtm).set_ivars(Ivars {
        window: RefCell::new(None),
        view: RefCell::new(None),
    });
    // SAFETY: super 的 init；ivars 已就位。
    let delegate: Retained<AppDelegate> = unsafe { msg_send![super(this), init] };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    app.run();

    // delegate 被 AppKit 的 delegate 槽 weak 引用；进程生命期内保持存活。
    std::mem::forget(delegate);
}
