//! AppKit 引导（p2）：NSApplication（Regular）+ 多窗口 + 原生标签 +
//! 菜单（App/File/Panes/Window/Edit，键位来自 TOML 配置）+ delegate。
//! p3：[plugins] enabled 非空才绑 ADE socket（plugins.rs）；默认空载
//! 无 socket、无子进程除 PTY shell。

#![allow(non_snake_case)] // ObjC selector 方法名
use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::runtime::{ProtocolObject, Sel};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSEventModifierFlags,
    NSMenu, NSMenuItem, NSWindow, NSWindowDelegate,
};
use objc2_foundation::{NSNotification, NSObject, NSString};

use crate::config::{self, Config};
use crate::pane::PaneContainer;
use crate::plugins;
use crate::shell;

pub struct Ivars {
    pub config: Config,
    /// 门禁取证钩子的步骤序列（NINJA_P2_SELFTEST，启动后延时执行）。
    pub selftest: RefCell<Option<String>>,
    /// p4 命中分发取证钩子（NINJA_P4_HIT="col,row"，内容门控重试）。
    pub p4_hit: RefCell<Option<String>>,
    /// p4 钩子的重试定时器（点击后/超时后停）。
    p4_hit_timer: RefCell<Option<Retained<objc2_foundation::NSTimer>>>,
    /// p4 钩子首拍时刻（15s 重试窗口计时用）。
    p4_hit_started: Cell<Option<std::time::Instant>>,
    /// p6 同会话禁用/再启用取证钩子：NINJA_P6_PLUGIN_FILE 轮询的文件路径。
    p6_file: RefCell<Option<String>>,
    /// p6 钩子已应用的目标态（去抖：只在变化时动作）。
    p6_state: Cell<Option<bool>>,
    /// 插件面板控制器（ninjaPlugins: 创建/复用；关窗只藏不毁）。
    pub panel: RefCell<Option<Retained<crate::panel::PluginPanel>>>,
    /// 面板取证钩子：NINJA_PANEL_PLUGIN_FILE 轮询的文件路径（E2E 用，
    /// 与面板开关同一条 toggle 路径；不依赖合成 CGEvent）。
    panel_file: RefCell<Option<String>>,
    /// 面板钩子上次已应用的内容（去抖：内容变化才动作）。
    panel_last: RefCell<Option<String>>,
    /// 本壳持有的窗口强引用。**关键不变量**：窗口在 -[NSWindow close]
    /// 期间必须有人持有（NSApp 的窗口列表引用会在 close 中途摘掉，
    /// 若那是唯一引用，窗口在自己 close 的调用栈里 dealloc，后续
    /// close 路径碰已释放内部锁 → SIGSEGV。p2 实测根因）。因此：
    /// 创建时登记，close 完成后（延迟一拍）再释放。
    pub windows: RefCell<Vec<Retained<NSWindow>>>,
    /// 待释放（已 windowWillClose）窗口的裸指针，去定时器后一拍移除。
    closing: RefCell<Vec<*const NSWindow>>,
    prune_scheduled: Cell<bool>,
}

