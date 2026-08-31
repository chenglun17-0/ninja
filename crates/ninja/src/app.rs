//! q2 配置系统 + q1 AppKit 壳引导：NSApplication（Regular）+
//! AppDelegate（窗口注册表、裸⌘W 决策、windowWillClose 收尾）+
//! **键位全量继承 ghostty 的菜单** + 取证钩子。
//!
//! - 键位单一来源：菜单 keyEquivalent 全部由 `ghostty_config_trigger(action)`
//!   推导（用户 ghostty keybind 重绑后菜单同步），菜单项触发走
//!   `ghostty_surface_binding_action`（与键位同一 action 路径）；q1 的
//!   ItemSpec 硬编码键位表已删（平行键位层不复活），ninja.toml [keys]
//!   语义不复活。⌘W 菜单项保留 performClose: 裸⌘W 窗口决策（shell.rs，
//!   非键位绑定层；keyEquivalent 仍取 trigger(close_surface)）。
//! - ninja 特有动作（插件面板 ⌘,）：认领 ghostty 空闲动作
//!   toggle_visibility，宿主层绑 ⌘,，用户可 `keybind = …=toggle_visibility`
//!   统一重绑（ghostty 动作集封闭，自定义动作名不可用——已取证）。面板
//!   UI 是 q3 交付，q2 动作接收记日志（dispatch 见 host.rs）。
//! - 热重载：NSTimer 轮询配置文件 mtime → host::schedule_reload（管线
//!   重跑 + ghostty_app_update_config + 菜单/派生态刷新）；⌘⇧,
//!   （reload_config action）同途。
//! - 窗口注册表 + releasedWhenClosed(false) + 延迟 prune（v1 SIGSEGV
//!   教训）。
//! - 取证钩子：NINJA_P2_SELFTEST（tab,split,win,close,closepane,
//!   closebinding,reloadcfg,cfgdump）+ NINJA_ZOOM_FILE/NINJA_ZOOM_DUMP
//!   （含 cfgdump/reloadcfg/panel/bindact）+ NINJA_CFG_DUMP（生效配置
//!   JSON，启动/重载/cfgdump 时写）。

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

            // 热重载监视拍（0.5s 轮询配置文件 mtime；配置链见
            // crate::config）。变化 → host::schedule_reload（下一拍重跑
            // 装载管线 + ghostty_app_update_config + 菜单/派生态刷新）。
            {
                // SAFETY: 同上（-self 返回 retain 过的引用）。
                let target: Retained<objc2::runtime::AnyObject> = unsafe { msg_send![self, self] };
                // SAFETY: scheduledTimer 平凡。
                let timer = unsafe {
                    objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                        0.5,
                        &target,
                        objc2::sel!(ninjaConfigTick:),
                        None,
                        true,
                    )
                };
                std::mem::forget(timer); // 进程生命期常驻
            }

            // 取证钩子：NINJA_P2_SELFTEST=tab,split,win,close,closepane,
            // closebinding,reloadcfg,cfgdump——runloop 起转、首窗 key 后按
            // 序触发（免 CGEvent 抖动）。close=菜单 performClose 路径
            //（裸⌘W 决策）；closebinding=ghostty close_surface 绑定路径
            //（surface_binding_action 直驱）；reloadcfg=⌘⇧, 同途的
            // reload_config action；cfgdump=写 NINJA_CFG_DUMP。未知项忽略。
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
            // 到 NINJA_ZOOM_DUMP（布局/隐藏/网格尺寸/内容取证）；q2 增
            // "cfgdump"（写 NINJA_CFG_DUMP）、"reloadcfg"（⌘⇧, 同途的
            // 绑定驱动热重载）与 "panel"（binding_action(toggle_visibility)
            // 直证 ninja 特有动作进 dispatch）。E2E 用。
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
                    // reload_config action 路径（⌘⇧, 同途）：绑定驱动热重载。
                    "reloadcfg" => {
                        let f = (|| {
                            let w = key_container()?;
                            let container = crate::pane::container_of(&w)?;
                            container
                                .focused_leaf()
                                .or_else(|| container.leaves().first().cloned())
                                .and_then(|f| f.surface_opt())
                        })();
                        let Some(s) = f else {
                            eprintln!("ninja: NINJA_P2_SELFTEST reloadcfg 无可用 surface");
                            continue;
                        };
                        // SAFETY: 公开 C API；reload_config 是 app-scoped，
                        // surface_binding_action 会转发到 app 路径。
                        unsafe {
                            ghostty_sys::ghostty_surface_binding_action(
                                s,
                                c"reload_config".as_ptr(),
                                c"reload_config".to_bytes().len(),
                            )
                        };
                    }
                    // 写 NINJA_CFG_DUMP（此刻 surface 已建，取证用）。
                    "cfgdump" => {
                        host::dump_config_if_requested();
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
                // bindact:<action>：任意 ghostty 动作经 binding_action
                //（performBindingAction，与键位派发同一 action 核心）直驱
                //——菜单镜像集外的动作用（decrease_font_size 等）。
                s if s.starts_with("bindact:") => {
                    let name = s["bindact:".len()..].to_string();
                    let Some(f) = container
                        .focused_leaf()
                        .or_else(|| container.leaves().first().cloned())
                        .and_then(|f| f.surface_opt())
                    else {
                        eprintln!("ninja: NINJA_ZOOM_FILE bindact 无可用 surface");
                        return;
                    };
                    // SAFETY: 公开 C API；动作名合法（E2E 固定值）。
                    unsafe {
                        ghostty_sys::ghostty_surface_binding_action(
                            f,
                            name.as_ptr() as *const std::ffi::c_char,
                            name.len(),
                        )
                    };
                }
                // panel：binding_action("toggle_visibility")——ninja 特有
                // 动作经 ghostty 绑定系统驱动（q2 dispatch 记日志，面板 UI
                // 是 q3 交付；E2E 直证 TOGGLE_VISIBILITY 到宿主 dispatch）。
                "panel" => {
                    let Some(f) = container
                        .focused_leaf()
                        .or_else(|| container.leaves().first().cloned())
                        .and_then(|f| f.surface_opt())
                    else {
                        eprintln!("ninja: NINJA_ZOOM_FILE panel 无可用 surface");
                        return;
                    };
                    // SAFETY: 公开 C API；toggle_visibility 是 app-scoped，
                    // surface_binding_action 转发 app 路径 → action_cb。
                    unsafe {
                        ghostty_sys::ghostty_surface_binding_action(
                            f,
                            c"toggle_visibility".as_ptr(),
                            c"toggle_visibility".to_bytes().len(),
                        )
                    };
                }
                // cfgdump：写 NINJA_CFG_DUMP（E2E 在任意时刻取生效配置快照）。
                "cfgdump" => host::dump_config_if_requested(),
                // reloadcfg：⌘⇧,（reload_config action）同途的绑定驱动
                // 热重载（E2E 可在任意时刻触发 action 路径）。
                "reloadcfg" => {
                    let Some(f) = container
                        .focused_leaf()
                        .or_else(|| container.leaves().first().cloned())
                        .and_then(|f| f.surface_opt())
                    else {
                        eprintln!("ninja: NINJA_ZOOM_FILE reloadcfg 无可用 surface");
                        return;
                    };
                    // SAFETY: 公开 C API；reload_config 是 app-scoped，
                    // surface_binding_action 转发 app 路径。
                    unsafe {
                        ghostty_sys::ghostty_surface_binding_action(
                            f,
                            c"reload_config".as_ptr(),
                            c"reload_config".to_bytes().len(),
                        )
                    };
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

        /// ⌘N：菜单 File→New Window（绑定路径；无焦点面时回退宿主直驱）。
        #[unsafe(method(ninjaActNewWindow:))]
        fn ninja_act_new_window(&self, _sender: Option<&objc2::runtime::AnyObject>) {
            if !perform_menu_binding("new_window") {
                let Some(mtm) = MainThreadMarker::new() else { return };
                shell::new_window(mtm, None);
            }
        }

        /// ⌘T / File→New Tab：同上（binding_action("new_tab")）。
        #[unsafe(method(ninjaActNewTab:))]
        fn ninja_act_new_tab(&self, _sender: Option<&objc2::runtime::AnyObject>) {
            if !perform_menu_binding("new_tab") {
                let Some(mtm) = MainThreadMarker::new() else { return };
                shell::new_tab(mtm, None);
            }
        }

        /// Panes 菜单动作（绑定路径；无焦点面时 no-op）。
        #[unsafe(method(ninjaActSplitRight:))]
        fn ninja_act_split_right(&self, _s: Option<&objc2::runtime::AnyObject>) {
            perform_menu_binding("new_split:right");
        }

        #[unsafe(method(ninjaActSplitDown:))]
        fn ninja_act_split_down(&self, _s: Option<&objc2::runtime::AnyObject>) {
            perform_menu_binding("new_split:down");
        }

        #[unsafe(method(ninjaActToggleZoom:))]
        fn ninja_act_toggle_zoom(&self, _s: Option<&objc2::runtime::AnyObject>) {
            perform_menu_binding("toggle_split_zoom");
        }

        #[unsafe(method(ninjaActFocusLeft:))]
        fn ninja_act_focus_left(&self, _s: Option<&objc2::runtime::AnyObject>) {
            perform_menu_binding("goto_split:left");
        }

        #[unsafe(method(ninjaActFocusRight:))]
        fn ninja_act_focus_right(&self, _s: Option<&objc2::runtime::AnyObject>) {
            perform_menu_binding("goto_split:right");
        }

        #[unsafe(method(ninjaActFocusUp:))]
        fn ninja_act_focus_up(&self, _s: Option<&objc2::runtime::AnyObject>) {
            perform_menu_binding("goto_split:up");
        }

        #[unsafe(method(ninjaActFocusDown:))]
        fn ninja_act_focus_down(&self, _s: Option<&objc2::runtime::AnyObject>) {
            perform_menu_binding("goto_split:down");
        }

        #[unsafe(method(ninjaActPrevPane:))]
        fn ninja_act_prev_pane(&self, _s: Option<&objc2::runtime::AnyObject>) {
            perform_menu_binding("goto_split:previous");
        }

        #[unsafe(method(ninjaActNextPane:))]
        fn ninja_act_next_pane(&self, _s: Option<&objc2::runtime::AnyObject>) {
            perform_menu_binding("goto_split:next");
        }

        /// ⌘T / 系统标签栏 +：新标签（NSResponder 动作 newWindowForTab:）。
        #[unsafe(method(newWindowForTab:))]
        fn new_window_for_tab(&self, _sender: Option<&objc2::runtime::AnyObject>) {
            let Some(mtm) = MainThreadMarker::new() else { return };
            shell::new_tab(mtm, None); // make_window 内 wire_window
        }

        /// ⌘, / App 菜单「Plugins…」：驱动 toggle_visibility（q2 dispatch
        // 记日志；插件面板 UI 是 q3 交付）。键位语义来自宿主层认领。
        #[unsafe(method(ninjaPlugins:))]
        fn ninja_plugins(&self, _sender: Option<&objc2::runtime::AnyObject>) {
            perform_menu_binding("toggle_visibility");
        }

        /// 热重载执行拍（host::schedule_reload 起的 timer 落这里）。
        #[unsafe(method(ninjaReloadTick:))]
        fn ninja_reload_tick(&self, _timer: Option<&objc2::runtime::AnyObject>) {
            host::reload_tick();
        }

        /// 配置文件 mtime 监视拍（0.5s repeating）。
        #[unsafe(method(ninjaConfigTick:))]
        fn ninja_config_tick(&self, _timer: Option<&objc2::runtime::AnyObject>) {
            host::watch_tick();
        }
    }

    unsafe impl NSObjectProtocol for AppDelegate {}
);

