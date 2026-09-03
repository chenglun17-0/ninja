//! q1 壳：多窗口 + 原生标签（NSWindow tabbing）+ surface 关闭汇聚。
//! 移植自 v1 crates/ninja/src/shell.rs（p2/D-A/X2 资产），叶子和关闭协议
//! 换成嵌入 surface。
//!
//! - ⌘N 新窗口；⌘T `newWindowForTab:` 新标签（系统标签栏 + 菜单同走），
//!   `addTabbedWindow:ordered:` 挂进当前窗口的 tab 组；
//! - ghostty `close_surface`（⌘W 默认绑定）→ `close_surface_cb` →
//!   [`handle_surface_close`]（≡ v1 handle_pane_eof）：多 pane 拆焦点叶
//!   并 surface_free、单 pane performClose 关 tab/窗；活进程按
//!   Ghostty `confirm-close-surface` 弹确认（libghostty 已把配置折进
//!   `process_alive` / `needsConfirmQuit`）；
//! - `windowShouldClose` 的裸⌘W 决策（菜单 Close=performClose 路径，
//!   多 pane 只关焦点面、单 pane 放行原生语义）+ 整 tab/窗关闭确认；
//! - `windowWillClose` → 全叶 surface_free（延迟，见 [`crate::host`]）。

use std::cell::Cell;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, msg_send};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSAppearance, NSAppearanceCustomization,
    NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication, NSBackingStoreType, NSEventType,
    NSScreen, NSTextField, NSView, NSWindow, NSWindowOrderingMode, NSWindowStyleMask,
    NSWindowTabbingMode,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSUserDefaults};

use crate::host;
use crate::pane::container_of;
use crate::surface::SurfaceHostView;

/// 所有终端窗口共用的 tabbing identifier（相同才能自动成组）。
pub(crate) const TABBING_ID: &str = "ninja-terminal";
const LAST_FRAME_KEY: &str = "dev.ninja.last-window-frame";
const QUIT_KEEPS_WINDOWS_KEY: &str = "NSQuitAlwaysKeepsWindows";

/// Ghostty `TerminalController.lastCascadePoint`：新窗错开，不叠在上一扇上。
static LAST_CASCADE: Mutex<NSPoint> = Mutex::new(NSPoint { x: 0.0, y: 0.0 });

std::thread_local! {
    /// 确认框重入守卫：modal 期间再来一次 close 直接拦，不叠第二张。
    static CONFIRMING: Cell<bool> = const { Cell::new(false) };
}

/// 开窗尺寸：`window-width/height` 都 >0 用格子；否则铺满当前屏可见区域。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeChoice {
    Maximize,
    InitialSize,
}

/// Ghostty 开窗原点：配置坐标 → 上次位置 → 居中；多窗再 cascade。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginChoice {
    ConfigPos,
    Restored,
    Center,
}

pub fn choose_size(has_initial_size: bool) -> SizeChoice {
    if has_initial_size {
        SizeChoice::InitialSize
    } else {
        SizeChoice::Maximize
    }
}

/// `window-position-x/y` 都设 > 上次原点 > 居中。
pub fn choose_origin(has_config_pos: bool, has_restored_origin: bool) -> OriginChoice {
    if has_config_pos {
        OriginChoice::ConfigPos
    } else if has_restored_origin {
        OriginChoice::Restored
    } else {
        OriginChoice::Center
    }
}

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
    window.setTabbingMode(NSWindowTabbingMode::Preferred);
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

    // 虚拟屏先落位再建面，scale 才对。
    place_on_e2e_screen(&window);

    let first = container.first_leaf();
    let parent = parent.filter(|p| p.surface_opt().is_some());
    host::attach_surface(&first, context, parent, None);
    // Ghostty：window-width/height 都 >0 才发 INITIAL_SIZE；否则 800×600。
    if !window.isVisible() {
        let (w, h) = first
            .ivars()
            .initial_pt
            .get()
            .filter(|(w, h)| *w > 0 && *h > 0)
            .unwrap_or((800, 600));
        window.setContentSize(objc2_foundation::NSSize::new(w as f64, h as f64));
    }
    place_on_e2e_screen(&window);
    window
}

