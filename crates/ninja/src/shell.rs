//! q1 壳：多窗口 + 原生标签（NSWindow tabbing）+ surface 关闭汇聚。
//! 移植自 v1 crates/ninja/src/shell.rs（p2/D-A/X2 资产），叶子和关闭协议
//! 换成嵌入 surface。
//!
//! - ⌘N 新窗口；⌘T `newWindowForTab:` 新标签（系统标签栏 + 菜单同走），
//!   `addTabbedWindow:ordered:` 挂进当前窗口的 tab 组；
//! - ghostty `close_surface`（⌘W 默认绑定）→ `close_surface_cb` →
//!   [`handle_surface_close`]（≡ v1 handle_pane_eof）：多 pane 拆焦点叶
//!   并 surface_free、单 pane performClose 关 tab/窗；
//! - `windowShouldClose` 的裸⌘W 决策（菜单 Close=performClose 路径，
//!   多 pane 只关焦点面、单 pane 放行原生语义）；
//! - `windowWillClose` → 全叶 surface_free（延迟，见 [`crate::host`]）。

use objc2::rc::Retained;
use objc2::{msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSEventType, NSScreen, NSWindow, NSWindowOrderingMode,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSSize, NSString};

use crate::host;
use crate::pane::container_of;
use crate::surface::SurfaceHostView;

/// 所有终端窗口共用的 tabbing identifier（相同才能自动成组）。
const TABBING_ID: &str = "ninja-terminal";

/// 建一个窗口：内容 = PaneContainer（含首个叶子）。`parent` 给出时首叶
/// surface 按 `context`（WINDOW/TAB）走 inherited_config（继承字号/工作
/// 目录）；delegate 挂 AppDelegate（windowWillClose 统一收尾；窗口注册表
/// 见 app.rs）。
pub fn make_window(
    mtm: MainThreadMarker,
    parent: Option<&SurfaceHostView>,
    context: ghostty_sys::ghostty_surface_context_e,
) -> Retained<NSWindow> {
    let container = crate::pane::PaneContainer::new(mtm);
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
    apply_chrome(&window);
    // delegate 挂 AppDelegate（windowShouldClose 的裸⌘W 决策、
    // windowWillClose 的全叶收尾、key 窗焦点同步——v1 同款）。
    crate::app::wire_window(&window);
    // 所有权模型（v1 p2 教训）：窗口默认 releasedWhenClosed=YES——close 时
    // 窗口自释放，加上壳的 registry 强引用就是过释放（实测关窗 SIGSEGV）。
    // 改为 NO：registry 是唯一 owner，close 完成后由 ninjaPruneClosedWindows
    // 延迟释放。
    // SAFETY: 布尔 setter，无别名风险。
    unsafe { window.setReleasedWhenClosed(false) };

    // E2E 虚拟屏落位在**建面之前**：建面首推 push_size 读窗口所在屏的
    // backingScaleFactor——先落位保证首推就用目标屏的 scale（先建面再
    // 移屏会留下 2x→1x 的记账错位：渲染挤压 + 底部暗带，q3 hit 的行
    // 读取/像素换算全漂移，实测踩过；app.rs 的居中只对非 E2E 路径）。
    let on_e2e_screen = std::env::var_os("NINJA_E2E_SCREEN").is_some();
    if on_e2e_screen {
        place_on_e2e_screen(&window);
    }

    // 首叶 surface（建面统一走 inherited_config 传 context）。
    let first = container.first_leaf();
    let parent = parent.filter(|p| p.surface_opt().is_some());
    host::attach_surface(&first, context, parent);
    // INITIAL_SIZE action（surface_new 期间已到）：未显示窗口按它定内容
    // 尺寸。默认配置 window-width/height=0（无 INITIAL_SIZE）——退回
    // CELL_SIZE × 80x24 的默认窗（v1 同款语义；scale 反算 points）。
    if !window.isVisible() {
        let size = first
            .ivars()
            .initial_pt
            .get()
            .or_else(|| default_initial_pt(&window, &first));
        if let Some((w, h)) = size {
            window.setContentSize(objc2_foundation::NSSize::new(w as f64, h as f64));
        }
    }
    place_on_e2e_screen(&window);
    window
}

