//! q3 插件面板（⌘, → `toggle_visibility` 动作）：极简 NSWindow。
//!
//! 行 = 名 / 开关 / 运行状态（pid+MB 或「已停止(原因)」）。开关即启停
//! （宿主 [`crate::plugins::toggle_plugin`] 的「启用即拉起/禁用即回收」
//! 单一生命周期）+ 名单写回 ninja.toml（[`crate::config::write_plugins_enabled`]）。
//!
//! 行集 = 会话真值（enabled ∪ 在跑 ∪ 有错误记录；面板不做插件发现——
//! 分发市场不是本阶段）。E2E 钩子 `NINJA_PANEL_PLUGIN_FILE`（app.rs 轮询
//! 「<name> on|off」行，与 UI 开关同一条路径，免 CGEvent）。

#![allow(non_snake_case)] // ObjC selector 方法名

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBackingStoreType, NSButton, NSButtonType, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// 一行 = 一个插件。
struct Row {
    name: String,
    check: Retained<NSButton>,
    status: Retained<objc2_app_kit::NSTextField>,
}

pub struct Ivars {
    window: RefCell<Option<Retained<NSWindow>>>,
    rows: RefCell<Vec<Row>>,
    refresh_scheduled: RefCell<bool>,
}

define_class!(
    // SAFETY: NSObject 子类化无要求；不实现 Drop；全部状态主线程访问。
    #[unsafe(super(objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct PluginPanel;

    impl PluginPanel {
        /// 行开关动作（checkbox target）。
        #[unsafe(method(ninjaToggle:))]
        fn ninja_toggle(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            // SAFETY: sender 是本面板的 NSButton（target-action 配对）。
            let state: isize = unsafe { msg_send![sender, state] };
            let on = state == 1;
            let sender_ptr = sender as *const AnyObject;
            let Some(name) = self
                .ivars()
                .rows
                .borrow()
                .iter()
                .find(|r| {
                    std::ptr::eq(&*r.check as *const NSButton as *const AnyObject, sender_ptr)
                })
                .map(|r| r.name.clone())
            else {
                return;
            };
            apply_toggle(&name, on);
        }

        /// 刷新拍（面板可见期间的 1s repeating timer）。
        #[unsafe(method(ninjaPanelRefresh:))]
        fn ninja_panel_refresh(&self, _sender: Option<&AnyObject>) {
            self.refresh();
        }


    }

    unsafe impl NSObjectProtocol for PluginPanel {}
);

static PANEL: std::sync::atomic::AtomicPtr<PluginPanel> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

fn panel() -> Option<&'static PluginPanel> {
    let p = PANEL.load(std::sync::atomic::Ordering::Acquire);
    (!p.is_null()).then(|| unsafe { &*p })
}

/// 开关的一条路（UI checkbox / E2E 钩子共用）：宿主生命周期 + 写回
/// ninja.toml + 面板刷新。
fn apply_toggle(name: &str, on: bool) {
    eprintln!(
        "ninja: 面板开关 {name:?} → {}",
        if on { "on" } else { "off" }
    );
    let ok = crate::plugins::toggle_plugin(name, on);
    if !ok && on {
        eprintln!("ninja: 插件 {name:?} 拉起失败（面板开关回弹）");
    }
    // 名单写回（会话真值）。
    let enabled = crate::plugins::session_cfg().enabled;
    if let Err(e) = crate::config::write_plugins_enabled(&enabled) {
        eprintln!("ninja: 写回 ninja.toml 失败：{e}");
    }
    if let Some(p) = panel() {
        p.refresh();
    }
}

/// toggle_visibility（⌘,/菜单）的入口：显示（建/刷新）或隐藏。
pub fn toggle_visibility() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let p = ensure_panel(mtm);
    let Some(w) = p.ivars().window.borrow().clone() else {
        return;
    };
    if w.isVisible() {
        w.orderOut(None);
        return;
    }
    p.rebuild_rows(mtm);
    p.refresh();
    // 不夺焦（附属工具窗）：终端保持 key（键盘继续落终端；E2E 的 zoom
    // 钩子按 keyWindow 找终端容器——面板夺焦会打断它，q2 回归实测）。
    w.orderFrontRegardless();
    p.schedule_refresh(mtm);
}

/// E2E 钩子（NINJA_PANEL_PLUGIN_FILE 的「<name> on|off」行）：与 UI
/// checkbox 同一条路径。
pub fn toggle_from_hook(name: &str, on: bool) {
    apply_toggle(name, on);
}