/// 会话恢复：按保存的分屏树和工作目录建窗，不走默认 cwd。
pub fn make_window_restored(
    mtm: MainThreadMarker,
    context: ghostty_sys::ghostty_surface_context_e,
    tab: &crate::session::SessionTab,
    frame: NSRect,
) -> Retained<NSWindow> {
    let container = crate::pane::PaneContainer::new(mtm);
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setTitle(&NSString::from_str("ninja"));
    window.setTabbingIdentifier(&NSString::from_str(TABBING_ID));
    window.setTabbingMode(NSWindowTabbingMode::Preferred);
    window.setContentView(Some(&container));
    apply_chrome(&window);
    crate::app::wire_window(&window);
    unsafe { window.setReleasedWhenClosed(false) };
    container.restore_layout(&tab.tree, context);
    if let Some(t) = &tab.title_override {
        container.set_title_override(Some(t.clone()));
    }
    window.setFrame_display(frame, false);
    window
}

/// Ghostty Change Tab Title：空白则回到 OSC 标题。
pub fn prompt_tab_title(view: &Option<Retained<SurfaceHostView>>) {
    let Some(v) = view else {
        return;
    };
    let Some(w) = v.window() else {
        return;
    };
    prompt_tab_title_for_window(&w);
}

pub fn prompt_tab_title_for_window(w: &NSWindow) {
    if crate::tab_rename::begin_inline(w) {
        return;
    }
    let Some(c) = container_of(w) else {
        // 预览 tab：直接改 window title。
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str("Change Tab Title"));
        let field: Retained<NSTextField> = unsafe {
            msg_send![NSTextField::alloc(mtm), initWithFrame: NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(250.0, 24.0),
            )]
        };
        field.setStringValue(&w.title());
        alert.setAccessoryView(Some(&field));
        alert.addButtonWithTitle(&NSString::from_str("OK"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        if alert.runModal() != NSAlertFirstButtonReturn {
            return;
        }
        let t = field.stringValue().to_string();
        if !t.is_empty() {
            w.setTitle(&NSString::from_str(&t));
        }
        return;
    };
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str("Change Tab Title"));
    alert.setInformativeText(&NSString::from_str("Leave blank to restore the default."));
    let field: Retained<NSTextField> = unsafe {
        msg_send![NSTextField::alloc(mtm), initWithFrame: NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(250.0, 24.0),
        )]
    };
    let current = c.title_override().unwrap_or_else(|| w.title().to_string());
    field.setStringValue(&NSString::from_str(&current));
    alert.setAccessoryView(Some(&field));
    alert.addButtonWithTitle(&NSString::from_str("OK"));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    let resp = alert.runModal();
    if resp != NSAlertFirstButtonReturn {
        return;
    }
    let new_title = field.stringValue().to_string();
    if new_title.is_empty() {
        c.set_title_override(None);
    } else {
        c.set_title_override(Some(new_title));
    }
}

/// 按 Ghostty `showWindow` + `applyCascade` 展示：尺寸/原点优先级见
/// [`choose_size`]/[`choose_origin`]；多窗且未钉坐标时 cascade。
/// E2E 虚拟屏只 orderFront（落位已在 make_window）。
pub fn present_window(window: &NSWindow) {
    apply_quit_keeps_windows();
    if std::env::var_os("NINJA_E2E_SCREEN").is_none() {
        apply_open_policy(window);
    }
    window.makeKeyAndOrderFront(None);
}

fn apply_open_policy(window: &NSWindow) {
    let cfg = host::config();
    let pos = match (
        cfg.and_then(|c| crate::config::get_i16(c, "window-position-x")),
        cfg.and_then(|c| crate::config::get_i16(c, "window-position-y")),
    ) {
        (Some(x), Some(y)) => Some((x, y)),
        _ => None,
    };
    let has_initial = window_has_initial_size(window);
    let saved = load_last_frame();
    match choose_size(has_initial) {
        SizeChoice::InitialSize => {
            let origin = choose_origin(pos.is_some(), saved.is_some());
            match origin {
                OriginChoice::ConfigPos => {
                    if let Some((x, y)) = pos {
                        apply_config_position(window, x, y);
                    }
                }
                OriginChoice::Restored => {
                    if let Some(saved) = saved {
                        let mut f = window.frame();
                        f.origin = saved.origin;
                        clamp_to_visible(window, &mut f);
                        window.setFrame_display(f, true);
                    }
                }
                OriginChoice::Center => window.center(),
            }
            apply_cascade(window, pos.is_some());
        }
        SizeChoice::Maximize => {
            if let Some(screen) = window.screen().or_else(fallback_screen) {
                window.setFrame_display(screen.visibleFrame(), true);
            }
            if pos.is_some() {
                if let Some((x, y)) = pos {
                    apply_config_position(window, x, y);
                }
            } else {
                apply_cascade(window, false);
            }
        }
    }
    apply_restorable(window);
}

