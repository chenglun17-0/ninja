//! q1 AppKit 壳引导：NSApplication（Regular）+ AppDelegate（窗口注册表、
//! 裸⌘W 决策、windowWillClose 收尾）+ 菜单 + 取证钩子。
//! 移植自 v1 crates/ninja/src/app.rs（p2/X3 资产，旧树 ninja-embed 同款
//! 重接，按当前 PLAN 重写进本 crate）：
//!
//! - 菜单：App（Plugins… ⌘, 禁用占位（q3）/ Quit ⌘Q）、File（⌘N/⌘T/
//!   Close ⌘W）、Panes（⌘D/⌘⇧D/⌘⇧W/⌘⇧Enter/焦点导航 ⌥⌘方向键/⌘[⌘]）、
//!   Window（⌘⇧[/⌘⇧]）、Edit（⌘C/⌘V/⌘A）。键位与 v1 默认表一致
//!   （ghostty 默认绑定同键位走 surface_key 时由 action_cb 汇到同一批
//!   操作；⌘W 菜单=performClose 路径 vs ghostty close_surface 绑定路径
//!   双通道同语义；差异项如 close_pane=⌘⇧W 留 q2 配置系统统一）。
//! - 窗口注册表 + releasedWhenClosed(false) + 延迟 prune（v1 SIGSEGV
//!   教训）。
//! - 取证钩子：NINJA_P2_SELFTEST（tab,split,win,close,closepane,
//!   closebinding）+ NINJA_ZOOM_FILE/NINJA_ZOOM_DUMP（zoom dump JSON），
//!   供独立验证免 CGEvent 驱动。
//! - ⌘, 只保留键位空位认领（禁用菜单项），不建面板 UI（q1 不做插件/
//!   Agent/预览/面板）。

#![allow(non_snake_case)] // ObjC selector 方法名
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicPtr, Ordering};

use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSMenu, NSMenuItem,
    NSEventModifierFlags, NSWindow, NSWindowDelegate,
};
use objc2_foundation::{NSNotification, NSObject, NSString};

use crate::host;
use crate::shell;

pub struct Ivars {
    /// 门禁取证钩子的步骤序列（NINJA_P2_SELFTEST，启动后延时执行）。
    selftest: RefCell<Option<String>>,
    /// zoom 取证钩子：NINJA_ZOOM_FILE 轮询的文件路径。
    zoom_file: RefCell<Option<String>>,
    /// zoom 钩子上次已应用的内容（去抖）。
    zoom_last: RefCell<Option<String>>,
    /// 本壳持有的窗口强引用（v1 红线：close 期间必须有人持有，close
    /// 完成后延迟一拍释放）。
    pub windows: RefCell<Vec<Retained<NSWindow>>>,
    closing: RefCell<Vec<*const NSWindow>>,
    prune_scheduled: Cell<bool>,
}

