//! 插件面板 v2（2026-08-29 用户产品决策）：菜单栏「插件…」（⌘,）开的
//! 极简小窗——每行一个插件：名/启用开关/运行状态（运行中 pid + 内存
//! MB | 已停止 | 已停用）。**开关语义 = 单一 spawn 策略**：开 = 进
//! enabled 名单 + 当场拉起；关 = 立即走 p6 幂等 shutdown 半边（杀进程
//! + 收层/断连）+ 名单移除 + 写回 `ninja.toml`（只重写 enabled 数组，
//! 其余字节含注释不动，见 [`crate::config::save_plugins_enabled`]）。
//! 面板开着时 1s 刷新一次状态；关窗即停（timer 失效）。
//!
//! 窗口极简：列表 + 开关 + 关闭按钮，非 resizable。行发现 = 会话
//! enabled 名单 ∪ `[plugins.paths]` 键 ∪ `NINJA_PLUGIN_DIR` /
//! `~/.config/ninja/plugins` 里的可执行文件（面板是装/卸之外的日常
//! 启停面；宿主同目录段是开发布局回退，不进发现——target 目录噪声）。
//!
//! 面板动作与 `NINJA_P6_PLUGIN_FILE` 取证钩子共用同一条幂等生命周期
//! 路径（[`toggle`] → [`crate::plugins::toggle_plugin`]）；测试编程触发
//! 不依赖合成 CGEvent。

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSControlStateValueOff, NSControlStateValueOn, NSStackView,
    NSTextField, NSUserInterfaceLayoutOrientation, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{NSEdgeInsets, NSNotification, NSObject, NSString};

use crate::plugins;

pub struct PanelIvars {
    window: RefCell<Option<Retained<NSWindow>>>,
    rows: RefCell<Vec<Row>>,
    timer: RefCell<Option<Retained<objc2_foundation::NSTimer>>>,
}

/// 一行 = 名 + 开关 + 状态栏（状态文本 1s 刷新）。
struct Row {
    name: String,
    toggle: Retained<NSButton>,
    status: Retained<NSTextField>,
}

objc2::define_class!(
    // SAFETY: NSObject 子类化无要求；只在主线程碰（MainThreadOnly）。
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = PanelIvars]
    pub struct PluginPanel;

    unsafe impl NSWindowDelegate for PluginPanel {
        // 关窗：停刷新 timer（PRODUCT：面板关了就不花任何 CPU）。
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            self.stop_timer();
        }
    }

    impl PluginPanel {
        /// 开关行动作（checkbox）：状态即意图 → [`toggle`]（与取证钩子
        /// 同一条路径）；失败回弹开关并刷新状态。
        #[unsafe(method(ninjaPanelToggle:))]
        fn toggle_action(&self, sender: Option<&objc2::runtime::AnyObject>) {
            self.handle_toggle(sender);
        }

        /// 1s 刷新拍：只改状态文本（开关状态反映会话真值，不重画行）。
        #[unsafe(method(ninjaPanelTick:))]
        fn tick(&self, _timer: Option<&objc2::runtime::AnyObject>) {
            self.refresh();
        }
    }

    unsafe impl NSObjectProtocol for PluginPanel {}
);

impl PluginPanel {
    /// 开关处理（selector `ninjaPanelToggle:` 的实现；普通 Rust 方法，
    /// 单测/钩子可直调——面板动作的编程触发不依赖合成 CGEvent）。
    fn handle_toggle(&self, sender: Option<&objc2::runtime::AnyObject>) {
        let Some(sender) = sender else { return };
        // SAFETY: 行 checkbox 的 target 是本面板，sender 必为 NSButton。
        let button: &NSButton = unsafe { &*(std::ptr::from_ref(sender) as *const NSButton) };
        let rows = self.ivars().rows.borrow();
        let Some(row) = rows.iter().find(|r| &*r.toggle == button) else {
            return; // 未知发送者（行已重建中）：忽略
        };
        let name = row.name.clone();
        drop(rows);
        let want = button.state() == NSControlStateValueOn;
        if toggle(&name, want) {
            eprintln!(
                "ninja: 面板开关 {name:?} → {}（已生效并写回 ninja.toml）",
                if want { "on" } else { "off" }
            );
        } else {
            eprintln!("ninja: 面板开关 {name:?} → on 失败（绑定失败？），回弹");
            // 失败：开关回弹到动作前的相反态。
            button.setState(if want {
                NSControlStateValueOff
            } else {
                NSControlStateValueOn
            });
        }
        self.refresh();
    }