fn window_has_initial_size(window: &NSWindow) -> bool {
    container_of(window)
        .and_then(|c| c.leaves().into_iter().next())
        .and_then(|v| v.ivars().initial_pt.get())
        .is_some_and(|(w, h)| w > 0 && h > 0)
}

fn fallback_screen() -> Option<objc2::rc::Retained<NSScreen>> {
    MainThreadMarker::new().and_then(NSScreen::mainScreen)
}

fn apply_config_position(window: &NSWindow, x: i16, y: i16) {
    let Some(screen) = window.screen().or_else(fallback_screen) else {
        return;
    };
    let vf = screen.visibleFrame();
    let size = window.frame().size;
    let mut origin = NSPoint::new(
        vf.origin.x + f64::from(x),
        vf.origin.y + vf.size.height - f64::from(y) - size.height,
    );
    origin.x = origin
        .x
        .clamp(vf.origin.x, vf.origin.x + vf.size.width - size.width);
    origin.y = origin
        .y
        .clamp(vf.origin.y, vf.origin.y + vf.size.height - size.height);
    window.setFrameOrigin(origin);
}

fn clamp_to_visible(window: &NSWindow, frame: &mut NSRect) {
    let Some(screen) = window.screen().or_else(fallback_screen) else {
        return;
    };
    let vf = screen.visibleFrame();
    frame.origin.x = frame
        .origin
        .x
        .clamp(vf.origin.x, vf.origin.x + vf.size.width - frame.size.width);
    frame.origin.y = frame.origin.y.clamp(
        vf.origin.y,
        vf.origin.y + vf.size.height - frame.size.height,
    );
}

fn apply_cascade(window: &NSWindow, has_fixed_pos: bool) {
    if has_fixed_pos {
        return;
    }
    let count = ninja_window_count();
    let Ok(mut last) = LAST_CASCADE.lock() else {
        return;
    };
    if count > 1 {
        *last = window.cascadeTopLeftFromPoint(*last);
    } else {
        *last = window.cascadeTopLeftFromPoint(NSPoint::ZERO);
    }
}

fn ninja_window_count() -> usize {
    let Some(mtm) = MainThreadMarker::new() else {
        return 0;
    };
    NSApplication::sharedApplication(mtm)
        .windows()
        .iter()
        .filter(|w| container_of(w).is_some())
        .count()
}

fn apply_quit_keeps_windows() {
    let state = host::config()
        .and_then(|c| crate::config::get_enum_str(c, "window-save-state"))
        .unwrap_or_else(|| "default".into());
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = NSString::from_str(QUIT_KEEPS_WINDOWS_KEY);
    match state.as_str() {
        "never" => unsafe {
            let _: () = msg_send![&defaults, setBool: false, forKey: &*key];
        },
        "always" => unsafe {
            let _: () = msg_send![&defaults, setBool: true, forKey: &*key];
        },
        _ => defaults.removeObjectForKey(&key),
    }
}

fn apply_restorable(window: &NSWindow) {
    let state = host::config()
        .and_then(|c| crate::config::get_enum_str(c, "window-save-state"))
        .unwrap_or_else(|| "default".into());
    window.setRestorable(state != "never");
}

/// 热重载时同步 NSQuitAlwaysKeepsWindows（Ghostty configDidChange 同款）。
pub fn sync_save_state() {
    apply_quit_keeps_windows();
}

/// Ghostty LastWindowPosition：可见窗才写，避免装饰变化覆盖。
pub fn save_last_frame(window: &NSWindow) {
    if !window.isVisible() {
        return;
    }
    let f = window.frame();
    let s = format!(
        "{} {} {} {}",
        f.origin.x, f.origin.y, f.size.width, f.size.height
    );
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = NSString::from_str(LAST_FRAME_KEY);
    let val = NSString::from_str(&s);
    unsafe {
        defaults.setObject_forKey(Some(&val), &key);
    }
}