// ---------------------------------------------------------------------------
// 菜单：键位全量继承 ghostty（q2）——keyEquivalent 全部由
// ghostty_config_trigger(action) 推导，菜单点击走 binding_action（同一
// action 路径）。q1 的 ItemSpec 硬编码键位表已删（平行键位层不复活）。
// ---------------------------------------------------------------------------

/// 菜单项：标题 + selector + ghostty 动作名（keyEquivalent 从该动作的
/// 生效绑定推导；Close 项 selector 走 performClose: 裸⌘W 决策，但
/// keyEquivalent 仍取 trigger(close_surface) 同源）。
struct MenuSpec {
    title: &'static str,
    selector: &'static str,
    action: &'static str,
}

const APP_ITEMS: &[MenuSpec] = &[
    // 插件面板 = ninja 特有动作：认领 ghostty 空闲动作 toggle_visibility
    //（宿主层绑 ⌘,，用户可经 keybind 统一重绑；面板 UI 是 q3 交付，
    // q2 点击/按键驱动 dispatch 记日志）。
    MenuSpec { title: "Plugins…", selector: "ninjaPlugins:", action: "toggle_visibility" },
    MenuSpec { title: "Quit ninja", selector: "terminate:", action: "quit" },
];

const FILE_ITEMS: &[MenuSpec] = &[
    MenuSpec { title: "New Window", selector: "ninjaActNewWindow:", action: "new_window" },
    MenuSpec { title: "New Tab", selector: "ninjaActNewTab:", action: "new_tab" },
    // 裸⌘W 决策保留（shell.rs：多 pane 只关焦点面、单 pane 放行原生语义，
    // 非键位绑定层）；keyEquivalent 仍与 ghostty close_surface 绑定同源。
    MenuSpec { title: "Close", selector: "performClose:", action: "close_surface" },
];