    /// 刷新全部行的状态文本（运行中 pid + 内存 / 已停止 / 已停用）。
    fn refresh(&self) {
        let snapshot = plugins::status_snapshot();
        let rows = self.ivars().rows.borrow();
        for row in rows.iter() {
            let st = snapshot.iter().find(|s| s.name == row.name);
            row.status.setStringValue(&NSString::from_str(&status_text(st)));
        }
    }

    /// 起 1s 刷新 timer（面板可见期间；幂等）。
    fn start_timer(&self) {
        if self.ivars().timer.borrow().is_some() {
            return;
        }
        // SAFETY: -self 按约定返回 retain 过的自身引用。
        let target: Retained<objc2::runtime::AnyObject> = unsafe { msg_send![self, self] };
        // SAFETY: scheduledTimer 平凡。
        let timer = unsafe {
            objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                1.0,
                &target,
                objc2::sel!(ninjaPanelTick:),
                None,
                true,
            )
        };
        *self.ivars().timer.borrow_mut() = Some(timer);
    }

    /// 停 timer（关窗；幂等）。
    fn stop_timer(&self) {
        if let Some(t) = self.ivars().timer.borrow_mut().take() {
            t.invalidate();
        }
    }

    /// 显示/前置（窗口常驻对象，重开复用）。
    pub fn show(&self) {
        if let Some(w) = self.ivars().window.borrow().as_ref() {
            w.makeKeyAndOrderFront(None);
        }
        self.refresh();
        self.start_timer();
    }
}

/// 状态列文本（面板 1s 刷新；快照缺名 = 已停用）。
fn status_text(st: Option<&plugins::PluginStatus>) -> String {
    match st {
        None => "已停用".to_string(),
        Some(s) if !s.enabled => "已停用".to_string(),
        Some(s) if s.running => {
            let mb = s
                .memory_bytes
                .map(|b| format!(" · {:.1} MB", b as f64 / (1024.0 * 1024.0)))
                .unwrap_or_default();
            match s.pid {
                Some(p) => format!("运行中 · pid {p}{mb}"),
                None => format!("运行中{mb}"),
            }
        }
        Some(s) => match &s.last_error {
            Some(e) => format!("已停止 · {e}"),
            None => "已停止".to_string(),
        },
    }
}

/// **面板开关的完整语义**（与取证钩子同一条路径）：
/// 1. `plugins::toggle_plugin`——会话内启用即拉起 / 禁用即回收（p6
///    幂等生命周期）；
/// 2. 成功即把新 enabled 名单写回 ninja.toml（保留其它字段与注释；
///    落盘失败只警告——会话内已生效，开关不回弹）。
pub fn toggle(name: &str, on: bool) -> bool {
    let ok = plugins::toggle_plugin(name, on);
    if ok {
        let enabled = plugins::session_cfg().enabled;
        crate::config::save_plugins_enabled(&enabled);
    }
    ok
}

/// 面板行发现（打开时算一次）：会话 enabled ∪ paths 键 ∪ 插件目录里的
/// 文件名（宿主同目录段不进发现——见模块文档）。
pub fn known_plugins() -> Vec<String> {
    let cfg = plugins::session_cfg();
    let mut names: std::collections::BTreeSet<String> =
        cfg.enabled.iter().cloned().collect();
    names.extend(cfg.paths.keys().cloned().filter(|k| !k.is_empty()));
    if let Some(dir) = std::env::var_os("NINJA_PLUGIN_DIR") {
        scan_dir_files(std::path::Path::new(&dir), &mut names);
    }
    if let Some(dir) = plugins::user_plugin_dir() {
        scan_dir_files(&dir, &mut names);
    }
    names.into_iter().collect()
}

fn scan_dir_files(dir: &std::path::Path, names: &mut std::collections::BTreeSet<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if !ft.is_file() {
            continue;
        }
        if let Some(n) = e.file_name().to_str()
            && !n.is_empty()
            && !n.starts_with('.')
        {
            names.insert(n.to_string());
        }
    }
}

