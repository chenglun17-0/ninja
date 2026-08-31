//! q1 宿主单例：ghostty app/config 句柄、运行时回调（wakeup/action/
//! clipboard/close_surface）、surface 生命周期（建/拆）与 action 全分发。
//!
//! q2 起：配置系统接入——生效配置的派生态（bg/焦点环/分隔条）随
//! [`reload_config`] 刷新；RELOAD_CONFIG/CONFIG_CHANGE/TOGGLE_VISIBILITY
//! 等配置动作进 dispatch（装载管线见 [`crate::config`]）。
//!
//! 所有回调都发生在主线程（action 由 app_tick 的邮箱排空或
//! surface_key 同步触发；wakeup 只唤醒 RunLoop），与 q0 的
//! AtomicPtr 单例同纪律：ghostty_app_tick 会同步重入 action_cb，不能用
//! Mutex（重入死锁），用裸指针单例。

use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;

use ghostty_sys::*;

use crate::pane;
use crate::shell;
use crate::surface::SurfaceHostView;

/// 焦点环颜色源链：cursor-color → foreground → ODP 光标蓝（末位兑底仍是
/// ODP 钉值）。注：cursor-color 是 ?TerminalColor 联合、无 cval——C API
/// app 级句柄读不出（config_get 恒 false，与 link-previews 回读怪象同类），
/// 实际链生效段是 foreground → 钉值。
pub const RING_FALLBACK: (u8, u8, u8) = (0x52, 0x8b, 0xff);
/// 分隔条线色（bg 提亮 ~25%）。
const DIVIDER_LIFT: f64 = 0.25;

struct Host {
    app: ghostty_app_t,
    config: ghostty_config_t,
    /// 收缩后的 ninja.toml（q2 只解析不拉起；监督器 q3）。
    host_config: crate::config::HostConfig,
    /// 装载决策取证（dump 用；reload 时更新）。
    load_info: crate::config::LoadInfo,
    /// 热重载 mtime 快照（NSTimer 轮询比对）。
    watch: crate::config::WatchState,
    /// 已排期的重载拍（去重）。
    reload_scheduled: bool,
    /// 活 surface → 其视图（Retained 持引用直到 surface_free 完成，
    /// 防 use-after-free；ghostty 回调可能晚到一拍）。
    live: Vec<(ghostty_surface_t, Retained<SurfaceHostView>)>,
    /// 待释放 surface（close_surface_cb 可能在 ghostty 调用栈深处触发，
    /// 立即 surface_free 会重入拆栈——延迟一拍 free，macOS 本尊
    /// Task.detached 同款纪律）。元素带视图 Retained，free 完才放。
    pending_free: Vec<(ghostty_surface_t, Retained<SurfaceHostView>)>,
    free_scheduled: bool,
    /// ghostty config 的 background 色（容器/分隔条/窗口 chrome 同源）。
    bg: (u8, u8, u8),
    /// 焦点环色（cursor-color → foreground 链，见 [RING_FALLBACK]）。
    ring: (u8, u8, u8),
}

static HOST: AtomicPtr<Host> = AtomicPtr::new(std::ptr::null_mut());
static NEXT_PANE_ID: AtomicU32 = AtomicU32::new(1);

fn host_opt() -> Option<&'static mut Host> {
    let p = HOST.load(Ordering::Acquire);
    if p.is_null() {
        None
    } else {
        Some(unsafe { &mut *p })
    }
}

pub fn next_pane_id() -> u32 {
    NEXT_PANE_ID.fetch_add(1, Ordering::Relaxed)
}

/// 建宿主（进程一次；main 里 ghostty_init 后调）。装载管线已在
/// [`crate::config::load_pipeline`] 跑完，这里接管句柄 + 派生态首读。
pub fn init(app: ghostty_app_t, config: ghostty_config_t, load_info: crate::config::LoadInfo) {
    assert!(HOST.load(Ordering::Acquire).is_null(), "host already init");
    let host_config = crate::config::load_host_config();
    if !host_config.plugins.enabled.is_empty() {
        // q2 只解析不拉起（监督器 q3）；空载红线：零插件进程/零 socket。
        eprintln!(
            "ninja: ninja.toml 启用插件 {:?}（q2 仅解析，监督器在 q3 拉起）",
            host_config.plugins.enabled
        );
    }
    let watch = crate::config::snapshot_watch(&load_info.watched);
    let mut host = Box::new(Host {
        app,
        config,
        host_config,
        load_info,
        watch,
        reload_scheduled: false,
        live: Vec::new(),
        pending_free: Vec::new(),
        free_scheduled: false,
        bg: (0x16, 0x16, 0x1e),
        ring: RING_FALLBACK,
    });
    refresh_derived(&mut host);
    HOST.store(Box::into_raw(host), Ordering::Release);
}

