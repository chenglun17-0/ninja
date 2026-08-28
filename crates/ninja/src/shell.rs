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
    NSApplication, NSBackingStoreType, NSEventType, NSWindow, NSWindowOrderingMode,
    NSWindowStyleMask,
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
    let Some(container) = pane_container_of(&w) else {
        return;
    };
    container.close_leaf(view);
}

/// 焦点变化（pane become/resign first responder）→ 同步该 pane 所在窗口
/// 的焦点环。只碰 view 自己的窗口（见 handle_pane_eof 同款红线）。
pub fn sync_focus_ring_for(view: &TerminalView) {
    let Some(w) = view.window() else { return };
    let Some(container) = pane_container_of(&w) else {
        return;
    };
    container.sync_focus_ring();
}

/// 窗的 contentView 是否 PaneContainer；是则返回它（windowWillClose /
/// performClose 等收尾路径共用）。非 ninja 终端窗（不存在，防御）→ None。
fn pane_container_of(w: &NSWindow) -> Option<&PaneContainer> {
    let content = w.contentView()?;
    // SAFETY: isKindOfClass: 任意 NSObject 可查；通过后转 PaneContainer。
    let is_container: bool =
        unsafe { objc2::msg_send![&*content, isKindOfClass: PaneContainer::class()] };
    if !is_container {
        return None;
    }
    // SAFETY: 消息发送已确认类型；ObjC 子类指针可安全上转。
    Some(unsafe { &*(std::ptr::from_ref(&*content) as *const PaneContainer) })
}

/// ⌘W 的关闭决策（D-A；纯逻辑，可单测）：
/// - `surfaces`：当前 tab 的 pane（叶子）数，1 = 单 pane 窗/标签。
/// - `bare_cmd_key`：本次关窗请求是否「裸 Cmd 键」触发——⌘W（我们的
///   Close=performClose: 或系统 tab 化后的 Close Tab）的指纹。
///
/// 语义（iTerm/Ghostty 同款，p2「⌘W 关 1 窗其余 pane SIGHUP」重审后
/// 钉死）：裸 ⌘W 只关「当前面」——多 pane 窗先关焦点 pane（其余 pane
/// 各自 PTY 独立，shell 绝不陪葬），返回 false 拦掉整窗 close；单 pane
/// 放行（关当前 tab，最后一个 tab 才关窗，macOS 原生语义）。非 ⌘W
/// 路径一律放行：红绿灯、Close Window(⇧⌘W)、Close All/Close Other
/// Tabs(⌥⌘W)、菜单点击（鼠标事件）、EOF 级联与 selftest（无键事件）。
pub fn should_close_whole_window(surfaces: usize, bare_cmd_key: bool) -> bool {
    !(bare_cmd_key && surfaces > 1)
}

/// 本次关窗请求是否由「裸 Cmd 键」触发：currentEvent 是 keyDown，带
/// Cmd 且不带 Shift/Option/Ctrl。⇧⌘W/⌥⌘W（系统注入的 Close Window /
/// Close All / Close Other Tabs）带额外修饰键，红绿灯与菜单点击是鼠标
/// 事件，EOF/selftest 走定时器或无 currentEvent——都不匹配。
fn close_request_is_bare_cmd_key() -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false; // 关窗只在主线程，防御
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(ev) = app.currentEvent() else {
        return false;
    };
    if ev.r#type() != NSEventType::KeyDown {
        return false;
    }
    let mods = crate::keymap::mods_from_flags(ev.modifierFlags().0 as u64);
    mods.contains(libghostty_vt::key::Mods::SUPER)
        && !mods.intersects(
            libghostty_vt::key::Mods::SHIFT
                | libghostty_vt::key::Mods::ALT
                | libghostty_vt::key::Mods::CTRL,
        )
}

/// `windowShouldClose:` 的实现（AppDelegate 委托到这里）：裸 ⌘W + 多
/// pane 窗 → 关焦点 pane 并拦掉整窗关闭；其余放行。
/// 返回值 = 是否放行 close（true = 关）。
pub fn window_should_close(w: &NSWindow) -> bool {
    if !close_request_is_bare_cmd_key() {
        return true;
    }
    let Some(container) = pane_container_of(w) else {
        return true;
    };
    if should_close_whole_window(container.leaf_count(), true) {
        return true; // 单 pane：关 tab/窗（原生语义）
    }
    // 多 pane：只关焦点面；close_leaf 自带焦点转移 + 焦点环同步 +
    // 该 pane 自己的 PTY 收尾（SIGHUP 只发它自己的进程组）。
    match container.focused_leaf() {
        Some(f) => {
            container.close_leaf(&f);
            false
        }
        None => true, // 无焦点叶子（异常态）：放行原语义，不硬拦
    }
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
        let Some(container) = pane_container_of(&w) else {
            continue;
        };
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

#[cfg(test)]
mod tests {
    use super::should_close_whole_window;

    #[test]
    fn bare_cmd_w_closes_one_surface_at_a_time() {
        // 多 pane 窗 + 裸 ⌘W：只关焦点 pane，拦掉整窗 close。
        assert!(!should_close_whole_window(2, true));
        assert!(!should_close_whole_window(3, true));
    }

    #[test]
    fn bare_cmd_w_closes_single_pane_window() {
        // 单 pane（= 普通窗/单 pane 标签）：放行 → 关当前 tab/窗，
        // 最后一个才关窗（macOS 原生语义）。
        assert!(should_close_whole_window(1, true));
        assert!(should_close_whole_window(0, true)); // 防御：无叶子视为单面
    }

    #[test]
    fn non_cmd_w_paths_always_close_whole_window() {
        // 红绿灯 / Close Window(⇧⌘W) / Close All·Other Tabs(⌥⌘W) /
        // 菜单点击（鼠标）/ EOF 级联 / selftest：整窗/整 tab 关不受影响。
        assert!(should_close_whole_window(2, false));
        assert!(should_close_whole_window(3, false));
        assert!(should_close_whole_window(1, false));
    }
}