const PANES_ITEMS: &[MenuSpec] = &[
    MenuSpec { title: "Split Right", selector: "ninjaActSplitRight:", action: "new_split:right" },
    MenuSpec { title: "Split Down", selector: "ninjaActSplitDown:", action: "new_split:down" },
    MenuSpec { title: "Zoom Pane", selector: "ninjaActToggleZoom:", action: "toggle_split_zoom" },
    MenuSpec { title: "Focus Pane Left", selector: "ninjaActFocusLeft:", action: "goto_split:left" },
    MenuSpec { title: "Focus Pane Right", selector: "ninjaActFocusRight:", action: "goto_split:right" },
    MenuSpec { title: "Focus Pane Up", selector: "ninjaActFocusUp:", action: "goto_split:up" },
    MenuSpec { title: "Focus Pane Down", selector: "ninjaActFocusDown:", action: "goto_split:down" },
    MenuSpec { title: "Previous Pane", selector: "ninjaActPrevPane:", action: "goto_split:previous" },
    MenuSpec { title: "Next Pane", selector: "ninjaActNextPane:", action: "goto_split:next" },
];

const WINDOW_ITEMS: &[MenuSpec] = &[
    MenuSpec { title: "Next Tab", selector: "selectNextTab:", action: "next_tab" },
    MenuSpec { title: "Previous Tab", selector: "selectPreviousTab:", action: "previous_tab" },
];

