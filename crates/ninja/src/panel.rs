//! 设置窗（⌘, → `toggle_visibility` 动作）：左侧 tab 源列表 + 右侧内容
//! 区，现在只有一个 tab「Plugins」。
//!
//! 结构：NSTabView（left position，原生偏好设置窗样式）承载 tab 列表；
//! 唯一 tab 的内容 = 插件仪表读出（见下）。将来宿主自有设置项再加 tab
//! ——Ghostty 语义仍只属于 `~/.config/ghostty/config`（宿主不维护第二
//! 份终端配置面），这里永远只放 ninja 自有的东西。
//!
//! 插件页：每行 = 开关 / ● 状态点 / 名 / 等宽数字遥测（pid · MB）。
//! ● 绿=运行中、灰=未启用、橙=已停止（括号里带原因）——扫描代替读字；
//! 遥测用 monospacedDigit 字体，1s 刷新不抖。超过 8 行出滚动条。页脚 =
//! 插件目录路径 + 「打开…」（Finder）——安装位置的永久可发现性：丢文件
//! 即装，PRODUCT 语义不变。
//!
//! 开关即启停（[`crate::plugins::toggle_plugin`] 的「启用即拉起/禁用即
//! 回收」单一生命周期）+ 名单写回 ninja.toml（
//! [`crate::config::write_plugins_enabled`]）。行集 = 会话真值（enabled ∪
//! 在跑 ∪ 有错误记录）∪ 已安装发现（[`crate::plugins::discover_plugin_names`]）。
//! E2E 钩子 `NINJA_PANEL_PLUGIN_FILE`（app.rs 轮询「<name> on|off」行，
//! 与 UI 开关同一条路径，免 CGEvent）。面板不夺终端焦点。

#![allow(non_snake_case)] // ObjC selector 方法名

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSButtonType, NSColor, NSFont, NSScrollView, NSTabPosition,
    NSTabView, NSTabViewItem, NSTextField, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// 行高（含行距）。
const ROW_H: f64 = 28.0;
/// 页脚高（路径 + 打开按钮）。
const FOOTER_H: f64 = 44.0;
/// 设置窗总宽（tab 列 + 内容区；固定尺寸工具窗）。
const SETTINGS_W: f64 = 660.0;
/// 设置窗总高（固定：行少留白、行多滚动——不再贴内容长高）。
const SETTINGS_H: f64 = 460.0;
/// 内容区宽的假定值（tab 布局完成前的兜底；完成后按实际 frame 布局）。
const HOST_W: f64 = 486.0;

/// 一行 = 一个插件。
struct Row {
    name: String,
    check: Retained<NSButton>,
    dot: Retained<NSTextField>,
    status: Retained<NSTextField>,
}

pub struct Ivars {
    window: RefCell<Option<Retained<NSWindow>>>,
    /// tab 骨架（左 tab 列 + 内容区）。
    tab: RefCell<Option<Retained<NSTabView>>>,
    /// tab「Plugins」的内容宿主视图（滚动区 + 页脚都挂它下面）。
    host: RefCell<Option<Retained<NSView>>>,
    scroll: RefCell<Option<Retained<NSScrollView>>>,
    /// 页脚路径标签（ensure 时定死；刷新不动它）。
    footer_path: RefCell<Option<Retained<NSTextField>>>,
    rows: RefCell<Vec<Row>>,
    /// 行布局时的内容区宽（1s 拍里漂移 >1pt → 自愈重建）。
    last_w: Cell<f64>,
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