fn load_last_frame() -> Option<NSRect> {
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = NSString::from_str(LAST_FRAME_KEY);
    let s: Option<Retained<NSString>> = unsafe { msg_send![&defaults, stringForKey: &*key] };
    let s = s?.to_string();
    let mut it = s.split_whitespace();
    let x: f64 = it.next()?.parse().ok()?;
    let y: f64 = it.next()?.parse().ok()?;
    let w: f64 = it.next()?.parse().ok()?;
    let h: f64 = it.next()?.parse().ok()?;
    Some(NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)))
}

/// NINJA_E2E_SCREEN=<displayID>（PLAN「E2E 虚拟屏幕」增补，q0 平移）：
/// 窗口落到指定虚拟屏（按 deviceDescription NSScreenNumber 匹配）居中，
/// 尺寸夹到 visibleFrame 内；未设置/未匹配 → 系统默认不动。这是取证
/// 钩子，不是产品配置。
fn place_on_e2e_screen(window: &NSWindow) {
    let Ok(id) = std::env::var("NINJA_E2E_SCREEN") else {
        return;
    };
    let Ok(target) = id.trim().parse::<u32>() else {
        println!("screen: NINJA_E2E_SCREEN={id:?} 非法，回退系统默认");
        return;
    };
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
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

/// 顶栏不能是会采样的材质。Tahoe 标题栏默认 Liquid Glass，会把下面的
/// Metal 帧当背景；终端一滚，顶栏就闪。透明标题栏 + 关掉采样层之后，
/// 露出的是窗口底色（终端色、静态），和内容面互不合成。
pub fn apply_chrome(window: &NSWindow) {
    window.setTitlebarAppearsTransparent(true);
    window.setTitlebarSeparatorStyle(objc2_app_kit::NSTitlebarSeparatorStyle::None);
    window.setOpaque(true);
    let (r, g, b) = host::bg_rgb();
    window.setBackgroundColor(Some(&host::bg_color()));
    // SAFETY: NSAppearanceName* 是框架提供的常量字符串。
    let name = unsafe {
        if bg_is_light(r, g, b) {
            NSAppearanceNameAqua
        } else {
            NSAppearanceNameDarkAqua
        }
    };
    window.setAppearance(NSAppearance::appearanceNamed(name).as_deref());
    suppress_titlebar_sampling(window);
    paint_titlebar_solid(window, r, g, b);
}

/// 关掉标题栏里的采样层（Glass / VisualEffect / TitlebarBackground）。
/// 只 setHidden；不 setWantsLayer、不改 backgroundColor——那些会自己触发重绘。
pub fn suppress_titlebar_sampling(window: &NSWindow) {
    let Some(cv) = window.contentView() else {
        return;
    };
    let Some(root) = (unsafe { cv.superview() }) else {
        return;
    };
    hide_sampling_views(&root, false);
}

fn paint_titlebar_solid(window: &NSWindow, r: u8, g: u8, b: u8) {
    let Some(cv) = window.contentView() else {
        return;
    };
    let Some(root) = (unsafe { cv.superview() }) else {
        return;
    };
    let Some(space) = objc2_core_graphics::CGColorSpace::new_device_rgb() else {
        return;
    };
    let comps = [
        f64::from(r) / 255.0,
        f64::from(g) / 255.0,
        f64::from(b) / 255.0,
        1.0,
    ];
    let Some(color) = (unsafe { objc2_core_graphics::CGColor::new(Some(&space), comps.as_ptr()) })
    else {
        return;
    };
    paint_titlebar_view(&root, &color);
}

fn paint_titlebar_view(v: &NSView, color: &objc2_core_graphics::CGColor) {
    if class_name(v) == "NSTitlebarView" {
        v.setWantsLayer(true);
        if let Some(layer) = v.layer() {
            layer.setOpaque(true);
            layer.setBackgroundColor(Some(color));
        }
        return;
    }
    for sub in v.subviews() {
        paint_titlebar_view(&sub, color);
    }
}

fn hide_sampling_views(v: &NSView, in_titlebar: bool) {
    let name = class_name(v);
    let in_titlebar = in_titlebar || name == "NSTitlebarContainerView";
    if in_titlebar
        && matches!(
            name.as_str(),
            "NSTitlebarBackgroundView" | "_NSTitlebarDecorationView"
        )
        && !v.isHidden()
    {
        v.setHidden(true);
    }
    for sub in v.subviews() {
        hide_sampling_views(&sub, in_titlebar);
    }
}

fn class_name(v: &NSView) -> String {
    let s: objc2::rc::Retained<NSString> = unsafe { objc2::msg_send![v, className] };
    s.to_string()
}

/// Rec. 601 亮度（与 Ghostty `OSColor.luminance` 同款）。
fn bg_is_light(r: u8, g: u8, b: u8) -> bool {
    let r = f64::from(r) / 255.0;
    let g = f64::from(g) / 255.0;
    let b = f64::from(b) / 255.0;
    0.299 * r + 0.587 * g + 0.114 * b > 0.5
}

/// 关窗确认文案（Ghostty `BaseTerminalController` / `TerminalController` 同款）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseConfirmKind {
    Surface,
    Tab,
    Window,
    OtherTabs,
    TabsToTheRight,
    Quit,
}