define_class!(
    // SAFETY:
    // - NSObject 子类化无要求；不实现 Drop。
    // - 全部状态只在主线程访问（MainThreadOnly）。
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct AppDelegate;

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn applicationDidFinishLaunching(&self, _notification: &NSNotification) {
            let mtm = MainThreadMarker::new().expect("delegate on main thread");
            let app = NSApplication::sharedApplication(mtm);

            // 首窗（context=WINDOW；INITIAL_SIZE 定尺寸见 make_window）。
            let window = shell::make_window(mtm, None, ghostty_sys::GHOSTTY_SURFACE_CONTEXT_WINDOW);
            // E2E 虚拟屏（NINJA_E2E_SCREEN）时 make_window 已定窗，不叠 center。
            if std::env::var_os("NINJA_E2E_SCREEN").is_none() {
                window.center();
            }
            window.makeKeyAndOrderFront(None);
            // 首叶夺焦（窗口 key 后把 first responder 落在终端面上）。
            if let Some(container) = crate::pane::container_of(&window)
                && let Some(first) = container.leaves().first()
            {
                window.makeFirstResponder(Some(crate::surface::as_responder(first)));
            }

            // 拉前台（deprecated 但行为稳定，v1 同款）。
            #[allow(deprecated)]
            {
                app.activateIgnoringOtherApps(true);
            }

            // 取证钩子：NINJA_P2_SELFTEST=tab,split,win,close,closepane,
            // closebinding——runloop 起转、首窗 key 后按序触发（免
            // CGEvent 抖动）。close=菜单 performClose 路径（裸⌘W 决策）；
            // closebinding=ghostty close_surface 绑定路径（surface_
            // binding_action 直驱）。未知项忽略。
            if let Ok(seq) = std::env::var("NINJA_P2_SELFTEST") {
                self.ivars().selftest.replace(Some(seq));
                // SAFETY: ObjC 的 -self 按约定返回 retain 过的自身引用。
                let target: Retained<objc2::runtime::AnyObject> = unsafe { msg_send![self, self] };
                // SAFETY: scheduledTimer 平凡。
                let timer = unsafe {
                    objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                        0.8,
                        &target,
                        objc2::sel!(ninjaSelfTestTick:),
                        None,
                        false,
                    )
                };
                std::mem::forget(timer); // 一次性；进程生命期内 self 常活
            }

            // zoom 取证钩子：NINJA_ZOOM_FILE=<path> 每 0.2s 读文件，内容
            // 变化才动作——"toggle"/"zoom"/"unzoom"/"split" 走 key window
            // 容器的 zoom 路径（⌘⇧Enter 同途）；"dump" 把 zoom 态 JSON 写
            // 到 NINJA_ZOOM_DUMP（布局/隐藏/网格尺寸/内容取证）。E2E 用。
            if let Ok(f) = std::env::var("NINJA_ZOOM_FILE") {
                self.ivars().zoom_file.replace(Some(f));
                // SAFETY: 同上（-self 返回 retain 过的引用）。
                let target: Retained<objc2::runtime::AnyObject> = unsafe { msg_send![self, self] };
                // SAFETY: scheduledTimer 平凡。
                let timer = unsafe {
                    objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                        0.2,
                        &target,
                        objc2::sel!(ninjaZoomTick:),
                        None,
                        true,
                    )
                };
                std::mem::forget(timer); // 进程生命期常驻
            }
        }

        // 多窗口：最后一个窗口关闭才退出（⌘Q 随时退）。
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn applicationShouldTerminateAfterLastWindowClosed(
            &self,
            _sender: &NSApplication,
        ) -> bool {
            if std::env::var_os("NINJA_Q1_DEBUG").is_some() {
                eprintln!("ninja: terminateAfterLastWindowClosed? -> true");
            }
            true
        }

        #[unsafe(method(applicationDidBecomeActive:))]
        fn applicationDidBecomeActive(&self, _notification: &NSNotification) {
            host::with_app(|app| unsafe { ghostty_sys::ghostty_app_set_focus(app, true) });
        }

        #[unsafe(method(applicationDidResignActive:))]
        fn applicationDidResignActive(&self, _notification: &NSNotification) {
            host::with_app(|app| unsafe { ghostty_sys::ghostty_app_set_focus(app, false) });
        }
    }

    unsafe impl NSWindowDelegate for AppDelegate {
        /// D-A：⌘W 只关「当前面」（决策群在 shell.rs，带单测）。裸 ⌘W
        /// （菜单 Close=performClose: / 系统 Close Tab）多 pane 窗只关
        /// 焦点 pane、拦掉整窗 close；单 pane 放行原生语义。非 ⌘W 路径
        /// （红绿灯、⇧⌘W/⌥⌘W、EOF、selftest）不受影响。须在
        /// windowWillClose 之前（performClose 先问 shouldClose）。
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, sender: &NSWindow) -> bool {
            let ok = shell::window_should_close(sender);
            if std::env::var_os("NINJA_Q1_DEBUG").is_some() {
                eprintln!("ninja: windowShouldClose -> {ok}");
            }
            ok
        }

        // 关窗前先收尾该窗全部 pane（surface 延迟 free），防止 ghostty
        // 回调在窗口拆一半时进 view。
        #[unsafe(method(windowWillClose:))]
        fn windowWillClose(&self, notification: &NSNotification) {
            if std::env::var_os("NINJA_Q1_DEBUG").is_some() {
                eprintln!("ninja: windowWillClose");
            }
            // SAFETY: object() 返回通知对象（此处恒为 NSWindow）。
            let window: Option<&NSWindow> = unsafe { msg_send![notification, object] };
            if let Some(w) = window {
                if let Some(content) = w.contentView() {
                    shell::window_closed(&content);
                }
                // close 完成后延迟释放登记的强引用（现在放会把窗口拆在
                // 它自己的 close 调用栈里）。
                self.schedule_release(w);
            }
        }

        // tab 组内的窗口切 key：同步 app 焦点给 ghostty（DidResign 先于
        // DidBecome，终态正确）。
        #[unsafe(method(windowDidBecomeKey:))]
        fn windowDidBecomeKey(&self, _notification: &NSNotification) {
            host::with_app(|app| unsafe { ghostty_sys::ghostty_app_set_focus(app, true) });
        }

        #[unsafe(method(windowDidResignKey:))]
        fn windowDidResignKey(&self, _notification: &NSNotification) {
            host::with_app(|app| unsafe { ghostty_sys::ghostty_app_set_focus(app, false) });
        }
    }

    impl AppDelegate {
        /// 延迟释放已关窗口的强引用（close 早已返回，此时释放安全）。
        #[unsafe(method(ninjaPruneClosedWindows:))]
        fn ninja_prune_closed_windows(&self, _timer: Option<&objc2::runtime::AnyObject>) {
            self.ivars().prune_scheduled.set(false);
            let closing = std::mem::take(&mut *self.ivars().closing.borrow_mut());
            self.ivars().windows.borrow_mut().retain(|w| {
                let ptr = &**w as *const NSWindow;
                !closing.contains(&ptr)
            });
        }

        /// 延迟 free 的执行拍（host::schedule_free 起的 timer 落这里）。
        #[unsafe(method(ninjaFreeTick:))]
        fn ninja_free_tick(&self, _timer: Option<&objc2::runtime::AnyObject>) {
            host::free_tick();
        }

        /// 门禁钩子：延时执行 selftest 序列（首窗已 key、runloop 已起转）。
        #[unsafe(method(ninjaSelfTestTick:))]
        fn ninja_selftest_tick(&self, _timer: Option<&objc2::runtime::AnyObject>) {
            let Some(seq) = self.ivars().selftest.take() else {
                return;
            };
            let Some(mtm) = MainThreadMarker::new() else { return };
            let app = NSApplication::sharedApplication(mtm);
            let key_window = || {
                app.keyWindow()
                    .or_else(|| app.mainWindow())
                    .or_else(|| self.ivars().windows.borrow().first().cloned())
            };
            let key_container = || -> Option<Retained<objc2_app_kit::NSWindow>> {
                let w = key_window()?;
                crate::pane::container_of(&w).map(|_| w)
            };
            for step in seq.split(',').map(str::trim) {
                match step {
                    "tab" => {
                        let parent = key_container().and_then(|w| {
                            crate::pane::container_of(&w)
                                .and_then(|c| c.focused_leaf().or_else(|| c.leaves().first().cloned()))
                        });
                        shell::new_tab(mtm, parent.as_deref()); // make_window 内 wire_window
                    }
                    "split" => {
                        if let Some(w) = key_container()
                            && let Some(container) = crate::pane::container_of(&w)
                        {
                            container.split_focused(crate::pane::Dir::Horizontal, false);
                        }
                    }
                    "win" => {
                        let parent = key_container().and_then(|w| {
                            crate::pane::container_of(&w)
                                .and_then(|c| c.focused_leaf().or_else(|| c.leaves().first().cloned()))
                        });
                        shell::new_window(mtm, parent.as_deref()); // 同上
                    }
                    // 菜单 ⌘W 同途：performClose（windowShouldClose 的
                    // 裸⌘W 决策——selftest 无键事件 → currentEvent 非
                    // keyDown → 放行整窗/tab 关）。
                    "close" => {
                        if let Some(w) = key_window() {
                            w.performClose(None);
                        }
                    }
                    "closepane" => {
                        if let Some(w) = key_container()
                            && let Some(container) = crate::pane::container_of(&w)
                            && let Some(f) = container.focused_leaf()
                        {
                            container.close_leaf(&f);
                        }
                    }
                    // ghostty 绑定路径 ⌘W：surface_binding_action 直驱
                    // close_surface action → close_surface_cb（与菜单
                    // performClose 双路径取证）。
                    // ghostty ⌘W 绑定路径（close_surface action →
                    // close_surface_cb）：request_close = 文档语义的「与
                    // close_surface 键位绑定相同的正常触发流程」
                    //（embedded.zig L1922-1926），E2E 直驱免 CGEvent。
                    "closebinding" => {
                        let f = (|| {
                            let w = key_container()?;
                            let container = crate::pane::container_of(&w)?;
                            container
                                .focused_leaf()
                                .or_else(|| container.leaves().first().cloned())
                                .and_then(|f| f.surface_opt())
                        })();
                        let Some(s) = f else {
                            eprintln!("ninja: NINJA_P2_SELFTEST closebinding 无可用 surface");
                            continue;
                        };
                        // SAFETY: 公开 C API；surface 句柄存活。
                        unsafe { ghostty_sys::ghostty_surface_request_close(s) };
                    }
                    other => eprintln!("ninja: NINJA_P2_SELFTEST 未知步骤 {other:?}"),
                }
            }
        }

        /// zoom 钩子拍（v1 X3 同款）。
        #[unsafe(method(ninjaZoomTick:))]
        fn ninja_zoom_tick(&self, _timer: Option<&objc2::runtime::AnyObject>) {
            let Some(path) = self.ivars().zoom_file.borrow().clone() else {
                return;
            };
            let Ok(raw) = std::fs::read_to_string(&path) else {
                return;
            };
            let content = raw.trim().to_string();
            if content.is_empty()
                || self.ivars().zoom_last.borrow().as_deref() == Some(content.as_str())
            {
                return; // 去抖：同一内容只动作一次
            }
            *self.ivars().zoom_last.borrow_mut() = Some(content.clone());
            let Some(mtm) = MainThreadMarker::new() else {
                return;
            };
            let app = NSApplication::sharedApplication(mtm);
            // key/main 窗口在后台/争激活时会短暂为 nil（v1 实测），
            // 退化顺序：keyWindow → mainWindow → 已登记窗口表。
            let window = app
                .keyWindow()
                .or_else(|| app.mainWindow())
                .or_else(|| self.ivars().windows.borrow().first().cloned());
            let Some(w) = window else {
                eprintln!("ninja: NINJA_ZOOM_FILE 无窗口可 zoom");
                return;
            };
            let Some(container) = crate::pane::container_of(&w) else {
                eprintln!("ninja: NINJA_ZOOM_FILE 窗口内容不是 PaneContainer");
                return;
            };
            match content.as_str() {
                "toggle" => container.toggle_zoom(),
                "zoom" => container.zoom_focused(),
                "unzoom" | "restore" => container.unzoom(),
                // split：E2E 布置双 pane（同 v1 钩子）。
                "split" => container.split_focused(crate::pane::Dir::Horizontal, false),
                // bindact:<name>：焦点面直驱 ghostty 绑定动作（与键位触发
                // 同一条 action 路径；E2E 驱动 TOGGLE_SPLIT_ZOOM 等）。
                s if s.starts_with("bindact:") => {
                    let name = &s["bindact:".len()..];
                    if let Some(f) = container
                        .focused_leaf()
                        .or_else(|| container.leaves().first().cloned())
                        && let Some(surf) = f.surface_opt()
                    {
                        // SAFETY: 公开 C API；surface 句柄存活。
                        unsafe {
                            ghostty_sys::ghostty_surface_binding_action(
                                surf,
                                name.as_ptr() as *const std::ffi::c_char,
                                name.len(),
                            )
                        };
                    }
                }
                // "dump"/"dump2"/… 前缀都算 dump（轮询递增后缀绕去抖）。
                s if s.starts_with("dump") => {
                    let Some(out) = std::env::var_os("NINJA_ZOOM_DUMP") else {
                        eprintln!("ninja: NINJA_ZOOM_FILE=dump 需要 NINJA_ZOOM_DUMP 输出路径");
                        return;
                    };
                    let _ = std::fs::write(&out, container.zoom_state_json());
                }
                other => eprintln!(
                    "ninja: NINJA_ZOOM_FILE 动作 {other:?} 无效（toggle | zoom | unzoom | split | dump）"
                ),
            }
        }

        /// ⌘N：新窗口（nil target 动作最终落到 delegate）。
        #[unsafe(method(ninjaNewWindow:))]
        fn ninja_new_window(&self, _sender: Option<&objc2::runtime::AnyObject>) {
            let Some(mtm) = MainThreadMarker::new() else { return };
            let app = NSApplication::sharedApplication(mtm);
            let parent = app
                .keyWindow()
                .and_then(|w| crate::pane::container_of(&w))
                .and_then(|c| c.focused_leaf().or_else(|| c.leaves().first().cloned()));
            shell::new_window(mtm, parent.as_deref()); // make_window 内 wire_window
        }

        /// ⌘T / 系统标签栏 +：新标签（NSResponder 动作 newWindowForTab:）。
        #[unsafe(method(newWindowForTab:))]
        fn new_window_for_tab(&self, _sender: Option<&objc2::runtime::AnyObject>) {
            let Some(mtm) = MainThreadMarker::new() else { return };
            shell::new_tab(mtm, None); // make_window 内 wire_window
        }
    }

    unsafe impl NSObjectProtocol for AppDelegate {}
);