        /// 页脚「打开…」：确保目录存在 → Finder 打开。
        #[unsafe(method(ninjaOpenPluginsDir:))]
        fn ninja_open_plugins_dir(&self, _sender: Option<&AnyObject>) {
            let Some(dir) = crate::plugins::user_plugin_dir() else {
                return;
            };
            let _ = std::fs::create_dir_all(&dir);
            match std::process::Command::new("/usr/bin/open").arg(&dir).spawn() {
                Ok(_) => {}
                Err(e) => eprintln!("ninja: 打开插件目录 {dir:?} 失败：{e}"),
            }
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
    p.rebuild(mtm);
    // 不夺焦（附属工具窗）：终端保持 key（E2E 的 zoom 钩子按 keyWindow
    // 找终端容器——面板夺焦会打断它）。
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
        tab: RefCell::new(None),
        host: RefCell::new(None),
        scroll: RefCell::new(None),
        footer_path: RefCell::new(None),
        rows: RefCell::new(Vec::new()),
        last_w: Cell::new(0.0),
        refresh_scheduled: RefCell::new(false),
    });
    // SAFETY: super 的 init；ivars 已就位。
    let this: Retained<PluginPanel> = unsafe { msg_send![super(this), init] };
    let p = Box::leak(Box::new(this));
    let raw = &**p as *const PluginPanel as *mut PluginPanel;
    PANEL.store(raw, std::sync::atomic::Ordering::Release);

    // 窗口（固定尺寸设置窗；面板随用随显隐）。
    let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable;
    let frame = NSRect::new(NSPoint::new(120.0, 480.0), NSSize::new(SETTINGS_W, 240.0));
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
    window.setTitle(&NSString::from_str("Settings"));
    // SAFETY: 布尔 setter。
    unsafe { window.setReleasedWhenClosed(false) };
    p.ivars().window.replace(Some(window));
    let content = p.window_content();

    // 设置窗骨架：左 tab 源列表 + 右内容区（现在只有 Plugins 一页）。
    let tab = NSTabView::new(mtm);
    tab.setTabPosition(NSTabPosition::Left);
    let host = NSView::new(mtm);
    let item = NSTabViewItem::new();
    item.setLabel(&NSString::from_str("Plugins"));
    item.setView(Some(&host));
    tab.addTabViewItem(&item);
    // SAFETY: 选中首 tab 让 AppKit 当场铺内容区尺寸（rebuild 依赖）。
    unsafe { tab.selectFirstTabViewItem(None) };
    content.addSubview(&tab);

    // 插件页内容：滚动区（页脚之上占满）+ 页脚。
    let scroll = NSScrollView::new(mtm);
    scroll.setHasVerticalScroller(true);
    scroll.setHasHorizontalScroller(false);
    scroll.setAutohidesScrollers(true);
    host.addSubview(&scroll);
    p.ivars().scroll.replace(Some(scroll));
    build_footer(p, mtm, &host);

    p.ivars().tab.replace(Some(tab));
    p.ivars().host.replace(Some(host));
    unsafe { &*raw }
}

impl PluginPanel {
    fn window_content(&self) -> Retained<NSView> {
        self.ivars()
            .window
            .borrow()
            .as_ref()
            .and_then(|w| w.contentView())
            .expect("面板内容视图")
    }

    /// tab 内容区宽（布局未就绪时用假定值兜底；1s 拍自愈）。
    fn host_w(&self) -> f64 {
        let w = self
            .ivars()
            .host
            .borrow()
            .as_ref()
            .map(|h| h.frame().size.width)
            .unwrap_or(0.0);
        if w < 100.0 {
            HOST_W
        } else {
            w
        }
    }