fn ensure_panel(mtm: MainThreadMarker) -> &'static PluginPanel {
    if let Some(p) = panel() {
        return p;
    }
    let this = PluginPanel::alloc(mtm).set_ivars(Ivars {
        window: RefCell::new(None),
        rows: RefCell::new(Vec::new()),
        refresh_scheduled: RefCell::new(false),
    });
    // SAFETY: super 的 init；ivars 已就位。
    let this: Retained<PluginPanel> = unsafe { msg_send![super(this), init] };
    let p = Box::leak(Box::new(this));
    let raw = &**p as *const PluginPanel as *mut PluginPanel;
    PANEL.store(raw, std::sync::atomic::Ordering::Release);

    // 窗口（无极小化；面板随用随显隐）。
    let style =
        NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable;
    let frame = NSRect::new(NSPoint::new(120.0, 480.0), NSSize::new(460.0, 240.0));
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
    window.setTitle(&NSString::from_str("Plugins"));
    // SAFETY: 布尔 setter。
    unsafe { window.setReleasedWhenClosed(false) };
    // 面板不抢终端焦点风格：普通窗即可（keep simple）。
    p.ivars().window.replace(Some(window));
    unsafe { &*raw }
}

impl PluginPanel {
    /// 按会话真值重建行集。
    fn rebuild_rows(&self, mtm: MainThreadMarker) {
        let Some(window) = self.ivars().window.borrow().clone() else {
            return;
        };
        let content = window.contentView().expect("面板内容视图");
        // 清旧行（子视图全摘——面板内容只有行视图）。
        // SAFETY: subviews 读拷贝；removeFromSuperview 平凡。
        for sub in content.subviews().iter() {
            sub.removeFromSuperview();
        }
        let statuses = crate::plugins::status_snapshot();
        let mut rows = Vec::new();
        let mut y = 8.0;
        for st in &statuses {
            let check = NSButton::new(mtm);
            check.setButtonType(NSButtonType::Switch);
            check.setTitle(&NSString::from_str(""));
            check.setState(if st.enabled { 1 } else { 0 });
            // target-action：开关动作落本控制器。
            // SAFETY: setTarget/setAction 平凡。
            // SAFETY: setTarget/setAction 弱引用 target（AppKit 惯例）。
            unsafe {
                check.setTarget(Some(self));
                check.setAction(Some(objc2::sel!(ninjaToggle:)));
            }
            check.setFrame(NSRect::new(
                NSPoint::new(12.0, y + 2.0),
                NSSize::new(20.0, 18.0),
            ));

            let name = label(mtm, &st.name, 40.0, y, 150.0, 18.0);
            let status = label(mtm, "", 196.0, y, 240.0, 18.0);
            status.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
            // content 持有子视图。
            content.addSubview(&check);
            content.addSubview(&name);
            content.addSubview(&status);
            rows.push(Row {
                name: st.name.clone(),
                check,
                status,
            });
            y += 26.0;
        }
        if rows.is_empty() {
            let hint = label(
                mtm,
                "无插件（ninja.toml [plugins] enabled 配置）",
                16.0,
                y,
                420.0,
                18.0,
            );
            hint.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
            content.addSubview(&hint);
        }
        // 按行数收窗高（空面板不占屏）。
        let height = (y + 16.0).max(90.0);
        let f = window.frame();
        window.setFrame_display(
            NSRect::new(f.origin, NSSize::new(f.size.width, height)),
            false,
        );
        *self.ivars().rows.borrow_mut() = rows;
    }

    /// 刷新状态列（pid+MB / 已停止(原因)）。
    fn refresh(&self) {
        let statuses = crate::plugins::status_snapshot();
        let mut rows = self.ivars().rows.borrow_mut();
        for row in rows.iter_mut() {
            let Some(st) = statuses.iter().find(|s| s.name == row.name) else {
                continue;
            };
            let text = match (st.running, st.pid, st.memory_bytes) {
                (true, Some(pid), Some(mb)) => format!("pid {pid} · {:.1} MB", mb as f64 / 1e6),
                (true, Some(pid), None) => format!("pid {pid}"),
                _ => match &st.last_error {
                    Some(e) => format!("已停止（{e}）"),
                    None => "已停止".to_string(),
                },
            };
            row.status.setStringValue(&NSString::from_str(&text));
            // 开关态与会话真值同步（外部禁用/死亡也反映）。
            let want: isize = if st.enabled { 1 } else { 0 };
            if row.check.state() != want {
                row.check.setState(want);
            }
        }
    }

    /// 可见期间的刷新拍（1s repeating；窗口隐藏即无效循环，开销可忽略）。
    fn schedule_refresh(&self, _mtm: MainThreadMarker) {
        if *self.ivars().refresh_scheduled.borrow() {
            return;
        }
        *self.ivars().refresh_scheduled.borrow_mut() = true;
        // SAFETY: -self 返回 retain 过的引用。
        let target: Retained<AnyObject> = unsafe { msg_send![self, self] };
        // SAFETY: scheduledTimer 平凡。
        let timer = unsafe {
            objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                1.0,
                &target,
                objc2::sel!(ninjaPanelRefresh:),
                None,
                true,
            )
        };
        std::mem::forget(timer); // 进程生命期常驻（refresh 轻量）
    }
}

fn label(
    mtm: MainThreadMarker,
    text: &str,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Retained<objc2_app_kit::NSTextField> {
    let label = objc2_app_kit::NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)));
    label.setEditable(false);
    label.setSelectable(false);
    label.setBezeled(false);
    label.setDrawsBackground(false);
    let _ = mtm;
    label
}