/// NINJA_E2E_SCREEN=<displayID>（PLAN「E2E 虚拟屏幕」增补，q0 平移）：
/// 窗口落到指定虚拟屏（按 deviceDescription NSScreenNumber 匹配）居中，
/// 尺寸夹到 visibleFrame 内；未设置/未匹配 → 系统默认不动。这是取证
/// 钩子，不是产品配置。
fn place_on_e2e_screen(window: &NSWindow) {
    let Ok(id) = std::env::var("NINJA_E2E_SCREEN") else { return };
    let Ok(target) = id.trim().parse::<u32>() else {
        println!("screen: NINJA_E2E_SCREEN={id:?} 非法，回退系统默认");
        return;
    };
    let Some(mtm) = MainThreadMarker::new() else { return };
    let key = NSString::from_str("NSScreenNumber");
    let matched = NSScreen::screens(mtm).iter().find(|s| {
        let desc = s.deviceDescription();
        // SAFETY: NSDictionary objectForKey: 取 NSNumber（字典仅读）。
        let v: Option<Retained<objc2_foundation::NSObject>> =
            unsafe { msg_send![&*desc, objectForKey: &*key] };
        v.map(|v| {
            // SAFETY: NSNumber integerValue 平凡。
            let num: isize = unsafe { msg_send![&*v, integerValue] };
            num as u32 == target
        })
        .unwrap_or(false)
    });
    let Some(s) = matched else {
        println!("screen: NINJA_E2E_SCREEN={target} 未匹配，回退系统默认");
        return;
    };
    let vf = s.visibleFrame();
    let cur = window.frame().size;
    let w = cur.width.min(vf.size.width - 24.0).max(320.0);
    let h = cur.height.min(vf.size.height - 24.0).max(240.0);
    window.setContentSize(NSSize::new(w, h));
    window.setFrameOrigin(NSPoint::new(
        vf.origin.x + (vf.size.width - w) / 2.0,
        vf.origin.y + (vf.size.height - h) / 2.0,
    ));
    // 落屏后重推 surface 几何：建面时窗口未上屏（backingScale 取主屏），
    // 跨屏移动若 scale 变化必须重推 content_scale/size——否则 surface
    // 以旧 scale 记账（px 网格与视图 points 错位：q3 hit 的行读取与
    // 像素换算全部漂移，实测虚拟屏 1x vs 主屏 2x 踩过）。
    if let Some(container) = crate::pane::container_of(window) {
        for leaf in container.leaves() {
            leaf.push_size();
        }
    }
    println!("screen: NINJA_E2E_SCREEN={target}（虚拟屏取证）");
}

/// CELL_SIZE(px) × (80, 24) → 内容 points（INITIAL_SIZE 缺省时）。
fn default_initial_pt(
    window: &objc2_app_kit::NSWindow,
    first: &crate::surface::SurfaceHostView,
) -> Option<(u32, u32)> {
    let (cw, ch) = first.ivars().cell_px.get()?;
    let scale = window.backingScaleFactor().max(1.0);
    Some((
        (f64::from(cw) * 80.0 / scale).ceil() as u32,
        (f64::from(ch) * 24.0 / scale).ceil() as u32,
    ))
}

/// 窗口 chrome 跟终端底色统一（v1 X2 经验）：标题栏透明 + 背景同色，
/// 免白色标题栏割裂。主题系统在 q2，这里只钉 q1 的静态同色。
pub fn apply_chrome(window: &NSWindow) {
    window.setTitlebarAppearsTransparent(true);
    window.setTitlebarSeparatorStyle(objc2_app_kit::NSTitlebarSeparatorStyle::None);
    window.setBackgroundColor(Some(&host::bg_color()));
    window.invalidateShadow();
}