impl CloseConfirmKind {
    fn copy(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Surface => (
                "Close Terminal?",
                "The terminal still has a running process. If you close the terminal the process will be killed.",
                "Close",
            ),
            Self::Tab => (
                "Close Tab?",
                "The terminal still has a running process. If you close the tab the process will be killed.",
                "Close",
            ),
            Self::Window => (
                "Close Window?",
                "All terminal sessions in this window will be terminated.",
                "Close",
            ),
            Self::OtherTabs => (
                "Close Other Tabs?",
                "At least one other tab still has a running process. If you close the tab the process will be killed.",
                "Close",
            ),
            Self::TabsToTheRight => (
                "Close Tabs on the Right?",
                "At least one tab to the right still has a running process. If you close the tab the process will be killed.",
                "Close",
            ),
            Self::Quit => (
                "Quit ninja?",
                "The terminal still has a running process. If you quit, the process will be killed.",
                "Terminate",
            ),
        }
    }
}

/// 关整个 tab/窗时的文案：组里还有别的 tab → Tab，否则 Window。
pub(crate) fn close_window_or_tab_kind(tab_count: usize) -> CloseConfirmKind {
    if tab_count > 1 {
        CloseConfirmKind::Tab
    } else {
        CloseConfirmKind::Window
    }
}

fn confirm_close_suppressed() -> bool {
    std::env::var_os("NINJA_P2_SELFTEST").is_some()
        || std::env::var_os("NINJA_E2E_SCREEN").is_some()
}

fn tab_count(w: &NSWindow) -> usize {
    w.tabGroup().map(|g| g.windows().len()).unwrap_or(1).max(1)
}

fn window_needs_confirm(w: &NSWindow) -> bool {
    if confirm_close_suppressed() {
        return false;
    }
    container_of(w).is_some_and(|c| c.leaves().iter().any(|v| v.needs_confirm_quit()))
}

/// 任一面需要确认才拦 ⌘Q（libghostty `ghostty_app_needs_confirm_quit`）。
pub fn app_needs_confirm_quit() -> bool {
    if confirm_close_suppressed() {
        return false;
    }
    let mut needs = false;
    host::with_app(|app| {
        // SAFETY: 公开 C API；app 句柄由 host 单例保证存活。
        needs = unsafe { ghostty_sys::ghostty_app_needs_confirm_quit(app) };
    });
    needs
}

fn run_close_confirm(kind: CloseConfirmKind) -> bool {
    if confirm_close_suppressed() {
        return true;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return true;
    };
    if CONFIRMING.with(|c| c.get()) {
        return false;
    }
    CONFIRMING.with(|c| c.set(true));
    let (title, info, button) = kind.copy();
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(info));
    alert.setAlertStyle(NSAlertStyle::Warning);
    alert.addButtonWithTitle(&NSString::from_str(button));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    let resp = alert.runModal();
    CONFIRMING.with(|c| c.set(false));
    resp == NSAlertFirstButtonReturn
}

/// ⌘Q 确认（取消 → 不退出）。
pub fn confirm_quit() -> bool {
    run_close_confirm(CloseConfirmKind::Quit)
}

/// ⇧⌘W / `CLOSE_WINDOW`：整 tab 关（`close` 跳过裸⌘W 拆 pane），活进程先确认。
pub fn confirm_then_close_window(w: &NSWindow) {
    if window_needs_confirm(w) && !run_close_confirm(close_window_or_tab_kind(tab_count(w))) {
        return;
    }
    w.close();
}