define_class!(
    // SAFETY:
    // - NSObject 子类化无要求；不实现 Drop。
    // - config 只在主线程读（MainThreadOnly 类）。
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct AppDelegate;

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn applicationDidFinishLaunching(&self, _notification: &NSNotification) {
            let mtm = MainThreadMarker::new().expect("delegate on main thread");
            let app = NSApplication::sharedApplication(mtm);

            // 首个窗口（80x24 cell 内容，见 PaneContainer/TerminalView）。
            let window = shell::make_window(mtm, &self.ivars().config, self);
            window.center();
            window.makeKeyAndOrderFront(None);
            self.register_window(window);

            // 面板 v2 单一策略（2026-08-29 决策）：**启用即拉起**——
            // runloop 就绪后立即拉起全部 enabled 插件（空载 = 无操作，
            // 门禁不变）。拉起后 SPAWN 窗口钉住泵 timer 直到插件连上
            //（连接即推的 theme.set 靠泵消化，见 plugins.rs）。
            plugins::spawn_startup_plugins();

            // 拉起就激活（无 user gesture 的冷启动也拉前台）。deprecated 但
            // 行为稳定（macOS 14 上 activate() 有无手势拉前台失败的坑）。
            #[allow(deprecated)]
            {
                app.activateIgnoringOtherApps(true);
            }

            // 空载门禁取证钩子（非产品功能）：NINJA_P2_SELFTEST=tab,split,win
            // 在 runloop 起转、首窗成 key 后按序触发新标签/分屏/新窗口
            //（免 CGEvent 抖动，多 pane 内存取证可复现）。未知项忽略。
            if let Ok(seq) = std::env::var("NINJA_P2_SELFTEST") {
                self.ivars().selftest.replace(Some(seq));
                // SAFETY: ObjC 的 -self 按约定返回 retain 过的自身引用。
                let target: Retained<objc2::runtime::AnyObject> = unsafe { msg_send![self, self] };
                let timer = unsafe {
                    objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                        0.8,
                        &target,
                        objc2::sel!(ninjaSelfTestTick:),
                        None,
                        false,
                    )
                };
                std::mem::forget(timer); // 只触发一次；进程生命期内 self 常活
            }

            // p4 命中分发取证钩子（非产品功能）：NINJA_P4_HIT="col,row"
            // 在首帧落定后对 key window 的焦点 pane 走一遍 Cmd+点击路径
            //（与 NINJA_P2_SELFTEST 同惯例；真实合成点击取证用
            // tools/verify/synth_input.swift 的 click x y 1）。p5 起改为
            // **内容门控重试**：shell 首行没字节前重试（最多 15s；系统忙
            // 时 fakesh 的 printf 可能晚于定时器首拍，一次性触发会随机
            // 落空——E2E 实测），有内容才真正点击并停表。真实 Cmd+点击
            // 无此等待（点击时内容早已在屏上）。
            if let Ok(spec) = std::env::var("NINJA_P4_HIT") {
                self.ivars().p4_hit.replace(Some(spec));
                // SAFETY: 同上。
                let target: Retained<objc2::runtime::AnyObject> = unsafe { msg_send![self, self] };
                let timer = unsafe {
                    objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                        1.0,
                        &target,
                        objc2::sel!(ninjaP4HitTick:),
                        None,
                        true,
                    )
                };
                self.ivars().p4_hit_timer.replace(Some(timer));
            }

            // p6 同会话禁用/再启用取证钩子（非产品功能）：
            // NINJA_P6_PLUGIN_FILE=<path> 时每 0.2s 读该文件内容
            //（"off"/"0"=禁用，"on"/"1"=再启用），状态变化才动作。
            // 文件触发（同 NINJA_* 惯例）让 E2E/验证员免 CGEvent 驱动
            // 同会话「启用→用一次→禁用→再启用」的生命周期；产品 UI
            // 归后续阶段。
            if let Ok(f) = std::env::var("NINJA_P6_PLUGIN_FILE") {
                self.ivars().p6_file.replace(Some(f));
                // SAFETY: 同上（-self 返回 retain 过的引用）。
                let target: Retained<objc2::runtime::AnyObject> = unsafe { msg_send![self, self] };
                let timer = unsafe {
                    objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                        0.2,
                        &target,
                        objc2::sel!(ninjaP6PluginTick:),
                        None,
                        true,
                    )
                };
                std::mem::forget(timer); // 进程生命期常驻（同 selftest 惯例）
            }

            // 面板取证钩子（非产品功能）：NINJA_PANEL_PLUGIN_FILE=<path>
            // 时每 0.5s 读该文件，内容变化才动作——"open" = 打开面板窗口
            //（编程触发，免 CGEvent）；"<name> on|off" = 走面板开关的
            // 同一条 toggle 路径（panel::toggle：会话生命周期 + 写回
            // ninja.toml）。E2E 用。
            if let Ok(f) = std::env::var("NINJA_PANEL_PLUGIN_FILE") {
                self.ivars().panel_file.replace(Some(f));
                // SAFETY: 同上（-self 返回 retain 过的引用）。
                let target: Retained<objc2::runtime::AnyObject> = unsafe { msg_send![self, self] };
                let timer = unsafe {
                    objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                        0.5,
                        &target,
                        objc2::sel!(ninjaPanelPluginTick:),
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
            true
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn applicationWillTerminate(&self, _notification: &NSNotification) {
            // terminate 不保证逐窗 windowWillClose：主动收尾全部 pane
            //（PTY SIGHUP + join、runloop source 摘除、timer 停）。
            if let Some(mtm) = MainThreadMarker::new() {
                shell::shutdown_all_windows(mtm);
            }
            // p6：`terminate:` 直接 exit(0)，不走 Rust 栈展开——栈上
            // PluginHost 的 Drop 不会跑（⌘Q/关最后窗都走这里；socket
            // 尸体因此不只来自 SIGKILL）。显式关一次（幂等，与 Drop 同
            // 一实现：收层/断连/收割子进程/删 socket）。
            plugins::host_shutdown();
        }
    }

    unsafe impl NSWindowDelegate for AppDelegate {
        /// D-A：⌘W 只关「当前面」（决策群在 shell.rs，带单测）。裸 ⌘W
        /// （我们的 Close=performClose: / 系统 Close Tab）多 pane 窗只关
        /// 焦点 pane、拦掉整窗 close——其余 pane 各自 PTY 独立、shell
        /// 绝不陪葬；单 pane 放行原生语义（关当前 tab，最后 tab 才关
        /// 窗）。非 ⌘W 路径（红绿灯、⇧⌘W/⌥⌘W 系统项、EOF、selftest）
        /// 不受影响。须在 windowWillClose 之前（performClose 先问
        /// shouldClose 再走 close/willClose）。
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, sender: &NSWindow) -> bool {
            shell::window_should_close(sender)
        }

        // 关窗前先收尾该窗全部 pane，防止 runloop source 在窗口拆一半时
        // 进 view。多窗口下每个窗口的 close 都走这里。
        #[unsafe(method(windowWillClose:))]
        fn windowWillClose(&self, notification: &NSNotification) {
            // SAFETY: object() 返回通知对象（此处恒为 NSWindow）。
            let window: Option<&NSWindow> = unsafe {
                msg_send![notification, object]
            };
            if let Some(w) = window {
                if let Some(content) = w.contentView() {
                    shell::window_closed(&content);
                }
                // close 完成后延迟释放登记的强引用（现在放会把窗口
                // 拆在它自己的 close 调用栈里）。
                self.schedule_release(w);
            }
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
    }

    impl AppDelegate {
        /// 门禁钩子：延时执行 selftest 序列（首窗已 key、runloop 已起转）。
        #[unsafe(method(ninjaSelfTestTick:))]
        fn ninja_selftest_tick(&self, _timer: Option<&objc2::runtime::AnyObject>) {
            let Some(seq) = self.ivars().selftest.take() else {
                return;
            };
            let Some(mtm) = MainThreadMarker::new() else { return };
            let app = NSApplication::sharedApplication(mtm);
            for step in seq.split(',').map(str::trim) {
                match step {
                    "tab" => shell::new_tab(mtm, &self.ivars().config, self),
                    "split" => {
                        if let Some(c) = app
                            .keyWindow()
                            .or_else(|| app.mainWindow())
                            .and_then(|w| w.contentView())
                        {
                            // SAFETY: isKindOfClass: 任意 NSObject 可查。
                            let is_c: bool = unsafe {
                                objc2::msg_send![&*c, isKindOfClass: PaneContainer::class()]
                            };
                            if is_c {
                                let container: &PaneContainer = unsafe {
                                    &*(std::ptr::from_ref(&*c) as *const PaneContainer)
                                };
                                container.split_focused(crate::pane::Dir::Horizontal);
                            }
                        }
                    }
                    "win" => shell::new_window(mtm, &self.ivars().config, self),
                    "close" => {
                        if let Some(w) = app.keyWindow().or_else(|| app.mainWindow()) {
                            w.performClose(None);
                        }
                    }
                    "closepane" => {
                        if let Some(c) = app
                            .keyWindow()
                            .or_else(|| app.mainWindow())
                            .and_then(|w| w.contentView())
                        {
                            // SAFETY: isKindOfClass: 任意 NSObject 可查。
                            let is_c: bool = unsafe {
                                objc2::msg_send![&*c, isKindOfClass: PaneContainer::class()]
                            };
                            if is_c {
                                let container: &PaneContainer = unsafe {
                                    &*(std::ptr::from_ref(&*c) as *const PaneContainer)
                                };
                                if let Some(f) = container.focused_leaf() {
                                    container.close_leaf(&f);
                                }
                            }
                        }
                    }
                    other => eprintln!("ninja: NINJA_P2_SELFTEST 未知步骤 {other:?}"),
                }
            }
        }

        /// p4 取证钩子：解析 NINJA_P4_HIT="col,row"，对 key window 的焦点
        /// pane（没有焦点就第一个叶子）走 Cmd+点击命中分发路径。
        #[unsafe(method(ninjaP4HitTick:))]
        fn ninja_p4_hit_tick(&self, _timer: Option<&objc2::runtime::AnyObject>) {
            use std::time::{Duration, Instant};
            let Some(spec) = self.ivars().p4_hit.borrow().clone() else {
                return;
            };
            if self.ivars().p4_hit_started.get().is_none() {
                self.ivars().p4_hit_started.set(Some(Instant::now()));
            }
            if let Some(t0) = self.ivars().p4_hit_started.get() {
                if t0.elapsed() > Duration::from_secs(15) {
                    eprintln!("ninja: NINJA_P4_HIT 等待行内容超时，放弃");
                    self.ivars().p4_hit.take();
                    self.stop_p4_hit_timer();
                    return;
                }
            }
            let Some(mtm) = MainThreadMarker::new() else { return };
            let app = NSApplication::sharedApplication(mtm);
            // key/main 窗口在后台/争激活时会短暂为 nil（并行取证实测），
            // 退化顺序：keyWindow → mainWindow → 已登记窗口表（确定存在）。
            let window = app.keyWindow().or_else(|| app.mainWindow()).or_else(|| {
                self.ivars()
                    .windows
                    .borrow()
                    .first()
                    .map(|w| w.clone())
            });
            let Some(content) = window.and_then(|w| w.contentView()) else {
                eprintln!("ninja: NINJA_P4_HIT 无窗口可命中");
                return;
            };
            // SAFETY: isKindOfClass: 任意 NSObject 可查。
            let is_c: bool =
                unsafe { objc2::msg_send![&*content, isKindOfClass: PaneContainer::class()] };
            if !is_c {
                eprintln!("ninja: NINJA_P4_HIT 窗口内容不是 PaneContainer");
                return;
            }
            // SAFETY: 通过类型检查后的上转（同 selftest 惯例）。
            let container: &PaneContainer =
                unsafe { &*(std::ptr::from_ref(&*content) as *const PaneContainer) };
            let Some(view) = container
                .focused_leaf()
                .or_else(|| container.leaves().first().cloned())
            else {
                return;
            };
            // "col,row"（十进制，可含空白）。
            let Some((col, row)) = spec
                .split(',')
                .map(|s| s.trim().parse::<u16>().ok())
                .collect::<Option<Vec<_>>>()
                .and_then(|v| (v.len() == 2).then(|| (v[0], v[1])))
            else {
                eprintln!("ninja: NINJA_P4_HIT 需为 \"col,row\"（u16），得到 {spec:?}");
                self.stop_p4_hit_timer();
                return;
            };
            // 内容门控：目标行还没字节（shell 首行未落定）→ 等下一拍。
            if view.row_is_blank(row) {
                return;
            }
            self.ivars().p4_hit.take();
            self.stop_p4_hit_timer();
            view.cmd_click(col, row, libghostty_vt::key::Mods::SUPER);
        }

        /// p6 钩子拍：读 NINJA_P6_PLUGIN_FILE 文件内容，状态变化才
        /// 禁用/再启用（plugins::host_set_enabled）；"quit" 驱动正常退出
        /// 路径（terminate: → applicationWillTerminate → host_shutdown →
        /// exit(0)——产品 ⌘Q/关最后窗的同一条路径；E2E 用。
        /// CGEventPostToPid 的 ⌘Q 到不了后台应用的菜单系统，实证）。
        /// 文件没写/被删/内容未知 → 维持现状。
        #[unsafe(method(ninjaP6PluginTick:))]
        fn ninja_p6_plugin_tick(&self, _timer: Option<&objc2::runtime::AnyObject>) {
            let Some(path) = self.ivars().p6_file.borrow().clone() else {
                return;
            };
            let Ok(content) = std::fs::read_to_string(&path) else {
                return;
            };
            let want = match content.trim() {
                "off" | "0" | "disable" => false,
                "on" | "1" | "enable" => true,
                "quit" => {
                    let Some(mtm) = MainThreadMarker::new() else {
                        return;
                    };
                    NSApplication::sharedApplication(mtm).terminate(None);
                    return;
                }
                _ => return, // 未知内容：不动
            };
            if self.ivars().p6_state.get() == Some(want) {
                return; // 已处于目标态
            }
            if plugins::host_set_enabled(want) {
                self.ivars().p6_state.set(Some(want));
            }
        }

        /// ⌘N：新窗口（独立窗口；nil target 动作最终落到 app delegate）。
        #[unsafe(method(ninjaNewWindow:))]
        fn ninja_new_window(&self, _sender: Option<&objc2::runtime::AnyObject>) {
            if let Some(mtm) = MainThreadMarker::new() {
                shell::new_window(mtm, &self.ivars().config, self);
            }
        }

        /// ⌘T / 系统标签栏 +：新标签（NSResponder 动作 newWindowForTab:）。
        #[unsafe(method(newWindowForTab:))]
        fn new_window_for_tab(&self, _sender: Option<&objc2::runtime::AnyObject>) {
            if let Some(mtm) = MainThreadMarker::new() {
                shell::new_tab(mtm, &self.ivars().config, self);
            }
        }

        /// ⌘, / App 菜单「Plugins…」：开/复用插件面板（panel.rs；2026-08-29
        /// 决策：启用即拉起 + 可见的设置面）。可直接编程调用（面板 E2E
        /// 的 "open" 钩子同途）。
        #[unsafe(method(ninjaPlugins:))]
        fn ninja_plugins(&self, _sender: Option<&objc2::runtime::AnyObject>) {
            let Some(mtm) = MainThreadMarker::new() else { return };
            if let Some(p) = self.ivars().panel.borrow().as_ref() {
                p.show();
            } else {
                let p = crate::panel::open(mtm);
                p.show();
                *self.ivars().panel.borrow_mut() = Some(p);
            }
        }

        /// 面板钩子拍：读 NINJA_PANEL_PLUGIN_FILE，内容变化才动作。
        /// "open"/"panel" → 开面板；"<name> on|off|1|0|enable|disable"
        /// → 面板开关同一条 toggle 路径（panel::toggle）。
        #[unsafe(method(ninjaPanelPluginTick:))]
        fn ninja_panel_plugin_tick(&self, _timer: Option<&objc2::runtime::AnyObject>) {
            let Some(path) = self.ivars().panel_file.borrow().clone() else {
                return;
            };
            let Ok(content) = std::fs::read_to_string(&path) else {
                return;
            };
            let content = content.trim().to_string();
            if content.is_empty()
                || self.ivars().panel_last.borrow().as_deref() == Some(content.as_str())
            {
                return; // 去抖：同一内容只动作一次
            }
            match content.as_str() {
                "open" | "panel" => self.ninja_plugins(objc2::sel!(ninjaPlugins:), None),
                _ => {
                    let Some((name, want)) = content
                        .rsplit_once(' ')
                        .map(|(n, w)| (n.trim().to_string(), w.trim()))
                    else {
                        eprintln!(
                            "ninja: NINJA_PANEL_PLUGIN_FILE 需为 \"open\" 或 \"<name> on|off\"，得到 {content:?}"
                        );
                        return;
                    };
                    let on = match want {
                        "on" | "1" | "enable" => true,
                        "off" | "0" | "disable" => false,
                        other => {
                            eprintln!(
                                "ninja: NINJA_PANEL_PLUGIN_FILE 动作 {other:?} 无效（on | off）"
                            );
                            return;
                        }
                    };
                    crate::panel::toggle(&name, on);
                }
            }
            *self.ivars().panel_last.borrow_mut() = Some(content);
        }
    }

    unsafe impl NSObjectProtocol for AppDelegate {}
);

