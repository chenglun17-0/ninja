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
        }
    }

    unsafe impl NSWindowDelegate for AppDelegate {
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

const APP_ITEMS: &[ItemSpec] = &[ItemSpec {
    action: "quit",
    title: "Quit ninja",
    selector: "terminate:",
}];

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
    for spec in APP_ITEMS {
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
    //（空载门禁）。启用时绑 Unix socket；accept/拉起在 p5。
    // 生命周期：住在 run() 栈上，app.run() 返回（退出）时 drop 并删
    // socket 文件。
    let _plugin_host = plugins::PluginHost::start(&config.plugins);

    // 两阶段初始化（同 view）：先放 ivars 再走 NSObject 的 init。
    let this = AppDelegate::alloc(mtm).set_ivars(Ivars {
        config,
        selftest: RefCell::new(None),
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