// ---------------------------------------------------------------------------
// 菜单（v1 默认键位；键位可配置系统是 q2）
// ---------------------------------------------------------------------------

struct ItemSpec {
    title: &'static str,
    selector: &'static str,
    key: &'static str,
    cmd: bool,
    shift: bool,
    alt: bool,
    /// false = 禁用占位（键位空位认领，不建 UI）。
    disabled: bool,
}

const APP_ITEMS: &[ItemSpec] = &[
    // ⌘, 只保留键位空位认领（插件面板 UI 是 q3；q2 会把它映射到 ghostty
    // 封闭动作集的 toggle_visibility 空位）。禁用项不派发动作。
    ItemSpec { title: "Plugins…", selector: "ninjaPlugins:", key: ",", cmd: true, shift: false, alt: false, disabled: true },
    ItemSpec { title: "Quit ninja", selector: "terminate:", key: "q", cmd: true, shift: false, alt: false, disabled: false },
];

const FILE_ITEMS: &[ItemSpec] = &[
    ItemSpec { title: "New Window", selector: "ninjaNewWindow:", key: "n", cmd: true, shift: false, alt: false, disabled: false },
    ItemSpec { title: "New Tab", selector: "newWindowForTab:", key: "t", cmd: true, shift: false, alt: false, disabled: false },
    ItemSpec { title: "Close", selector: "performClose:", key: "w", cmd: true, shift: false, alt: false, disabled: false },
];