// ---------------------------------------------------------------------------
// 菜单（键位来自 config [keys]；动作经响应链：pane 动作落到 PaneContainer，
// 窗口/标签动作落到本 delegate；copy/paste/selectAll 落到 first responder）
// ---------------------------------------------------------------------------

/// 一个菜单项的描述：(config 动作名, 标题, selector)。
/// 键位查 config.keys[动作名]（缺失用 config 默认表，永不空）。
struct ItemSpec {
    action: &'static str,
    title: &'static str,
    selector: &'static str,
}

const APP_ITEMS: &[ItemSpec] = &[
    ItemSpec {
        action: "plugins",
        title: "Plugins…",
        selector: "ninjaPlugins:",
    },
    ItemSpec {
        action: "quit",
        title: "Quit ninja",
        selector: "terminate:",
    },
];

const FILE_ITEMS: &[ItemSpec] = &[
    ItemSpec {
        action: "new_window",
        title: "New Window",
        selector: "ninjaNewWindow:",
    },
    ItemSpec {
        action: "new_tab",
        title: "New Tab",
        selector: "newWindowForTab:",
    },
    ItemSpec {
        action: "close",
        title: "Close",
        selector: "performClose:",
    },
];

const PANE_ITEMS: &[ItemSpec] = &[
    ItemSpec {
        action: "split_right",
        title: "Split Right",
        selector: "ninjaSplitRight:",
    },
    ItemSpec {
        action: "split_down",
        title: "Split Down",
        selector: "ninjaSplitDown:",
    },
    ItemSpec {
        action: "close_pane",
        title: "Close Pane",
        selector: "ninjaClosePane:",
    },
    ItemSpec {
        action: "focus_left",
        title: "Focus Pane Left",
        selector: "ninjaFocusLeft:",
    },
    ItemSpec {
        action: "focus_right",
        title: "Focus Pane Right",
        selector: "ninjaFocusRight:",
    },
    ItemSpec {
        action: "focus_up",
        title: "Focus Pane Up",
        selector: "ninjaFocusUp:",
    },
    ItemSpec {
        action: "focus_down",
        title: "Focus Pane Down",
        selector: "ninjaFocusDown:",
    },
    ItemSpec {
        action: "prev_pane",
        title: "Previous Pane",
        selector: "ninjaPrevPane:",
    },
    ItemSpec {
        action: "next_pane",
        title: "Next Pane",
        selector: "ninjaNextPane:",
    },
];