/// 进程收尾（app.run 返回后）：同步 free 剩余 surface（延迟队列可能没
/// 跑完——RunLoop 已停），再 app/config free。调用后进程退出。
pub fn shutdown() {
    let p = HOST.swap(std::ptr::null_mut(), Ordering::AcqRel);
    if p.is_null() {
        return;
    }
    let mut host = unsafe { Box::from_raw(p) };
    let mut all = std::mem::take(&mut host.live);
    all.append(&mut host.pending_free);
    for (surface, _view) in all {
        unsafe { ghostty_surface_free(surface) };
    }
    unsafe {
        ghostty_app_free(host.app);
        ghostty_config_free(host.config);
    }
}

/// 生效 ghostty config 句柄（菜单键位推导等短句柄读值用）。
pub fn config() -> Option<ghostty_config_t> {
    host_opt().map(|h| h.config)
}

/// NINJA_CFG_DUMP=<path> 时写生效配置取证 JSON（启动/热重载后调；
/// 配置句柄与装载决策都在 Host 里，放这里最顺）。
pub fn dump_config_if_requested() {
    let Some(h) = host_opt() else { return };
    if let Ok(path) = std::env::var("NINJA_CFG_DUMP") {
        crate::config::dump_effective_config(&path, h.config, &h.load_info, &h.host_config);
    }
}

/// 容器/分隔条底色（NSColor；pane.rs drawRect 用）。
pub fn bg_color() -> Retained<objc2_app_kit::NSColor> {
    let Some(h) = host_opt() else {
        return gray(0x16, 0x16, 0x1e);
    };
    gray(h.bg.0, h.bg.1, h.bg.2)
}

/// 焦点环 RGB（pane.rs 焦点环 layer 边框用；cursor-color → foreground 链）。
pub fn ring_rgb() -> (u8, u8, u8) {
    host_opt().map(|h| h.ring).unwrap_or(RING_FALLBACK)
}

/// 分隔条 1px 线色（bg 提亮）。
pub fn divider_color() -> Retained<objc2_app_kit::NSColor> {
    let Some(h) = host_opt() else {
        return gray(0x3e, 0x44, 0x52);
    };
    let (r, g, b) = h.bg;
    let lift = |c: u8| ((f64::from(c) / 255.0 + DIVIDER_LIFT).min(1.0) * 255.0) as u8;
    gray(lift(r), lift(g), lift(b))
}

fn gray(r: u8, g: u8, b: u8) -> Retained<objc2_app_kit::NSColor> {
    objc2_app_kit::NSColor::colorWithSRGBRed_green_blue_alpha(
        f64::from(r) / 255.0,
        f64::from(g) / 255.0,
        f64::from(b) / 255.0,
        1.0,
    )
}

// ---------------------------------------------------------------------------
// 热重载（q2）：管线重跑 + ghostty_app_update_config + 派生态刷新
// ---------------------------------------------------------------------------

/// 从生效配置重读派生态（bg/焦点环）并刷新全部窗口 chrome/重绘。
fn refresh_derived(h: &mut Host) {
    h.bg = crate::config::get_color(h.config, "background").unwrap_or((0x16, 0x16, 0x1e));
    h.ring = crate::config::get_color(h.config, "cursor-color")
        .or_else(|| crate::config::get_color(h.config, "foreground"))
        .unwrap_or(RING_FALLBACK);
    // 全部窗口：背景色/阴影跟随 + 重绘（分隔条/容器底色随 drawRect 重读）。
    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    for w in app.windows().iter() {
        if let Some(container) = pane::container_of(&w) {
            shell::apply_chrome(&w);
            container.refresh_ring();
            if let Some(cv) = w.contentView() {
                cv.setNeedsDisplay(true);
                for sub in cv.subviews() {
                    sub.setNeedsDisplay(true);
                }
            }
        }
    }
}

/// 请求一次热重载（异步：下一拍执行，避免在 ghostty 调用栈内重入
/// ghostty_app_update_config——RELOAD_CONFIG 可能从 surface_key 栈同步到）。
/// 调用方：RELOAD_CONFIG action、mtime 监视拍。
pub fn schedule_reload(reason: &str) {
    let Some(h) = host_opt() else { return };
    if h.reload_scheduled {
        return;
    }
    h.reload_scheduled = true;
    if std::env::var_os("NINJA_Q1_DEBUG").is_some() {
        eprintln!("ninja: 热重载排期（{reason}）");
    }
    run_reload_tick_soon();
}

