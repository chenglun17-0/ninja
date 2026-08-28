//! p2 壳：多窗口 + 原生标签（NSWindow tabbing）+ pane EOF 汇聚。
//!
//! 窗口对象不建注册表——NSApp 的 `windows` 可枚举，delegate
//! （AppDelegate）只挂通知。规则：
//! - ⌘N 新窗口；⌘T `newWindowForTab:` 新标签（系统标签栏 + 菜单同走
//!   这里），`addTabbedWindow:ordered:` 挂进当前窗口的 tab 组。
//! - pane shell 退出（EOF）→ [`handle_pane_eof`]：多 pane 窗口拆掉该
//!   pane，单 pane 窗口关窗；最后一个窗口关闭才退出（由
//!   `applicationShouldTerminateAfterLastWindowClosed` 汇聚，p2 改为 true
//!   但多窗口下最后窗关闭才触发）。

use objc2::rc::Retained;
use objc2::{ClassType, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSWindow, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_foundation::NSString;

use crate::app::AppDelegate;
use crate::config::Config;
use crate::pane::PaneContainer;
use crate::view::TerminalView;

/// 所有终端窗口共用的 tabbing identifier（相同才能自动成组）。
const TABBING_ID: &str = "ninja-terminal";

/// 建一个窗口：内容 = PaneContainer（含首个 pane）。
/// delegate 挂 AppDelegate（windowWillClose 统一收尾 pane）。
pub fn make_window(
    mtm: MainThreadMarker,
    config: &Config,
    delegate: &AppDelegate,
) -> Retained<NSWindow> {
    let container = PaneContainer::new(mtm, config);
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let content = container.bounds();
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
    window.setTabbingIdentifier(&NSString::from_str(TABBING_ID));
    window.setContentView(Some(&container));
    // 所有权模型（p2）：窗口默认 releasedWhenClosed=YES——close 时窗口
    // 自释放，再加上壳的 registry 强引用就是过释放（实测关窗 SIGSEGV，
    // pc=0xc8 跳已释放对象）。改为 NO：registry 是唯一 owner，close 完成
    // 后由 ninjaPruneClosedWindows 释放。
    // SAFETY: 布尔 setter，无别名风险。
    unsafe { window.setReleasedWhenClosed(false) };
    // windowWillClose 先收尾 pane（PTY/runloop source/timer），
    // 防止拆一半时回调进 view。
    window.setDelegate(Some(&objc2::runtime::ProtocolObject::from_ref(delegate)));
    window
}

/// pane 的 shell 退出：把它从所属窗口的 pane 树拆掉；最后剩一个 pane
/// 的窗口直接 performClose（最后窗关闭 → 退出由 AppKit 汇聚）。
/// 只碰 `view` 自己的窗口（不做全局遍历：关窗瞬间别的窗口可能在拆）。
pub fn handle_pane_eof(view: &TerminalView) {
    let Some(w) = view.window() else {
        return; // 已脱窗（关窗中）：pane 已收尾，无事可做。
    };
    let Some(content) = w.contentView() else { return };
    // SAFETY: isKindOfClass: 任意 NSObject 可查；通过后转 PaneContainer。
    let is_container: bool =
        unsafe { objc2::msg_send![&*content, isKindOfClass: PaneContainer::class()] };
    if !is_container {
        return;
    }
    // SAFETY: 消息发送已确认类型；ObjC 子类指针可安全上转。
    let container: &PaneContainer =
        unsafe { &*(std::ptr::from_ref(&*content) as *const PaneContainer) };
    container.close_leaf(view);
}

/// 焦点变化（pane become/resign first responder）→ 同步该 pane 所在窗口
/// 的焦点环。只碰 view 自己的窗口（见 handle_pane_eof 同款红线）。
pub fn sync_focus_ring_for(view: &TerminalView) {
    let Some(w) = view.window() else { return };
    let Some(content) = w.contentView() else { return };
    // SAFETY: isKindOfClass: 任意 NSObject 可查；通过后转 PaneContainer。
    let is_container: bool =
        unsafe { objc2::msg_send![&*content, isKindOfClass: PaneContainer::class()] };
    if !is_container {
        return;
    }
    // SAFETY: 消息发送已确认类型；ObjC 子类指针可安全上转。
    let container: &PaneContainer =
        unsafe { &*(std::ptr::from_ref(&*content) as *const PaneContainer) };
    container.sync_focus_ring();
}

/// ⌘N：新窗口（独立窗口，不成 tab）。
pub fn new_window(mtm: MainThreadMarker, config: &Config, delegate: &AppDelegate) {
    let w = make_window(mtm, config, delegate);
    w.makeKeyAndOrderFront(None);
    delegate.register_window(w);
}

/// ⌘T / 系统标签栏 +：新标签挂进当前 key window 的 tab 组。
pub fn new_tab(mtm: MainThreadMarker, config: &Config, delegate: &AppDelegate) {
    let app = NSApplication::sharedApplication(mtm);
    let w = make_window(mtm, config, delegate);
    let host = app.keyWindow().or_else(|| app.mainWindow());
    match host {
        Some(host) => {
            host.addTabbedWindow_ordered(&w, NSWindowOrderingMode::Above);
            w.makeKeyAndOrderFront(None);
        }
        None => {
            w.makeKeyAndOrderFront(None);
        }
    }
    delegate.register_window(w);
}

/// ⌘Q 退出前收尾：遍历所有窗口的 pane 容器（窗口可能没走
/// windowWillClose——terminate 不逐窗发 close）。
pub fn shutdown_all_windows(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    for w in app.windows().iter() {
        let Some(content) = w.contentView() else { continue };
        // SAFETY: isKindOfClass: 任意 NSObject 可查；通过后转 PaneContainer。
        let is_container: bool = unsafe {
            objc2::msg_send![&*content, isKindOfClass: PaneContainer::class()]
        };
        if !is_container {
            continue;
        }
        let container: &PaneContainer = unsafe { &*(std::ptr::from_ref(&*content) as *const PaneContainer) };
        container.shutdown_all();
    }
}

/// windowWillClose：单窗收尾（找 contentView 的 pane 容器）。
pub fn window_closed(content: &objc2_app_kit::NSView) {
    // SAFETY: isKindOfClass: 任意 NSObject 可查；通过后转 PaneContainer。
    let is_container: bool =
        unsafe { objc2::msg_send![content, isKindOfClass: PaneContainer::class()] };
    if !is_container {
        return;
    }
    // SAFETY: 消息发送已确认类型；ObjC 子类指针可安全上转。
    let container: &PaneContainer =
        unsafe { &*(std::ptr::from_ref(content) as *const PaneContainer) };
    container.shutdown_all();
}