const EDIT_ITEMS: &[ItemSpec] = &[
    ItemSpec {
        action: "copy",
        title: "Copy",
        selector: "copy:",
    },
    ItemSpec {
        action: "paste",
        title: "Paste",
        selector: "paste:",
    },
    ItemSpec {
        action: "select_all",
        title: "Select All",
        selector: "selectAll:",
    },
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

/// 按 spec 造菜单项（键位查 config；查不到用默认绑定——config 默认表
/// 覆盖全部动作名，这里只是防御）。
fn add_item(mtm: MainThreadMarker, menu: &NSMenu, spec: &ItemSpec, config: &Config) {
    let binding = config.keys.get(spec.action).cloned().unwrap_or_else(|| {
        eprintln!("ninja: 动作 {} 无绑定（用默认表）", spec.action);
        config::default_keys()
            .into_iter()
            .find(|(n, _)| n == spec.action)
            .map(|(_, b)| b)
            .expect("default table covers all actions")
    });
    let item = unsafe {
        let sel = std::ffi::CString::new(spec.selector).expect("selector cstr");
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(spec.title),
            Some(Sel::register(sel.as_c_str())),
            &NSString::from_str(&binding.key),
        )
    };
    item.setKeyEquivalentModifierMask(NSEventModifierFlags(binding.flags() as usize));
    menu.addItem(&item);
}