/// ghostty close_surface（⌘W 默认绑定）/ EOF（close_surface_cb）的宿主侧
/// 决策 ≡ v1 handle_pane_eof：多 pane 窗拆该面（surface 延迟 free），
/// 单 pane 窗 performClose 关 tab/窗。
pub fn handle_surface_close(view: &SurfaceHostView, _process_alive: bool) {
    let Some(w) = view.window() else {
        // 已脱窗（关窗中）：surface 由 windowWillClose 统一拆。
        host::close_leaf_deferred(view);
        return;
    };
    let Some(container) = container_of(&w) else {
        return;
    };
    container.close_leaf(view);
}

/// ⌘W 的关闭决策（v1 D-A；纯逻辑，可单测）：
/// - `surfaces`：当前 tab 的 pane（叶子）数，1 = 单 pane 窗/标签；
/// - `bare_cmd_key`：本次关窗请求是否「裸 Cmd 键」触发。
///
/// 语义（iTerm/Ghostty 同款）：裸 ⌘W 只关「当前面」——多 pane 窗先关
/// 焦点 pane，返回 false 拦掉整窗 close；单 pane 放行（关当前 tab，最后
/// 一个 tab 才关窗，macOS 原生语义）。非 ⌘W 路径一律放行：红绿灯、
/// Close Window(⇧⌘W)、Close All/Close Other Tabs(⌥⌘W)、菜单点击、EOF
/// 级联与 selftest。
pub fn should_close_whole_window(surfaces: usize, bare_cmd_key: bool) -> bool {
    !(bare_cmd_key && surfaces > 1)
}

/// 本次关窗请求是否由「裸 Cmd 键」触发（v1 原样：currentEvent 是
/// keyDown 且只带 Cmd）。
fn close_request_is_bare_cmd_key() -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(ev) = app.currentEvent() else {
        return false;
    };
    if ev.r#type() != NSEventType::KeyDown {
        if std::env::var_os("NINJA_Q1_DEBUG").is_some() {
            eprintln!(
                "ninja: close-request currentEvent type={:?} (not keyDown)",
                ev.r#type()
            );
        }
        return false;
    }
    let bare = crate::keymap::is_bare_super(ev.modifierFlags().0 as u64);
    if std::env::var_os("NINJA_Q1_DEBUG").is_some() {
        eprintln!(
            "ninja: close-request keyDown mods={:#x} bare_cmd={bare}",
            ev.modifierFlags().0
        );
    }
    bare
}

/// `windowShouldClose:` 实现：裸 ⌘W + 多 pane → 关焦点面并拦掉整窗关闭；
/// 其余放行。
pub fn window_should_close(w: &NSWindow) -> bool {
    if !close_request_is_bare_cmd_key() {
        return true;
    }
    let Some(container) = container_of(w) else {
        return true;
    };
    if should_close_whole_window(container.leaf_count(), true) {
        return true; // 单 pane：关 tab/窗（原生语义）
    }
    // 多 pane：只关焦点面（close_leaf 自带焦点转移 + surface 延迟 free）。
    match container.focused_leaf() {
        Some(f) => {
            container.close_leaf(&f);
            false
        }
        None => true, // 无焦点叶子（异常态）：放行原语义，不硬拦
    }
}

/// ⌘N / ghostty NEW_WINDOW：新窗口（独立窗口，不成 tab）。parent =
/// 发起面（inherited_config(context=WINDOW) 继承字号/工作目录）。
pub fn new_window(mtm: MainThreadMarker, parent: Option<&SurfaceHostView>) -> Retained<NSWindow> {
    let w = make_window(mtm, parent, ghostty_sys::GHOSTTY_SURFACE_CONTEXT_WINDOW);
    // E2E 虚拟屏时 make_window 已定窗，不叠 center。
    if std::env::var_os("NINJA_E2E_SCREEN").is_none() {
        w.center();
    }
    w.makeKeyAndOrderFront(None);
    w
}