const EDIT_ITEMS: &[MenuSpec] = &[
    // selector 落 first responder（SurfaceHostView 实现并转 binding_action）。
    // 注意：copy/paste 的默认绑定带 performable 旗标——Trigger.Set 不为
    // performable 绑定建反向映射（getTrigger 返空，Binding.zig putFlags），
    // 菜单不显示快捷键（⌘C/⌘V 仍由 surface_key → ghostty 运行时判定执行，
    // 不被菜单拦截——语义正确，已知语义）；selectAll 无旗标，⌘A 正常镜像。
    MenuSpec { title: "Copy", selector: "copy:", action: "copy_to_clipboard" },
    MenuSpec { title: "Paste", selector: "paste:", action: "paste_from_clipboard" },
    MenuSpec { title: "Select All", selector: "selectAll:", action: "select_all" },
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

fn add_item(mtm: MainThreadMarker, menu: &NSMenu, spec: &MenuSpec) {
    // keyEquivalent 单一来源：生效 ghostty 配置里该动作的当前绑定
    //（未绑定 → 无快捷键、菜单点击不驱动——菜单镜像键位系统）。
    let equiv = host::config().and_then(|cfg| crate::config::action_equivalent(cfg, spec.action));
    let sel = std::ffi::CString::new(spec.selector).expect("selector cstr");
    let key = equiv
        .and_then(|e| char::from_u32(u32::from(e.key)))
        .map(String::from)
        .unwrap_or_default();
    // SAFETY: NSMenuItem 指定初始化器；参数平凡。
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(spec.title),
            Some(objc2::runtime::Sel::register(sel.as_c_str())),
            &NSString::from_str(&key),
        )
    };
    if let Some(e) = equiv {
        let mut flags = NSEventModifierFlags(0);
        if e.cmd {
            flags |= NSEventModifierFlags::Command;
        }
        if e.shift {
            flags |= NSEventModifierFlags::Shift;
        }
        if e.alt {
            flags |= NSEventModifierFlags::Option;
        }
        if e.ctrl {
            flags |= NSEventModifierFlags::Control;
        }
        item.setKeyEquivalentModifierMask(flags);
    }
    menu.addItem(&item);
}