    /// 按会话真值 + 已安装发现重建行集（每次打开重建；期间 1s 拍只刷
    /// 状态不重建）。
    fn rebuild(&self, mtm: MainThreadMarker) {
        let Some(scroll) = self.ivars().scroll.borrow().clone() else {
            return;
        };
        let statuses = crate::plugins::status_snapshot();
        let n = statuses.len();
        let w = self.host_w();

        // 固定窗尺寸 + 骨架（tab 占满内容、滚动区 = 内容高 - 页脚）。
        if let Some(win) = self.ivars().window.borrow().as_ref() {
            let f = win.frame();
            win.setFrame_display(
                NSRect::new(f.origin, NSSize::new(SETTINGS_W, SETTINGS_H)),
                false,
            );
        }
        self.layout_chrome(w);
        let area_h = self
            .ivars()
            .scroll
            .borrow()
            .as_ref()
            .map(|s| s.frame().size.height)
            .unwrap_or(SETTINGS_H - FOOTER_H);

        // 文档视图（行容器）：底朝上排（NSView 原点在左下）。行多时高于
        // 可视区 → 滚动；行少时窗口留白，不再收缩。
        let content_h = (n.max(1) as f64 * ROW_H + 12.0).max(area_h);
        let doc = NSView::new(mtm);
        doc.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(w - 16.0, content_h), // 16 = 预留滚动条
        ));

        let mut rows = Vec::new();
        if statuses.is_empty() {
            // 空态在大内容区里居中偏上，指向页脚目录。
            let hint = label(mtm, "无已安装插件", 16.0, content_h * 0.58, w - 48.0, 20.0);
            hint.setFont(Some(&NSFont::systemFontOfSize_weight(13.0, 0.0)));
            doc.addSubview(&hint);
            let sub = label(
                mtm,
                "把插件二进制放进下面的目录，回来开关启用",
                16.0,
                content_h * 0.58 - 24.0,
                w - 48.0,
                18.0,
            );
            sub.setTextColor(Some(&NSColor::secondaryLabelColor()));
            doc.addSubview(&sub);
        } else {
            // 非翻转坐标系（原点在左下）：行从顶部排——y = 高度 - (i+1) 行，
            // 行少时留白在下方（符合阅读习惯），行多时滚动。
            for (i, st) in statuses.iter().enumerate() {
                let y = content_h - (i as f64 + 1.0) * ROW_H + 4.0;
                let row = build_row(mtm, self, &doc, st, y, w);
                rows.push(row);
            }
        }

        scroll.setDocumentView(Some(&doc));
        *self.ivars().rows.borrow_mut() = rows;
        self.ivars().last_w.set(w);
    }

    /// 刷新状态列与状态点；内容区宽漂移（首帧布局迟到/字体度量变化）
    /// → 自愈重建一次。
    fn refresh(&self) {
        let w = self.host_w();
        if (w - self.ivars().last_w.get()).abs() > 1.0 {
            let Some(mtm) = MainThreadMarker::new() else {
                return;
            };
            self.rebuild(mtm);
            return;
        }
        let statuses = crate::plugins::status_snapshot();
        let mono = NSFont::monospacedDigitSystemFontOfSize_weight(12.0, 0.0);
        let mut rows = self.ivars().rows.borrow_mut();
        for row in rows.iter_mut() {
            let Some(st) = statuses.iter().find(|s| s.name == row.name) else {
                continue;
            };
            row.status
                .setStringValue(&NSString::from_str(&status_text(st)));
            row.status.setFont(Some(&mono));
            let (dot_color, text_color) = status_colors(st);
            row.dot.setTextColor(Some(&dot_color));
            row.status.setTextColor(Some(&text_color));
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

    /// 内容区骨架摆放：tab 占满窗口内容；滚动区 = 内容高 - 页脚。
    fn layout_chrome(&self, w: f64) {
        let cf = self.window_content().frame();
        if let Some(tab) = self.ivars().tab.borrow().as_ref() {
            tab.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), cf.size));
        }
        if let Some(scroll) = self.ivars().scroll.borrow().as_ref() {
            scroll.setFrame(NSRect::new(
                NSPoint::new(0.0, FOOTER_H),
                NSSize::new(w, (cf.size.height - FOOTER_H).max(ROW_H)),
            ));
        }
    }
}

/// 状态文本（右列）：运行中 = pid · MB（等宽数字刷新不抖）。
fn status_text(st: &crate::plugins::PluginStatus) -> String {
    match (st.running, st.pid, st.memory_bytes) {
        (true, Some(pid), Some(mb)) => format!("pid {pid} · {:.1} MB", mb as f64 / 1e6),
        (true, Some(pid), None) => format!("pid {pid}"),
        (true, None, _) => "运行中".to_string(),
        (false, _, _) => {
            if !st.enabled {
                "未启用".to_string()
            } else {
                match &st.last_error {
                    Some(e) => format!("已停止（{e}）"),
                    None => "已停止".to_string(),
                }
            }
        }
    }
}

/// 状态点色 + 状态文本色：绿=运行中、灰=未启用、橙=已停止（原因在文本里）。
fn status_colors(st: &crate::plugins::PluginStatus) -> (Retained<NSColor>, Retained<NSColor>) {
    if st.running {
        (NSColor::systemGreenColor(), NSColor::labelColor())
    } else if st.enabled {
        (NSColor::systemOrangeColor(), NSColor::secondaryLabelColor())
    } else {
        (
            NSColor::quaternaryLabelColor(),
            NSColor::secondaryLabelColor(),
        )
    }
}