/// ⌘T / 系统标签栏 + / ghostty NEW_TAB：新标签挂进当前 key window 的
/// tab 组；首叶 context=TAB。
pub fn new_tab(mtm: MainThreadMarker, parent: Option<&SurfaceHostView>) -> Retained<NSWindow> {
    let app = NSApplication::sharedApplication(mtm);
    let host_window = app.keyWindow().or_else(|| app.mainWindow());
    // parent 优先（ghostty action 的 target 面）；否则 key window 的焦点面。
    // parent 优先；否则 key window 的焦点面（绑定 Retained 保持借用有效）。
    let from_host: Option<Retained<crate::surface::SurfaceHostView>> = host_window
        .as_ref()
        .and_then(|w| container_of(w))
        .and_then(|c| c.focused_leaf().or_else(|| c.leaves().first().cloned()));
    let parent = parent.filter(|p| p.surface_opt().is_some()).or(from_host.as_deref());
    let w = make_window(mtm, parent, ghostty_sys::GHOSTTY_SURFACE_CONTEXT_TAB);
    match &host_window {
        Some(host) => {
            host.addTabbedWindow_ordered(&w, NSWindowOrderingMode::Above);
            w.makeKeyAndOrderFront(None);
        }
        None => {
            w.makeKeyAndOrderFront(None);
        }
    }
    w
}

/// ⌘Q 退出前 / 退出收尾：遍历所有窗口的容器（terminate 不逐窗发
/// close），全叶延迟 free。
pub fn shutdown_all_windows(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    for w in app.windows().iter() {
        if let Some(container) = container_of(&w) {
            container.shutdown_all();
        }
    }
}

/// windowWillClose：单窗收尾（contentView 的 pane 容器，全叶延迟 free）。
pub fn window_closed(content: &objc2_app_kit::NSView) {
    if !crate::pane::is_container(content) {
        return;
    }
    crate::pane::downcast_container(content).shutdown_all();
}

/// msg_send 用的 nil sender（Option<&AnyObject> 编码为 null id）。
fn nil_sender() -> Option<&'static objc2::runtime::AnyObject> {
    None
}

/// ghostty GOTO_TAB（菜单 ⌘⇧[/⌘⇧] 走 NSWindow 内建动作不经这里；
/// 键位/钩子路径用）。
pub fn goto_tab(w: &NSWindow, goto: ghostty_sys::ghostty_action_goto_tab_e) {
    use ghostty_sys::*;
    match goto {
        GHOSTTY_GOTO_TAB_PREVIOUS => {
            // SAFETY: NSWindow 内建 selectPreviousTab:（sender=nil）。
            unsafe { objc2::msg_send![w, selectPreviousTab: nil_sender()] }
        }
        GHOSTTY_GOTO_TAB_NEXT => {
            // SAFETY: NSWindow 内建 selectNextTab:（sender=nil）。
            unsafe { objc2::msg_send![w, selectNextTab: nil_sender()] }
        }
        GHOSTTY_GOTO_TAB_LAST => {
            if let Some(group) = w.tabGroup() {
                let windows = group.windows();
                if let Some(last) = windows.into_iter().last() {
                    last.makeKeyAndOrderFront(None);
                }
            }
        }
        _ => {}
    }
}

/// ghostty MOVE_TAB（macOS TerminalController 同款算法：摘出 → 目标位
/// 上下插回）。
pub fn move_tab(w: &NSWindow, amount: isize) {
    if amount == 0 {
        return;
    }
    let Some(group) = w.tabGroup() else {
        return;
    };
    let Some(selected) = group.selectedWindow() else {
        return;
    };
    let windows: Vec<Retained<NSWindow>> = group.windows().into_iter().collect();
    let count = windows.len();
    if count == 0 {
        return;
    }
    let Some(selected_index) = windows
        .iter()
        .position(|x| std::ptr::eq(&**x as *const NSWindow, &*selected as *const NSWindow))
    else {
        return;
    };
    let final_index = if amount < 0 {
        selected_index.saturating_sub((-amount) as usize)
    } else {
        (selected_index + amount as usize).min(count - 1)
    };
    if final_index == selected_index {
        return;
    }
    let target = &windows[final_index];
    group.removeWindow(&selected);
    let ordering = if amount < 0 {
        NSWindowOrderingMode::Below
    } else {
        NSWindowOrderingMode::Above
    };
    target.addTabbedWindow_ordered(&selected, ordering);
}