/// 建菜单栏：App / File / Panes / Window / Edit。Window 菜单的
/// Next/Previous Tab 走 NSWindow 内建动作（固定 ⌘⇧]/⌘⇧[）。
fn build_menu(mtm: MainThreadMarker, app: &NSApplication, config: &Config) {
    let main_menu = NSMenu::new(mtm);

    let app_menu = add_submenu(mtm, &main_menu, "__app__");
    for (i, spec) in APP_ITEMS.iter().enumerate() {
        // 面板项与退出项之间加分隔线（macOS 菜单惯例）。
        if i == APP_ITEMS.len() - 1 {
            app_menu.addItem(&NSMenuItem::separatorItem(mtm));
        }
        add_item(mtm, &app_menu, spec, config);
    }

    let file_menu = add_submenu(mtm, &main_menu, "File");
    for spec in FILE_ITEMS {
        add_item(mtm, &file_menu, spec, config);
    }

    let pane_menu = add_submenu(mtm, &main_menu, "Panes");
    for (i, spec) in PANE_ITEMS.iter().enumerate() {
        if i == 3 || i == 7 {
            pane_menu.addItem(&NSMenuItem::separatorItem(mtm));
        }
        add_item(mtm, &pane_menu, spec, config);
    }

    let window_menu = add_submenu(mtm, &main_menu, "Window");
    let next_tab = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Next Tab"),
            Some(Sel::register(c"selectNextTab:")),
            &NSString::from_str("]"),
        )
    };
    next_tab
        .setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Shift);
    window_menu.addItem(&next_tab);
    let prev_tab = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Previous Tab"),
            Some(Sel::register(c"selectPreviousTab:")),
            &NSString::from_str("["),
        )
    };
    prev_tab
        .setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Shift);
    window_menu.addItem(&prev_tab);

    let edit_menu = add_submenu(mtm, &main_menu, "Edit");
    for spec in EDIT_ITEMS {
        add_item(mtm, &edit_menu, spec, config);
    }

    app.setMainMenu(Some(&main_menu));
}