const PANE_ITEMS: &[ItemSpec] = &[
    ItemSpec { title: "Split Right", selector: "ninjaSplitRight:", key: "d", cmd: true, shift: false, alt: false, disabled: false },
    ItemSpec { title: "Split Down", selector: "ninjaSplitDown:", key: "d", cmd: true, shift: true, alt: false, disabled: false },
    ItemSpec { title: "Close Pane", selector: "ninjaClosePane:", key: "w", cmd: true, shift: true, alt: false, disabled: false },
    ItemSpec { title: "Zoom Pane", selector: "ninjaToggleZoom:", key: "\r", cmd: true, shift: true, alt: false, disabled: false },
    ItemSpec { title: "Focus Pane Left", selector: "ninjaFocusLeft:", key: "\u{F702}", cmd: true, shift: false, alt: true, disabled: false },
    ItemSpec { title: "Focus Pane Right", selector: "ninjaFocusRight:", key: "\u{F703}", cmd: true, shift: false, alt: true, disabled: false },
    ItemSpec { title: "Focus Pane Up", selector: "ninjaFocusUp:", key: "\u{F700}", cmd: true, shift: false, alt: true, disabled: false },
    ItemSpec { title: "Focus Pane Down", selector: "ninjaFocusDown:", key: "\u{F701}", cmd: true, shift: false, alt: true, disabled: false },
    ItemSpec { title: "Previous Pane", selector: "ninjaPrevPane:", key: "[", cmd: true, shift: false, alt: false, disabled: false },
    ItemSpec { title: "Next Pane", selector: "ninjaNextPane:", key: "]", cmd: true, shift: false, alt: false, disabled: false },
];