/// 建菜单栏：App / File / Panes / Window / Edit（q1 布局；键位自配置）。
/// 热重载后重调（配置变化 → 键位/菜单同步）。
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
    // 分隔：Zoom Pane（布局态）之后、导航组尾之后（q1 同款）。
    for (i, spec) in PANES_ITEMS.iter().enumerate() {
        if i == 3 || i == 7 {
            pane_menu.addItem(&NSMenuItem::separatorItem(mtm));
        }
        add_item(mtm, &pane_menu, spec);
    }

    let window_menu = add_submenu(mtm, &main_menu, "Window");
    for spec in WINDOW_ITEMS {
        add_item(mtm, &window_menu, spec);
    }

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

/// 进程入口（q2 配置壳）：ghostty_init → 装载管线（宿主层/ODP 层/用户
/// 配置/finalize，见 crate::config）→ app → 菜单（键位自配置推导）→
/// delegate/runloop + 热重载监视。⌘Q / 最后窗关闭 → NSApp.run 返回 →
/// main 统一收尾 free。
pub fn run() {
    let mtm = MainThreadMarker::new().expect("ninja must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    // ghostty 全局初始化 + q2 全量装载管线（用户既有 ghostty 配置直接
    // 生效：主题/字体/键位；GHOSTTY_RESOURCES_DIR 在 main 里已就位）。
    unsafe {
        assert_eq!(ghostty_sys::ghostty_init(0, std::ptr::null_mut()), 0, "ghostty_init failed");
        let info = ghostty_sys::ghostty_info();
        let version = std::str::from_utf8(std::slice::from_raw_parts(
            info.version as *const u8,
            info.version_len,
        ))
        .unwrap()
        .to_string();
        let (config, load_info) = crate::config::load_pipeline();
        println!(
            "ninja q2 shell — libghostty {version}；配置：用户 theme={} ODP={} 监视 {} 文件；资源目录 {:?}",
            load_info.user_theme,
            load_info.odp_applied,
            load_info.watched.len(),
            std::env::var("GHOSTTY_RESOURCES_DIR").unwrap_or_default()
        );
        host::init(
            ghostty_sys::ghostty_app_new(&host::runtime_config(), config),
            config,
            load_info,
        );
    }
    ghostty_app_set_focus_compat(true);

    // 菜单（keyEquivalent 从生效配置的 trigger 推导）+ 启动取证 dump。
    build_menu(mtm, &app);
    host::dump_config_if_requested();

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

// ---------------------------------------------------------------------------
// 菜单/键位共享路径 + 配置应用回调
// ---------------------------------------------------------------------------

/// 菜单项动作驱动：焦点面经 `ghostty_surface_binding_action`（与该动作
/// 的键位绑定同一 action 路径——菜单镜像键位系统，无平行层）。
/// 返回是否驱动成功（false = 无可用 surface，调用方可回退宿主直驱）。
fn perform_menu_binding(action: &str) -> bool {
    if std::env::var_os("NINJA_Q1_DEBUG").is_some() {
        eprintln!("ninja: menu→binding_action({action})");
    }
    let Some(view) = host::current_surface_view() else {
        if std::env::var_os("NINJA_Q1_DEBUG").is_some() {
            eprintln!("ninja: menu→binding 无可用 surface，回退/no-op");
        }
        return false;
    };
    view.binding_action(action);
    true
}

/// 配置应用后的壳侧刷新（host::reload_tick 调）：菜单键位重建 + 取证
/// dump。菜单 keyEquivalent 与生效键位同步（用户重绑 → 菜单跟随）。
pub fn on_config_applied() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    build_menu(mtm, &app);
    host::dump_config_if_requested();
}