/// 进程入口：读配置 → 起 AppKit、上菜单（配置键位）、挂 delegate、
/// 跑 runloop。多窗口；最后窗关闭 / ⌘Q / 各 pane shell 全退 → 退出。
/// p3：[plugins] enabled 非空才绑 ADE socket，默认空载不建。
pub fn run() {
    let mtm = MainThreadMarker::new().expect("ninja must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    // p2 配置：缺文件 = 内置默认（可启动门禁）；坏字段降级默认 + 警告。
    let config = Config::load();
    build_menu(mtm, &app, &config);

    // p3 ADE 插件门：默认（enabled 空）不建 socket、不拉任何插件进程
    //（空载门禁）。启用时绑 Unix socket；拉起在 runloop 就绪后
    //（applicationDidFinishLaunching → spawn_startup_plugins，2026-08-29
    // 决策：启用即拉起）。面板 v2：分发器静态槽持 Arc（运行中从零拉起
    // 插件需要可造新 host）；退出收口 = applicationWillTerminate →
    // host_shutdown（幂等，与 Drop 同一实现）。
    match plugins::PluginHost::start(&config.plugins) {
        Some(h) => {
            plugins::install_dispatcher(
                std::sync::Arc::new(std::sync::Mutex::new(h)),
                config.plugins.clone(),
            );
        }
        None => plugins::install_session_cfg(config.plugins.clone()),
    }

    // 两阶段初始化（同 view）：先放 ivars 再走 NSObject 的 init。
    let this = AppDelegate::alloc(mtm).set_ivars(Ivars {
        config,
        selftest: RefCell::new(None),
        p4_hit: RefCell::new(None),
        p4_hit_timer: RefCell::new(None),
        p4_hit_started: Cell::new(None),
        p6_file: RefCell::new(None),
        p6_state: Cell::new(None),
        panel: RefCell::new(None),
        panel_file: RefCell::new(None),
        panel_last: RefCell::new(None),
        windows: RefCell::new(Vec::new()),
        closing: RefCell::new(Vec::new()),
        prune_scheduled: Cell::new(false),
    });
    // SAFETY: super 的 init；ivars 已就位。
    let delegate: Retained<AppDelegate> = unsafe { msg_send![super(this), init] };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    app.run();

    // delegate 被 AppKit 的 delegate 槽 weak 引用；进程生命期内保持存活。
    std::mem::forget(delegate);
}

/// Rust 侧公开接口（define_class 外的 impl）。
impl AppDelegate {
    /// 停 p4 取证定时器（点击已发/参数错/超时）。普通 Rust 方法，
    /// 不挂 selector（define_class 内的无属性方法会被当 ObjC 方法拒收）。
    fn stop_p4_hit_timer(&self) {
        if let Some(t) = self.ivars().p4_hit_timer.take() {
            t.invalidate();
        }
    }

    /// 登记 window 强引用（make_window 后立即调；见 Ivars.windows）。
    /// 窗口在 -close 期间必须有人持有（NSApp 窗口列表的引用会在 close
    /// 中途摘掉，唯一引用会让窗口拆在自己的 close 调用栈里 → SIGSEGV）。
    pub fn register_window(&self, w: Retained<NSWindow>) {
        self.ivars().windows.borrow_mut().push(w);
    }

    /// windowWillClose 时调：安排下一拍释放强引用。close 尚在
    /// 进行中，不能立即 drop。
    fn schedule_release(&self, w: &NSWindow) {
        self.ivars().closing.borrow_mut().push(w as *const NSWindow);
        if self.ivars().prune_scheduled.get() {
            return;
        }
        self.ivars().prune_scheduled.set(true);
        // SAFETY: ObjC 的 -self 按约定返回 retain 过的自身引用。
        let target: Retained<objc2::runtime::AnyObject> = unsafe { msg_send![self, self] };
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