fn run_reload_tick_soon() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    let Some(delegate) = app.delegate() else { return };
    // SAFETY: -self 按约定返回 retain 过的引用。
    let target: Retained<objc2::runtime::AnyObject> = unsafe { objc2::msg_send![&*delegate, self] };
    // SAFETY: scheduledTimer 平凡；selector 由 app.rs 定义。
    let timer = unsafe {
        objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
            0.05,
            &target,
            objc2::sel!(ninjaReloadTick:),
            None,
            false,
        )
    };
    std::mem::forget(timer); // 一次性；触发即失效
}

/// ninjaReloadTick: 实现体（app.rs 转发）：真正执行重载。
pub fn reload_tick() {
    let Some(h) = host_opt() else { return };
    if !h.reload_scheduled {
        return;
    }
    h.reload_scheduled = false;
    // 1. 重跑装载管线（含 ninja.toml 重读 + 新监视集快照）。
    let (cfg, info) = crate::config::load_pipeline();
    let host_cfg = crate::config::load_host_config();
    // 2. 传播：embedded 克隆新配置并下发全部 surface（字号/主题色即时
    //    生效；CONFIG_CHANGE action 携新 config 回 dispatch——指针仅
    //    回调内有效，我们不存）。旧句柄在传播后释放（embedded 已自持克隆）。
    unsafe { ghostty_app_update_config(h.app, cfg) };
    unsafe { ghostty_config_free(h.config) };
    h.config = cfg;
    h.load_info = info;
    h.host_config = host_cfg;
    h.watch = crate::config::snapshot_watch(&h.load_info.watched);
    // 3. 派生态：bg/焦点环/窗口 chrome + 菜单键位重建 + 取证 dump。
    refresh_derived(h);
    crate::app::on_config_applied();
    eprintln!(
        "ninja: 配置已重载（用户 theme={} ODP={} 监视 {} 文件）",
        h.load_info.user_theme, h.load_info.odp_applied, h.load_info.watched.len()
    );
}

/// mtime 监视拍（app.rs 的 NSTimer 调）：任一配置文件变化 → 排期重载。
pub fn watch_tick() {
    let Some(h) = host_opt() else { return };
    // 监视集每次重算（config-file 链可能变）。
    let files = {
        let mut w = crate::config::collect_ghostty_files(&crate::config::default_config_files());
        w.push(crate::config::host_config_path());
        w
    };
    if h.watch.changed(&files) {
        schedule_reload("file-watch");
    }
}

// ---------------------------------------------------------------------------
// surface 生命周期
// ---------------------------------------------------------------------------

/// 建面统一入口：nsview/userdata 交 ghostty，context（WINDOW/TAB/SPLIT）
/// + 父面走 inherited_config（继承字号/工作目录）。
pub fn attach_surface(
    view: &SurfaceHostView,
    context: ghostty_surface_context_e,
    parent: Option<&SurfaceHostView>,
) -> ghostty_surface_t {
    let host = host_opt().expect("host init");
    let mtm = MainThreadMarker::new().expect("main thread");
    let scale = view
        .window()
        .map(|w| w.backingScaleFactor())
        .or_else(|| {
            objc2_app_kit::NSScreen::mainScreen(mtm).map(|s| s.backingScaleFactor())
        })
        .unwrap_or(2.0);

    // SAFETY: 结构体按 ABI 组装；inherited_config 返回值的字符串指针仅
    // 在本函数内有效（surface_new 同步消费），macOS 本尊同款。
    unsafe {
        let mut scfg = match parent.and_then(|p| p.surface_opt()) {
            Some(p) => ghostty_surface_inherited_config(p, context),
            None => ghostty_surface_config_new(),
        };
        scfg.platform_tag = GHOSTTY_PLATFORM_MACOS;
        scfg.platform.macos.nsview = std::ptr::from_ref(view) as *mut c_void;
        scfg.userdata = std::ptr::from_ref(view) as *mut c_void;
        scfg.scale_factor = scale;
        // command/initial_input 不设（默认 shell；q0 取证机的显式
        // /bin/bash 只属 demo 模式）。
        scfg.initial_input = std::ptr::null();
        scfg.wait_after_command = false;
        scfg.context = context;
        let surface = ghostty_surface_new(host.app, &scfg);
        assert!(!surface.is_null(), "ghostty_surface_new failed");
        view.ivars().surface.set(surface);
        // SAFETY: 同类指针 retain（AppKit 引用计数安全）。
        let retained =
            Retained::retain(std::ptr::from_ref(view) as *mut SurfaceHostView).expect("view alive");
        host.live.push((surface, retained));
        view.push_size();
        surface
    }
}