const EDIT_ITEMS: &[ItemSpec] = &[
    ItemSpec { title: "Copy", selector: "copy:", key: "c", cmd: true, shift: false, alt: false, disabled: false },
    ItemSpec { title: "Paste", selector: "paste:", key: "v", cmd: true, shift: false, alt: false, disabled: false },
    ItemSpec { title: "Select All", selector: "selectAll:", key: "a", cmd: true, shift: false, alt: false, disabled: false },
];

fn add_submenu(mtm: MainThreadMarker, main_menu: &NSMenu, title: &str) -> Retained<NSMenu> {
    let menu = NSMenu::new(mtm);
    let item = NSMenuItem::new(mtm);
    if title != "__app__" {
        item.setTitle(&NSString::from_str(title));
    }
    item.setSubmenu(Some(&menu));
    main_menu.addItem(&item);
    menu
}

fn add_item(mtm: MainThreadMarker, menu: &NSMenu, spec: &ItemSpec) {
    let sel = std::ffi::CString::new(spec.selector).expect("selector cstr");
    // SAFETY: NSMenuItem 指定初始化器；参数平凡。
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(spec.title),
            Some(objc2::runtime::Sel::register(sel.as_c_str())),
            &NSString::from_str(spec.key),
        )
    };
    let mut flags = NSEventModifierFlags(0);
    if spec.cmd {
        flags |= NSEventModifierFlags::Command;
    }
    if spec.shift {
        flags |= NSEventModifierFlags::Shift;
    }
    if spec.alt {
        flags |= NSEventModifierFlags::Option;
    }
    item.setKeyEquivalentModifierMask(flags);
    if spec.disabled {
        item.setEnabled(false);
    }
    menu.addItem(&item);
}