/// ghostty CLOSE_TAB(this/other/right)：tab 组操作。this → 当前 tab
/// performClose（windowShouldClose 决策放行）；other/right → 其余 tab
/// close（非裸⌘W 路径，整 tab 关）。
pub fn close_tab(w: &NSWindow, mode: ghostty_sys::ghostty_action_close_tab_mode_e) {
    use ghostty_sys::*;
    let Some(group) = w.tabGroup() else {
        w.performClose(None);
        return;
    };
    let windows: Vec<Retained<NSWindow>> = group.windows().into_iter().collect();
    if windows.len() <= 1 {
        // 单 tab 组：this = 关窗；other/right 无对象。
        if matches!(mode, GHOSTTY_ACTION_CLOSE_TAB_MODE_THIS) {
            w.performClose(None);
        }
        return;
    }
    match mode {
        GHOSTTY_ACTION_CLOSE_TAB_MODE_THIS => w.performClose(None),
        GHOSTTY_ACTION_CLOSE_TAB_MODE_OTHER => {
            for other in windows.iter() {
                if !same_window(other, w) {
                    other.close();
                }
            }
        }
        GHOSTTY_ACTION_CLOSE_TAB_MODE_RIGHT => {
            let mut after = false;
            for win in windows.iter() {
                if same_window(win, w) {
                    after = true;
                    continue;
                }
                if after {
                    win.close();
                }
            }
        }
        _ => {}
    }
}

fn same_window(a: &NSWindow, b: &NSWindow) -> bool {
    std::ptr::eq(a as *const NSWindow, b as *const NSWindow)
}

/// ghostty GOTO_WINDOW previous/next（NSApp 窗口表序，只数终端窗）。
pub fn goto_window(mtm: MainThreadMarker, goto: ghostty_sys::ghostty_action_goto_window_e) {
    use ghostty_sys::*;
    let app = NSApplication::sharedApplication(mtm);
    let list: Vec<Retained<NSWindow>> = app
        .windows()
        .into_iter()
        .filter(|w| container_of(w).is_some())
        .collect();
    if list.len() < 2 {
        return;
    }
    let idx = list.iter().position(|w| w.isKeyWindow());
    let next = match (goto, idx) {
        (GHOSTTY_GOTO_WINDOW_NEXT, Some(i)) => (i + 1) % list.len(),
        (GHOSTTY_GOTO_WINDOW_PREVIOUS, Some(i)) => (i + list.len() - 1) % list.len(),
        _ => 0,
    };
    list[next].makeKeyAndOrderFront(None);
}

/// ghostty NEW_SPLIT 方向 → (布局方向, before=左/上分)。
pub fn split_dir_of(
    d: ghostty_sys::ghostty_action_split_direction_e,
) -> Option<(crate::pane::Dir, bool)> {
    use ghostty_sys::*;
    Some(match d {
        GHOSTTY_SPLIT_DIRECTION_RIGHT => (crate::pane::Dir::Horizontal, false),
        GHOSTTY_SPLIT_DIRECTION_DOWN => (crate::pane::Dir::Vertical, false),
        GHOSTTY_SPLIT_DIRECTION_LEFT => (crate::pane::Dir::Horizontal, true),
        GHOSTTY_SPLIT_DIRECTION_UP => (crate::pane::Dir::Vertical, true),
        _ => return None,
    })
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
        // 单 pane：放行 → 关当前 tab/窗，最后一个才关窗。
        assert!(should_close_whole_window(1, true));
        assert!(should_close_whole_window(0, true)); // 防御：无叶子视为单面
    }

    #[test]
    fn non_cmd_w_paths_always_close_whole_window() {
        // 红绿灯 / Close Window(⇧⌘W) / Close All·Other Tabs(⌥⌘W) /
        // 菜单点击 / EOF 级联 / selftest：整窗/整 tab 关不受影响。
        assert!(should_close_whole_window(2, false));
        assert!(should_close_whole_window(3, false));
        assert!(should_close_whole_window(1, false));
    }
}