/// 拆一个叶子（树操作在 pane::close_leaf；这里只做 surface 延迟 free
/// 登记）。幂等。
pub fn close_leaf_deferred(view: &SurfaceHostView) {
    if std::env::var_os("NINJA_Q1_DEBUG").is_some() {
        eprintln!("ninja: close_leaf_deferred pane={}", view.pane_id());
    }
    let Some(surface) = view.surface_opt() else {
        return;
    };
    // 先断开（view 不再收事件），再从 live 摘、进延迟队列。
    view.ivars().surface.set(std::ptr::null_mut());
    let Some(host) = host_opt() else { return };
    if let Some(pos) = host.live.iter().position(|(s, _)| *s == surface) {
        let (_, v) = host.live.remove(pos);
        host.pending_free.push((surface, v));
        schedule_free();
    }
}

/// 立即拆（不走延迟队列；防御路径用——split 失败回收）。
pub fn close_leaf_now(view: &SurfaceHostView) {
    let Some(surface) = view.surface_opt() else {
        return;
    };
    view.ivars().surface.set(std::ptr::null_mut());
    let Some(host) = host_opt() else { return };
    host.live.retain(|(s, _)| *s != surface);
    unsafe { ghostty_surface_free(surface) };
}

/// 延迟 free 的执行拍（NSTimer 一拍后；close_surface_cb 栈已退）。
fn schedule_free() {
    let Some(host) = host_opt() else { return };
    if host.free_scheduled || host.pending_free.is_empty() {
        return;
    }
    host.free_scheduled = true;
    run_free_tick_soon();
}

fn run_free_tick_soon() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    // 借 app delegate 的选择器（ninjaFreeTick:）起一次性 timer；delegate
    // 常活（进程生命期），timer 触发即失效。
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    let delegate: Option<Retained<objc2::runtime::ProtocolObject<dyn objc2_app_kit::NSApplicationDelegate>>> =
        app.delegate();
    let Some(delegate) = delegate else { return };
    // SAFETY: -self 按约定返回 retain 过的引用。
    let target: Retained<objc2::runtime::AnyObject> = unsafe { objc2::msg_send![&*delegate, self] };
    // SAFETY: scheduledTimer 平凡；selector 由 app.rs 定义。
    let timer = unsafe {
        objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
            0.05,
            &target,
            objc2::sel!(ninjaFreeTick:),
            None,
            false,
        )
    };
    std::mem::forget(timer); // 一次性；触发即失效
}

/// ninjaFreeTick: 的实现体（app.rs 转发到这里）。
pub fn free_tick() {
    let Some(host) = host_opt() else { return };
    host.free_scheduled = false;
    let pending = std::mem::take(&mut host.pending_free);
    for (surface, _view) in pending {
        // SAFETY: 此刻不在任何 ghostty 调用栈内（timer 拍）。
        unsafe { ghostty_surface_free(surface) };
    }
}

/// surface → 视图（action/close 回调翻面用）。
///
/// 经 `ghostty_surface_userdata`（建面时挂的 view 指针）取，而非 live 表：
/// surface_new 期间就会同步触发 CELL_SIZE/SIZE_LIMIT/INITIAL_SIZE 等
/// action（那时还没进 live 表）。retain 保证调用期间视图存活；已拆面
/// 的 ivars.surface 为 null，各分发分支自然 no-op。
pub fn view_of_surface(surface: ghostty_surface_t) -> Option<Retained<SurfaceHostView>> {
    // SAFETY: userdata 是建面时挂的本进程指针；retain 防已拆面在调用
    // 期间释放（pending_free 持有的 Retained 也参与保活）。
    unsafe {
        let ud = ghostty_surface_userdata(surface);
        if ud.is_null() {
            return None;
        }
        Retained::retain(ud as *mut SurfaceHostView)
    }
}