/// 建菜单栏：App / File / Panes / Window / Edit（v1 布局）。
fn build_menu(mtm: MainThreadMarker, app: &NSApplication) {
    let main_menu = NSMenu::new(mtm);

    let app_menu = add_submenu(mtm, &main_menu, "__app__");
    // 面板项与退出项之间加分隔线（macOS 菜单惯例）。
    for (i, spec) in APP_ITEMS.iter().enumerate() {
        if i == APP_ITEMS.len() - 1 {
            app_menu.addItem(&NSMenuItem::separatorItem(mtm));
        }
        add_item(mtm, &app_menu, spec);
    }

    let file_menu = add_submenu(mtm, &main_menu, "File");
    for spec in FILE_ITEMS {
        add_item(mtm, &file_menu, spec);
    }

    let pane_menu = add_submenu(mtm, &main_menu, "Panes");
    for (i, spec) in PANE_ITEMS.iter().enumerate() {
        // 分隔：Zoom Pane（布局态）之后、导航组尾之后（v1 同款）。
        if i == 4 || i == 8 {
            pane_menu.addItem(&NSMenuItem::separatorItem(mtm));
        }
        add_item(mtm, &pane_menu, spec);
    }

    let window_menu = add_submenu(mtm, &main_menu, "Window");
    add_item(
        mtm,
        &window_menu,
        &ItemSpec { title: "Next Tab", selector: "selectNextTab:", key: "]", cmd: true, shift: true, alt: false, disabled: false },
    );
    add_item(
        mtm,
        &window_menu,
        &ItemSpec { title: "Previous Tab", selector: "selectPreviousTab:", key: "[", cmd: true, shift: true, alt: false, disabled: false },
    );

    let edit_menu = add_submenu(mtm, &main_menu, "Edit");
    for spec in EDIT_ITEMS {
        add_item(mtm, &edit_menu, spec);
    }

    app.setMainMenu(Some(&main_menu));
}

// ---------------------------------------------------------------------------
// delegate 单例（host.rs 的 action 分发需要 register_window）
// ---------------------------------------------------------------------------

static DELEGATE: AtomicPtr<AppDelegate> = AtomicPtr::new(std::ptr::null_mut());

fn delegate() -> Option<&'static AppDelegate> {
    let p = DELEGATE.load(Ordering::Acquire);
    (!p.is_null()).then(|| unsafe { &*p })
}

/// 挂窗 delegate + 登记强引用（make_window 统一入口）。
pub fn wire_window(w: &NSWindow) {
    if let Some(d) = delegate() {
        // SAFETY: 协议对象包装（NSWindowDelegate 弱引用 delegate）。
        w.setDelegate(Some(ProtocolObject::from_ref(d)));
        let r = unsafe {
            Retained::retain(std::ptr::from_ref(w) as *mut NSWindow).unwrap()
        };
        d.register_window(r);
    }
}