/// `CLOSE_ALL_WINDOWS`：有活进程先确认一次，再逐窗 `close`。
pub fn confirm_then_close_all_windows(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    let list: Vec<Retained<NSWindow>> = app
        .windows()
        .into_iter()
        .filter(|w| container_of(w).is_some())
        .collect();
    if list.iter().any(|w| window_needs_confirm(w)) && !run_close_confirm(CloseConfirmKind::Window)
    {
        return;
    }
    crate::session::save();
    crate::session::begin_quit();
    for w in list {
        w.close();
    }
}

fn close_leaf_maybe_confirm(
    container: &crate::pane::PaneContainer,
    view: &SurfaceHostView,
    process_alive: bool,
) {
    if process_alive && !run_close_confirm(CloseConfirmKind::Surface) {
        return;
    }
    container.close_leaf(view);
}

/// ghostty close_surface（⌘W 默认绑定）/ EOF（close_surface_cb）的宿主侧
/// 决策 ≡ v1 handle_pane_eof：多 pane 窗拆该面（surface 延迟 free），
/// 单 pane 窗 performClose 关 tab/窗。`process_alive` 为真时先确认。
/// 最后一面把确认交给 `windowShouldClose`，避免双弹。
pub fn handle_surface_close(view: &SurfaceHostView, process_alive: bool) {
    let Some(w) = view.window() else {
        // 已脱窗（关窗中）：surface 由 windowWillClose 统一拆。
        host::close_leaf_deferred(view);
        return;
    };
    let Some(container) = container_of(&w) else {
        return;
    };
    if container.leaf_count() <= 1 {
        w.performClose(None);
        return;
    }
    close_leaf_maybe_confirm(container, view, process_alive);
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
/// 其余路径关整个 tab/窗，活进程先确认。
pub fn window_should_close(w: &NSWindow) -> bool {
    if close_request_is_bare_cmd_key()
        && let Some(container) = container_of(w)
        && !should_close_whole_window(container.leaf_count(), true)
    {
        // 多 pane：只关焦点面（close_leaf 自带焦点转移 + surface 延迟 free）。
        return match container.focused_leaf() {
            Some(f) => {
                close_leaf_maybe_confirm(container, &f, f.needs_confirm_quit());
                false
            }
            None => true, // 无焦点叶子（异常态）：放行原语义，不硬拦
        };
    }
    if window_needs_confirm(w) && !run_close_confirm(close_window_or_tab_kind(tab_count(w))) {
        return false;
    }
    true
}

/// ⌘N / ghostty NEW_WINDOW：新窗口（独立窗口，不成 tab）。parent =
/// 发起面（inherited_config(context=WINDOW) 继承字号/工作目录）。
pub fn new_window(mtm: MainThreadMarker, parent: Option<&SurfaceHostView>) -> Retained<NSWindow> {
    let w = make_window(mtm, parent, ghostty_sys::GHOSTTY_SURFACE_CONTEXT_WINDOW);
    present_window(&w);
    crate::session::note_new_window(&w);
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
    let parent = parent
        .filter(|p| p.surface_opt().is_some())
        .or(from_host.as_deref());
    let w = make_window(mtm, parent, ghostty_sys::GHOSTTY_SURFACE_CONTEXT_TAB);
    match &host_window {
        Some(host) => {
            host.addTabbedWindow_ordered(&w, NSWindowOrderingMode::Above);
            w.makeKeyAndOrderFront(None);
            crate::session::note_new_tab(host, &w);
            suppress_titlebar_sampling(host);
            suppress_titlebar_sampling(&w);
        }
        None => {
            w.makeKeyAndOrderFront(None);
            crate::session::note_new_window(&w);
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
pub fn window_closed(window: &NSWindow, content: &objc2_app_kit::NSView) {
    crate::plugins::layer_tab_closed(window, content);
    if !crate::pane::is_container(content) {
        return;
    }
    crate::pane::downcast_container(content).shutdown_all();
}

/// 插件层标签：无 PTY 的 chrome 窗，挂进当前 tab 组。content 由调用方
/// 提供（像素 LayerView 或 html WKWebView），标题走 `title`。
pub fn new_chrome_tab(
    mtm: MainThreadMarker,
    title: &str,
    content: &NSView,
    parent: Option<&NSWindow>,
) -> Retained<NSWindow> {
    let app = NSApplication::sharedApplication(mtm);
    let key = app.keyWindow().or_else(|| app.mainWindow());
    let host: Option<&NSWindow> = parent.or(key.as_deref());
    let content_size = host
        .map(|w| w.contentRectForFrameRect(w.frame()).size)
        .unwrap_or(NSSize::new(800.0, 600.0));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), content_size),
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setTitle(&NSString::from_str(title));
    window.setTabbingIdentifier(&NSString::from_str(TABBING_ID));
    window.setTabbingMode(NSWindowTabbingMode::Preferred);
    content.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), content_size));
    window.setContentView(Some(content));
    apply_chrome(&window);
    crate::app::wire_window(&window);
    unsafe { window.setReleasedWhenClosed(false) };
    match host {
        Some(host) => {
            host.addTabbedWindow_ordered(&window, NSWindowOrderingMode::Above);
            window.makeKeyAndOrderFront(None);
            suppress_titlebar_sampling(host);
            suppress_titlebar_sampling(&window);
        }
        None => {
            window.makeKeyAndOrderFront(None);
        }
    }
    let _ = window.makeFirstResponder(Some(content));
    window
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
        n if n >= 1 => {
            // Ghostty `goto_tab:1`…：1-based。
            if let Some(group) = w.tabGroup() {
                let idx = (n as usize).saturating_sub(1);
                if let Some(tw) = group.windows().into_iter().nth(idx) {
                    tw.makeKeyAndOrderFront(None);
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
    crate::session::note_move(&selected, amount);
}

/// ghostty CLOSE_TAB(this/other/right)：tab 组操作。this → 当前 tab
/// performClose（windowShouldClose 确认）；other/right → 有活进程先确认
/// 再 `close`（跳过 windowShouldClose，避免逐 tab 再弹）。
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
            let others: Vec<&NSWindow> = windows
                .iter()
                .filter(|other| !same_window(other, w))
                .map(|other| &**other)
                .collect();
            if others.iter().any(|other| window_needs_confirm(other))
                && !run_close_confirm(CloseConfirmKind::OtherTabs)
            {
                return;
            }
            for other in others {
                other.close();
            }
        }
        GHOSTTY_ACTION_CLOSE_TAB_MODE_RIGHT => {
            let mut after = false;
            let mut right = Vec::new();
            for win in windows.iter() {
                if same_window(win, w) {
                    after = true;
                    continue;
                }
                if after {
                    right.push(&**win);
                }
            }
            if right.iter().any(|win| window_needs_confirm(win))
                && !run_close_confirm(CloseConfirmKind::TabsToTheRight)
            {
                return;
            }
            for win in right {
                win.close();
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
    fn odp_background_is_dark_so_title_uses_dark_aqua() {
        // Ghostty 默认 background #282c34；浅色外观会让标题字变成黑字叠在深色底上（看不见）。
        assert!(!super::bg_is_light(0x28, 0x2c, 0x34));
        assert!(super::bg_is_light(0xf5, 0xf5, 0xf5));
    }

    #[test]
    fn window_size_priority_matches_ghostty() {
        use super::{SizeChoice, choose_size};
        assert_eq!(choose_size(true), SizeChoice::InitialSize);
        assert_eq!(choose_size(false), SizeChoice::Maximize);
    }

    #[test]
    fn window_origin_priority_matches_ghostty() {
        use super::{OriginChoice, choose_origin};
        assert_eq!(choose_origin(true, true), OriginChoice::ConfigPos);
        assert_eq!(choose_origin(false, true), OriginChoice::Restored);
        assert_eq!(choose_origin(false, false), OriginChoice::Center);
    }

    #[test]
    fn non_cmd_w_paths_always_close_whole_window() {
        // 红绿灯 / Close Window(⇧⌘W) / Close All·Other Tabs(⌥⌘W) /
        // 菜单点击 / EOF 级联 / selftest：整窗/整 tab 关不受影响。
        assert!(should_close_whole_window(2, false));
        assert!(should_close_whole_window(3, false));
        assert!(should_close_whole_window(1, false));
    }

    #[test]
    fn close_confirm_kind_matches_ghostty_copy() {
        use super::CloseConfirmKind;
        assert_eq!(super::close_window_or_tab_kind(1), CloseConfirmKind::Window);
        assert_eq!(super::close_window_or_tab_kind(2), CloseConfirmKind::Tab);
        assert_eq!(super::close_window_or_tab_kind(0), CloseConfirmKind::Window);
        let (title, _, button) = CloseConfirmKind::Quit.copy();
        assert_eq!(title, "Quit ninja?");
        assert_eq!(button, "Terminate");
        assert_eq!(CloseConfirmKind::Surface.copy().2, "Close");
    }
}