/// VIEWPORT 坐标精确区域读取（zoom dump 的 last 行取证；q0 demo 同款）。
pub fn read_text(surface: ghostty_surface_t, x0: u32, y0: u32, x1: u32, y1: u32) -> String {
    unsafe {
        let sel = ghostty_selection_s {
            top_left: ghostty_point_s {
                tag: GHOSTTY_POINT_VIEWPORT,
                coord: GHOSTTY_POINT_COORD_EXACT,
                x: x0,
                y: y0,
            },
            bottom_right: ghostty_point_s {
                tag: GHOSTTY_POINT_VIEWPORT,
                coord: GHOSTTY_POINT_COORD_EXACT,
                x: x1,
                y: y1,
            },
            rectangle: true,
        };
        let mut text = ghostty_text_s {
            tl_px_x: 0.0,
            tl_px_y: 0.0,
            offset_start: 0,
            offset_len: 0,
            text: std::ptr::null(),
            text_len: 0,
        };
        let ok = ghostty_surface_read_text(surface, sel, &mut text);
        let out = if ok && !text.text.is_null() {
            let bytes = std::slice::from_raw_parts(text.text as *const u8, text.text_len);
            String::from_utf8_lossy(bytes).to_string()
        } else {
            String::new()
        };
        ghostty_surface_free_text(surface, &mut text);
        out
    }
}

// ---------------------------------------------------------------------------
// ghostty runtime 回调
// ---------------------------------------------------------------------------

pub unsafe extern "C" fn wakeup_cb(_userdata: *mut c_void) {
    // 可能从 IO/渲染线程调用：只唤醒主 RunLoop，主线程 timer 里 app_tick
    //（CFRunLoop 唤醒线程安全）。
    objc2_core_foundation::CFRunLoop::main().unwrap().wake_up();
}

/// action 全分发（q1 的 window/tab/split 上下文回调接布局树）。
unsafe extern "C" fn action_cb(
    _app: ghostty_app_t,
    target: ghostty_target_s,
    action: ghostty_action_s,
) -> bool {
    // SAFETY: action 是联合体，按 tag 取字段；target.surface 经 userdata
    // 翻回视图（不在 live 表 = 已拆面，忽略）。
    unsafe {
        let view: Option<Retained<SurfaceHostView>> = (target.tag == GHOSTTY_TARGET_SURFACE)
            .then(|| view_of_surface(target.target.surface))
            .flatten();
        dispatch_action(view, action)
    }
}