impl AppDelegate {
    /// 登记 window 强引用（close 期间必须有人持有，见 Ivars.windows）。
    fn register_window(&self, w: Retained<NSWindow>) {
        self.ivars().windows.borrow_mut().push(w);
    }

    /// windowWillClose 时调：安排下一拍释放强引用。
    fn schedule_release(&self, w: &NSWindow) {
        self.ivars().closing.borrow_mut().push(w as *const NSWindow);
        if self.ivars().prune_scheduled.get() {
            return;
        }
        self.ivars().prune_scheduled.set(true);
        // SAFETY: ObjC 的 -self 按约定返回 retain 过的自身引用。
        let target: Retained<objc2::runtime::AnyObject> = unsafe { msg_send![self, self] };
        // SAFETY: scheduledTimer 平凡。
        let timer = unsafe {
            objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                0.05,
                &target,
                objc2::sel!(ninjaPruneClosedWindows:),
                None,
                false,
            )
        };
        std::mem::forget(timer); // 一次性；触发即失效
    }
}

/// 进程入口（q1 交互壳）：ghostty_init → app/config → 菜单/delegate →
/// runloop。⌘Q / 最后窗关闭 → NSApp.run 返回 → main 统一收尾 free。
pub fn run() {
    let mtm = MainThreadMarker::new().expect("ninja must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    // ghostty 全局初始化 + 配置（默认配置；加载/热重载是 q2——q1 只用
    // 钉点默认：默认 shell、默认键位、macOS 默认语义）。
    unsafe {
        assert_eq!(ghostty_sys::ghostty_init(0, std::ptr::null_mut()), 0, "ghostty_init failed");
        let info = ghostty_sys::ghostty_info();
        let version = std::str::from_utf8(std::slice::from_raw_parts(
            info.version as *const u8,
            info.version_len,
        ))
        .unwrap()
        .to_string();
        println!("ninja q1 shell — libghostty {version}");
        let config = ghostty_sys::ghostty_config_new();
        ghostty_sys::ghostty_config_load_default_files(config);
        ghostty_sys::ghostty_config_finalize(config);
        host::init(ghostty_sys::ghostty_app_new(&host::runtime_config(), config), config);
    }
    ghostty_app_set_focus_compat(true);

    build_menu(mtm, &app);

    // 两阶段初始化（v1 惯例）：先放 ivars 再走 NSObject 的 init。
    let this = AppDelegate::alloc(mtm).set_ivars(Ivars {
        selftest: RefCell::new(None),
        zoom_file: RefCell::new(None),
        zoom_last: RefCell::new(None),
        windows: RefCell::new(Vec::new()),
        closing: RefCell::new(Vec::new()),
        prune_scheduled: Cell::new(false),
    });
    // SAFETY: super 的 init；ivars 已就位。
    let delegate: Retained<AppDelegate> = unsafe { msg_send![super(this), init] };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    DELEGATE.store(&*delegate as *const AppDelegate as *mut AppDelegate, Ordering::Release);

    // 主 RunLoop tick（16ms）：app_tick 驱动 action/邮箱（渲染线程自画）。
    {
        let rl = objc2_core_foundation::CFRunLoop::main().unwrap();
        let mut ctx = objc2_core_foundation::CFRunLoopTimerContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: None,
            release: None,
            copyDescription: None,
        };
        // SAFETY: timer 创建平凡。
        let timer = unsafe {
            objc2_core_foundation::CFRunLoopTimer::new(
                None,
                0.05,
                0.016,
                0,
                0,
                Some(host::tick_cb),
                &mut ctx,
            )
        }
        .expect("timer create");
        unsafe { rl.add_timer(Some(&timer), objc2_core_foundation::kCFRunLoopCommonModes) };
        std::mem::forget(timer); // 进程生命期常驻
    }

    app.run();

    // NSApp.run 返回（⌘Q / 最后窗关闭）：delegate 被 AppKit 弱引用，
    // 保持存活；同步收尾全部 surface/app/config（host::shutdown）。
    std::mem::forget(delegate);
    shell::shutdown_all_windows(mtm);
    host::shutdown();
}

fn ghostty_app_set_focus_compat(focused: bool) {
    host::with_app(|app| unsafe { ghostty_sys::ghostty_app_set_focus(app, focused) });
}