/// 造一行（手动 frame，底朝上）：开关 | ● | 名 | 遥测（右侧）。
fn build_row(
    mtm: MainThreadMarker,
    panel: &PluginPanel,
    doc: &NSView,
    st: &crate::plugins::PluginStatus,
    y: f64,
    w: f64,
) -> Row {
    let check = NSButton::new(mtm);
    check.setButtonType(NSButtonType::Switch);
    check.setTitle(&NSString::from_str(""));
    check.setState(if st.enabled { 1 } else { 0 });
    // SAFETY: setTarget/setAction 弱引用 target（AppKit 惯例）。
    unsafe {
        check.setTarget(Some(panel));
        check.setAction(Some(objc2::sel!(ninjaToggle:)));
    }
    check.setFrame(NSRect::new(
        NSPoint::new(12.0, y + 4.0),
        NSSize::new(20.0, 18.0),
    ));

    // 状态点：● 带色文本（自适应深浅模式；比 layer 圆点省一整套 CALayer 舞步）。
    let dot = NSTextField::labelWithString(&NSString::from_str("●"), mtm);
    dot.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(
        12.0, 0.0,
    )));
    let (dot_color, text_color) = status_colors(st);
    dot.setTextColor(Some(&dot_color));
    dot.setFrame(NSRect::new(
        NSPoint::new(38.0, y + 5.0),
        NSSize::new(14.0, 18.0),
    ));

    let name = label(mtm, &st.name, 58.0, y + 5.0, w - 58.0 - 190.0, 18.0);
    name.setFont(Some(&NSFont::systemFontOfSize(12.5)));

    let status = label(mtm, &status_text(st), w - 182.0, y + 5.0, 170.0, 18.0);
    status.setTextColor(Some(&text_color));
    status.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(
        12.0, 0.0,
    )));

    doc.addSubview(&check);
    doc.addSubview(&dot);
    doc.addSubview(&name);
    doc.addSubview(&status);
    Row {
        name: st.name.clone(),
        check,
        dot,
        status,
    }
}

/// 页脚：分隔线 + 插件目录路径 + 「打开…」。安装位置常驻可发现。
fn build_footer(p: &PluginPanel, mtm: MainThreadMarker, host: &NSView) {
    // 分隔线（NSBox separator，自适应深浅模式）。
    let line = objc2_app_kit::NSBox::new(mtm);
    line.setBoxType(objc2_app_kit::NSBoxType::Separator);
    line.setBorderWidth(1.0);
    line.setFrame(NSRect::new(
        NSPoint::new(12.0, FOOTER_H - 1.0),
        NSSize::new(HOST_W - 24.0, 1.0),
    ));
    host.addSubview(&line);

    let dir_text = crate::plugins::user_plugin_dir()
        .map(|d| abbreviate_home(d.as_path()))
        .unwrap_or_else(|| "~/.config/ninja/plugins".to_string());
    let path = label(mtm, &dir_text, 14.0, 12.0, HOST_W - 130.0, 18.0);
    path.setTextColor(Some(&NSColor::secondaryLabelColor()));
    path.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(
        11.0, 0.0,
    )));
    host.addSubview(&path);
    *p.ivars().footer_path.borrow_mut() = Some(path);

    let open = NSButton::new(mtm);
    open.setTitle(&NSString::from_str("打开…"));
    // SAFETY: setTarget/setAction 同上。
    unsafe {
        open.setTarget(Some(p));
        open.setAction(Some(objc2::sel!(ninjaOpenPluginsDir:)));
    }
    open.setFrame(NSRect::new(
        NSPoint::new(HOST_W - 92.0, 8.0),
        NSSize::new(78.0, 26.0),
    ));
    host.addSubview(&open);
}

/// `~` 缩写家目录前缀（展示用）。
fn abbreviate_home(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().into_owned();
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        if let Some(rest) = s.strip_prefix(home.as_ref()) {
            return format!("~{rest}");
        }
    }
    s
}

fn label(
    mtm: MainThreadMarker,
    text: &str,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)));
    label.setEditable(false);
    label.setSelectable(false);
    label.setBezeled(false);
    label.setDrawsBackground(false);
    label
}