unsafe fn dispatch_action(
    view: Option<Retained<SurfaceHostView>>,
    action: ghostty_action_s,
) -> bool {
    if std::env::var_os("NINJA_Q1_DEBUG").is_some() {
        eprintln!("ninja: action tag={} has_view={}", action.tag as i32, view.is_some());
    }
    unsafe {
        match action.tag {
            // ---- 窗/tab/split 布局树 ----
            GHOSTTY_ACTION_NEW_WINDOW => {
                let mtm = MainThreadMarker::new().unwrap();
                shell::new_window(mtm, view.as_deref()); // make_window 内 wire_window
                true
            }
            GHOSTTY_ACTION_NEW_TAB => {
                let mtm = MainThreadMarker::new().unwrap();
                shell::new_tab(mtm, view.as_deref()); // 同上
                true
            }
            GHOSTTY_ACTION_NEW_SPLIT => {
                let Some(v) = view.as_ref() else { return false };
                let Some(w) = v.window() else { return false };
                let Some(container) = pane::container_of(&w) else {
                    return false;
                };
                let Some((dir, before)) = shell::split_dir_of(action.action.new_split) else {
                    return false;
                };
                container.split_beside(v, dir, before);
                true
            }
            GHOSTTY_ACTION_CLOSE_TAB => {
                let Some(w) = window_of(&view) else { return false };
                shell::close_tab(&w, action.action.close_tab_mode);
                true
            }
            GHOSTTY_ACTION_CLOSE_WINDOW => {
                let Some(w) = window_of(&view) else { return false };
                w.close(); // 整窗关（非裸⌘W 路径）
                true
            }
            GHOSTTY_ACTION_CLOSE_ALL_WINDOWS => {
                let Some(mtm) = MainThreadMarker::new() else { return false };
                let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
                for w in app.windows().iter() {
                    if pane::container_of(&w).is_some() {
                        w.close();
                    }
                }
                true
            }
            GHOSTTY_ACTION_GOTO_TAB => {
                let Some(w) = window_of(&view) else { return false };
                shell::goto_tab(&w, action.action.goto_tab);
                true
            }
            GHOSTTY_ACTION_MOVE_TAB => {
                let Some(w) = window_of(&view) else { return false };
                shell::move_tab(&w, action.action.move_tab.amount);
                true
            }
            GHOSTTY_ACTION_GOTO_WINDOW => {
                let Some(mtm) = MainThreadMarker::new() else { return false };
                shell::goto_window(mtm, action.action.goto_window);
                true
            }
            GHOSTTY_ACTION_GOTO_SPLIT => {
                let Some(v) = view.as_ref() else { return false };
                let Some(w) = v.window() else { return false };
                let Some(container) = pane::container_of(&w) else { return false };
                match action.action.goto_split {
                    GHOSTTY_GOTO_SPLIT_PREVIOUS => container.cycle_focus(-1),
                    GHOSTTY_GOTO_SPLIT_NEXT => container.cycle_focus(1),
                    GHOSTTY_GOTO_SPLIT_UP => container.focus_dir(crate::pane::Dir::Vertical, false),
                    GHOSTTY_GOTO_SPLIT_DOWN => container.focus_dir(crate::pane::Dir::Vertical, true),
                    GHOSTTY_GOTO_SPLIT_LEFT => {
                        container.focus_dir(crate::pane::Dir::Horizontal, false)
                    }
                    GHOSTTY_GOTO_SPLIT_RIGHT => {
                        container.focus_dir(crate::pane::Dir::Horizontal, true)
                    }
                    _ => {}
                }
                true
            }
            GHOSTTY_ACTION_RESIZE_SPLIT => {
                let Some(v) = view.as_ref() else { return false };
                let Some(w) = v.window() else { return false };
                let Some(container) = pane::container_of(&w) else { return false };
                let rs = action.action.resize_split;
                let dir = match rs.direction {
                    GHOSTTY_RESIZE_SPLIT_UP => pane::ResizeDir::Up,
                    GHOSTTY_RESIZE_SPLIT_DOWN => pane::ResizeDir::Down,
                    GHOSTTY_RESIZE_SPLIT_LEFT => pane::ResizeDir::Left,
                    GHOSTTY_RESIZE_SPLIT_RIGHT => pane::ResizeDir::Right,
                    _ => return false,
                };
                container.resize_split(dir, f64::from(rs.amount));
                true
            }
            GHOSTTY_ACTION_EQUALIZE_SPLITS => {
                let Some(v) = view.as_ref() else { return false };
                let Some(w) = v.window() else { return false };
                let Some(container) = pane::container_of(&w) else { return false };
                container.equalize();
                true
            }
            GHOSTTY_ACTION_TOGGLE_SPLIT_ZOOM => {
                let Some(v) = view.as_ref() else { return false };
                let Some(w) = v.window() else { return false };
                let Some(container) = pane::container_of(&w) else { return false };
                container.toggle_zoom(); // ⌘⇧Enter 三态状态机（v1 同款）
                true
            }
            GHOSTTY_ACTION_TOGGLE_MAXIMIZE => {
                let Some(w) = window_of(&view) else { return false };
                w.zoom(None);
                true
            }

            // ---- 窗口几何/标题/PWD ----
            GHOSTTY_ACTION_SIZE_LIMIT => {
                let Some(view) = &view else { return false };
                let s = action.action.size_limit;
                view.set_size_limit(
                    Some((s.min_width, s.min_height)),
                    Some((s.max_width, s.max_height)),
                );
                true
            }
            GHOSTTY_ACTION_INITIAL_SIZE => {
                let Some(view) = &view else { return false };
                let s = action.action.initial_size;
                view.ivars().initial_pt.set(Some((s.width, s.height)));
                // 只对未显示窗口生效（建窗流程 make_window 消费；运行中
                // 的窗口不被字体/配置变化重置——macOS 本尊同款只管新窗）。
                if let Some(w) = view.window()
                    && !w.isVisible()
                {
                    w.setContentSize(objc2_foundation::NSSize::new(
                        s.width as f64,
                        s.height as f64,
                    ));
                }
                true
            }
            GHOSTTY_ACTION_SET_TITLE => {
                let title = CStr::from_ptr(action.action.set_title.title)
                    .to_string_lossy()
                    .to_string();
                if let Some(w) = window_of(&view) {
                    w.setTitle(&NSString::from_str(&title));
                }
                true
            }
            GHOSTTY_ACTION_SET_TAB_TITLE => {
                // 原生 tab 的标题 = 各自 window 的 title。
                let title = CStr::from_ptr(action.action.set_tab_title.title)
                    .to_string_lossy()
                    .to_string();
                if let Some(w) = window_of(&view) {
                    w.setTitle(&NSString::from_str(&title));
                }
                true
            }
            GHOSTTY_ACTION_PWD => {
                let pwd = CStr::from_ptr(action.action.pwd.pwd)
                    .to_string_lossy()
                    .to_string();
                if let Some(v) = &view {
                    *v.ivars().pwd.borrow_mut() = Some(pwd);
                }
                true
            }

            GHOSTTY_ACTION_CELL_SIZE => {
                let Some(view) = &view else { return false };
                let c = action.action.cell_size;
                view.ivars().cell_px.set(Some((c.width, c.height)));
                true
            }

            // ---- 配置系统（q2）----
            // ⌘⇧,（ghostty 默认 reload_config 绑定）/条件态变化（soft）：
            // 下一拍重跑装载管线（见 schedule_reload——不在 ghostty 调用栈内
            // 重入 update_config）。
            GHOSTTY_ACTION_RELOAD_CONFIG => {
                let soft = action.action.reload_config.soft;
                schedule_reload(if soft { "reload_config(soft)" } else { "reload_config" });
                true
            }
            // CONFIG_CHANGE：update_config 传播后携新 config 回调（指针仅
            // 回调内有效）。派生态刷新由 reload_tick 统一做（swap 后）；
            // 这里只确认接收。
            GHOSTTY_ACTION_CONFIG_CHANGE => {
                if std::env::var_os("NINJA_Q1_DEBUG").is_some() {
                    eprintln!("ninja: CONFIG_CHANGE（新配置已传播全部 surface）");
                }
                true
            }
            // ninja 特有动作（插件面板）：宿主层绑 ⌘,，用户可经 ghostty
            // keybind 统一重绑（认领空闲动作，见 crate::config 模块头）。
            // q2 面板 UI 是 q3 交付（不做插件面板/主题切换 UI）：动作接收
            // 如实记日志（取证可断言），q3 接真面板。
            GHOSTTY_ACTION_TOGGLE_VISIBILITY => {
                eprintln!(
                    "ninja: toggle_visibility 收到（插件面板是 q3 交付，此处仅认领记录）"
                );
                true
            }
            // ghostty 默认 ⌘,=open_config 被宿主层重绑给面板；若用户又
            // 改绑到 open_config，这里如实接收（q2 不内置编辑器，只提示）。
            GHOSTTY_ACTION_OPEN_CONFIG => {
                eprintln!(
                    "ninja: open_config：编辑 ghostty 配置文件（路径见启动日志/用户配置目录）"
                );
                true
            }
            // ⌘Q（ghostty 默认 quit 绑定）与菜单 terminate: 同途。
            GHOSTTY_ACTION_QUIT => {
                let Some(mtm) = MainThreadMarker::new() else { return false };
                let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
                // 标准 NSApp terminate（applicationShouldTerminate 走默认
                // 同意，run 返回后 main 统一收尾；objc2 生成为安全方法）。
                app.terminate(None);
                true
            }

            // ---- 渲染/其余（q1 不需要；q3 的面）----
            GHOSTTY_ACTION_RENDER => {
                // 嵌入 apprt 无 must_draw_from_app_thread（渲染线程自画），
                // 该 action 不应到；防御性补一帧无害。
                if let Some(v) = &view
                    && let Some(s) = v.surface_opt()
                {
                    ghostty_surface_draw(s);
                }
                true
            }
            _ => false, // MOUSE_OVER_LINK/MOUSE_SHAPE/OPEN_URL… 留 q3
        }
    }
}