/// 建面板（窗口 + 行 + timer）。调用方持有返回值（AppDelegate 的
/// ivars；窗口 releasedWhenClosed=NO，关窗只藏不毁）。
pub fn open(mtm: MainThreadMarker) -> Retained<PluginPanel> {
    let names = known_plugins();
    // 两阶段初始化（同 app::AppDelegate 惯例）。
    let this = PluginPanel::alloc(mtm).set_ivars(PanelIvars {
        window: RefCell::new(None),
        rows: RefCell::new(Vec::new()),
        timer: RefCell::new(None),
    });
    // SAFETY: super 的 init；ivars 已就位。
    let panel: Retained<PluginPanel> = unsafe { msg_send![super(this), init] };

    let row_h: f64 = 26.0;
    let pad: f64 = 12.0;
    let close_h: f64 = 30.0;
    let height = pad + names.len() as f64 * row_h + pad + close_h + pad;
    let frame = objc2_foundation::NSRect {
        origin: objc2_foundation::NSPoint { x: 0.0, y: 0.0 },
        size: objc2_foundation::NSSize { width: 500.0, height },
    };
    let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable;
    // SAFETY: NSWindow 指定初始化器；参数平凡。
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setTitle(&NSString::from_str("插件"));
    // SAFETY: 布尔 setter，无别名风险；面板对象是唯一 owner（同 shell 红线）。
    unsafe { window.setReleasedWhenClosed(false) };
    window.setDelegate(Some(&objc2::runtime::ProtocolObject::from_ref(&*panel)));

    let content = window.contentView().expect("窗口必有 content");
    // 纵向 stack（行序列）+ 底部关闭按钮：frame 布局，不进 autolayout。
    let list = NSStackView::initWithFrame(
        NSStackView::alloc(mtm),
        objc2_foundation::NSRect {
            origin: objc2_foundation::NSPoint { x: 0.0, y: close_h + pad },
            size: objc2_foundation::NSSize {
                width: frame.size.width,
                height: names.len() as f64 * row_h,
            },
        },
    );
    list.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    list.setSpacing(2.0);
    list.setEdgeInsets(NSEdgeInsets {
        top: 0.0,
        left: pad,
        bottom: 0.0,
        right: pad,
    });

    // 初始快照：开关初始态与状态首刷一次到位。
    let snapshot = plugins::status_snapshot();
    let session_enabled = plugins::session_cfg().enabled;
    let mut rows = Vec::new();
    // 面板实例作为动作 target（checkbox → ninjaPanelToggle:）。
    let panel_obj: &objc2::runtime::AnyObject = panel.as_super().as_super();
    for name in &names {
        let enabled = snapshot.iter().any(|s| &s.name == name && s.enabled)
            || session_enabled.iter().any(|n| n == name);
        // 行 = [checkbox 开关][名字][状态]。
        let row = NSStackView::initWithFrame(
            NSStackView::alloc(mtm),
            objc2_foundation::NSRect {
                origin: objc2_foundation::NSPoint { x: 0.0, y: 0.0 },
                size: objc2_foundation::NSSize {
                    width: frame.size.width - 2.0 * pad,
                    height: row_h,
                },
            },
        );
        row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        row.setSpacing(8.0);

        // SAFETY: checkbox 构造器；target/action 平凡。
        let toggle_btn = unsafe {
            NSButton::checkboxWithTitle_target_action(
                &NSString::from_str(""),
                Some(panel_obj),
                Some(objc2::sel!(ninjaPanelToggle:)),
                mtm,
            )
        };
        toggle_btn.setState(if enabled {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        row.addArrangedSubview(&toggle_btn);

        let name_field = NSTextField::labelWithString(&NSString::from_str(name), mtm);
        row.addArrangedSubview(&name_field);

        let status_field = NSTextField::labelWithString(&NSString::from_str("…"), mtm);
        row.addArrangedSubview(&status_field);

        list.addArrangedSubview(&row);
        rows.push(Row {
            name: name.clone(),
            toggle: toggle_btn,
            status: status_field,
        });
    }
    *panel.ivars().rows.borrow_mut() = rows;

    // 关闭按钮（极简面的显式出口；红绿灯同效）：target = 窗口，
    // action = performClose:（NSWindow 内建）。
    let window_obj: &objc2::runtime::AnyObject = window.as_super().as_super().as_super();
    // SAFETY: button 构造器；target/action 平凡。
    let close = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("关闭"),
            Some(window_obj),
            Some(objc2::sel!(performClose:)),
            mtm,
        )
    };

    content.addSubview(&list);
    content.addSubview(&close);
    *panel.ivars().window.borrow_mut() = Some(window);
    panel
}