fn window_of(view: &Option<Retained<SurfaceHostView>>) -> Option<Retained<objc2_app_kit::NSWindow>> {
    view.as_ref().and_then(|v| v.window())
}

unsafe extern "C" fn read_clipboard_cb(
    _userdata: *mut c_void,
    _clipboard: ghostty_clipboard_e,
    request: *mut c_void,
) -> bool {
    // 主线程（app_tick 处理中）同步回粘贴板内容（q0 demo 同款）。
    let Some(view) = current_surface_view() else { return false };
    let Some(surface) = view.surface_opt() else {
        return false;
    };
    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        match pb.stringForType(NSPasteboardTypeString) {
            None => {
                ghostty_surface_complete_clipboard_request(
                    surface,
                    c"".as_ptr(),
                    request,
                    false,
                )
            }
            Some(s) => {
                let c = CString::new(s.to_string()).unwrap_or_default();
                ghostty_surface_complete_clipboard_request(surface, c.as_ptr(), request, true);
            }
        }
    }
    true
}

/// 剪贴板回调没有 surface 上下文（userdata 是 app 级）——取当前 key
/// window 的焦点面（q0 demo 单面直取的推广；q1 主用法成立）。
pub fn current_surface_view() -> Option<Retained<SurfaceHostView>> {
    let mtm = MainThreadMarker::new()?;
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    let w = app.keyWindow().or_else(|| app.mainWindow());
    if let Some(w) = w
        && let Some(container) = pane::container_of(&w)
        && let Some(leaf) = container.focused_leaf().or_else(|| container.leaves().first().cloned())
    {
        return Some(leaf);
    }
    // 兑底：无 key/main 窗口（后台启动未成 key 等）时取任一活面——
    // 菜单键等价物经 performKeyEquivalent 到达时不依赖窗口 key 态，
    // 单窗场景下语义等价（焦点面 = 唯一面）。
    let h = host_opt()?;
    for (_, view) in h.live.iter() {
        if view.window().is_some() {
            return Some(view.clone());
        }
    }
    None
}

unsafe extern "C" fn confirm_read_clipboard_cb(
    _userdata: *mut c_void,
    _data: *const c_char,
    request: *mut c_void,
    _kind: ghostty_clipboard_request_e,
) {
    // q1 无确认 UI：以空内容放行，避免粘贴挂起（q0 demo 同款）。
    let Some(view) = current_surface_view() else { return };
    if let Some(surface) = view.surface_opt() {
        unsafe {
            ghostty_surface_complete_clipboard_request(surface, c"".as_ptr(), request, false);
        }
    }
}

unsafe extern "C" fn write_clipboard_cb(
    _userdata: *mut c_void,
    clipboard: ghostty_clipboard_e,
    contents: *const ghostty_clipboard_content_s,
    count: usize,
    _confirmed: bool,
) {
    unsafe {
        if clipboard != GHOSTTY_CLIPBOARD_STANDARD || count == 0 || contents.is_null() {
            return;
        }
        let data = std::slice::from_raw_parts(
            (*contents).data as *const u8,
            libc::strlen((*contents).data),
        );
        let Ok(s) = std::str::from_utf8(data) else { return };
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        pb.setString_forType(&NSString::from_str(s), NSPasteboardTypeString);
    }
}

/// ghostty close_surface（⌘W 默认绑定）/EOF 的宿主入口：
/// process_alive 语义不影响决策（v1 ⌘W/EOF 同途：多 pane 拆焦点叶、
/// 单 pane performClose；确认对话框不是 v1 语义）。
pub unsafe extern "C" fn close_surface_cb(userdata: *mut c_void, process_alive: bool) {
    // userdata = 建 surface 时挂的 view 指针。幂等：ivars.surface 已被
    // close_leaf_deferred 清空的面（重复 EOF/⌘W）直接忽略。
    let view: &SurfaceHostView = unsafe { &*(userdata as *const SurfaceHostView) };
    if std::env::var_os("NINJA_Q1_DEBUG").is_some() {
        eprintln!("ninja: close_surface_cb alive={} process_alive={process_alive}", view.surface_opt().is_some());
    }
    if view.surface_opt().is_none() {
        return;
    }
    shell::handle_surface_close(view, process_alive);
}

/// 组运行时回调表（main 里 ghostty_app_new 用）。
pub fn runtime_config() -> ghostty_runtime_config_s {
    ghostty_runtime_config_s {
        userdata: std::ptr::null_mut(),
        supports_selection_clipboard: false,
        wakeup_cb: Some(wakeup_cb),
        action_cb: Some(action_cb),
        read_clipboard_cb: Some(read_clipboard_cb),
        confirm_read_clipboard_cb: Some(confirm_read_clipboard_cb),
        write_clipboard_cb: Some(write_clipboard_cb),
        close_surface_cb: Some(close_surface_cb),
    }
}

/// 主 RunLoop tick（16ms timer）：app_tick 驱动 action/邮箱（渲染线程
/// 自画，不在此 draw；q0 demo 的 timer+draw 是 demo 模式）。
pub unsafe extern "C-unwind" fn tick_cb(
    _timer: *mut objc2_core_foundation::CFRunLoopTimer,
    _info: *mut c_void,
) {
    let Some(host) = host_opt() else { return };
    unsafe { ghostty_app_tick(host.app) };
}

/// 短句柄访问（app 级操作：app_set_focus 等）。
pub fn with_app(f: impl FnOnce(ghostty_app_t)) {
    if let Some(h) = host_opt() {
        f(h.app);
    }
}
