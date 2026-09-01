//! 设置窗（⌘, → `toggle_visibility` 动作）：cmux 式偏好设置窗。
//!
//! 骨架（对照 cmux）：全高振动侧栏（深一档，SF Symbol 图标 + 条目行，
//! 现在只有 Plugins 一页，条目多了再加搜索）｜内容页 = 页头（标题 +
//! 一句描述 + 右上「打开配置」）+ 表单行（标签 + 右侧控件 + 下方灰字
//! 描述）+ 行间发丝分隔线（无卡片）。窗口透明标题栏、全高内容、固定
//! 尺寸。
//!
//! 插件页：行 = ● 状态点 + 名（semibold）+ 状态（等宽数字遥测 pid · MB，
//! 1s 刷新不抖）+ 右侧开关。● 绿=运行中、灰=未启用、橙=已停止（原因在
//! 描述里）。页脚 = 插件目录路径 + 「打开…」——丢文件即装的永久可发现。
//! 「打开配置」= 用默认文本编辑器打开 ninja.toml（宿主唯一的自有配置面；
//! Ghostty 语义只在 ~/.config/ghostty/config，永不出现在这里）。
//!
//! 开关即启停（[`crate::plugins::toggle_plugin`]）+ 名单写回 ninja.toml
//! （[`crate::config::write_plugins_enabled`]）。行集 = 会话真值 ∪ 已安装
//! 发现。E2E 钩子 `NINJA_PANEL_PLUGIN_FILE`（与 UI 开关同一条路径）。
//! 面板不夺终端焦点。

#![allow(non_snake_case)] // ObjC selector 方法名

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSButton, NSButtonType, NSColor, NSFont, NSImage,
    NSImageView, NSScrollView, NSTextField, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectView, NSWindow, NSWindowStyleMask,
    NSWindowTitleVisibility,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// 行高（双行行：名 + 状态描述）。
const ROW_H: f64 = 58.0;
/// 页脚高（路径 + 打开按钮）。
const FOOTER_H: f64 = 44.0;
/// 内容页头高（标题 + 描述 + 打开配置按钮）。
const HEADER_H: f64 = 96.0;
/// 侧栏宽（图标 + 条目 + 选中高亮）。
const SIDEBAR_W: f64 = 200.0;
/// 设置窗总宽（固定尺寸工具窗）。
const SETTINGS_W: f64 = 780.0;
/// 设置窗总高。
const SETTINGS_H: f64 = 540.0;
/// 内容区宽（SETTINGS_W - SIDEBAR_W）。
const CONTENT_W: f64 = SETTINGS_W - SIDEBAR_W;

/// 一行 = 一个插件。
struct Row {
    name: String,
    check: Retained<NSButton>,
    dot: Retained<NSTextField>,
    status: Retained<NSTextField>,
}

pub struct Ivars {
    window: RefCell<Option<Retained<NSWindow>>>,
    /// 内容页宿主（滚动区 + 页脚都挂它下面；表单行画在滚动文档里）。
    content: RefCell<Option<Retained<NSView>>>,
    scroll: RefCell<Option<Retained<NSScrollView>>>,
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

        /// 页脚「打开…」：确保插件目录存在 → Finder 打开。
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

        /// 页头「打开配置」：ninja.toml 不存在则写默认骨架 → 文本编辑器打开。
        #[unsafe(method(ninjaOpenHostConfig:))]
        fn ninja_open_host_config(&self, _sender: Option<&AnyObject>) {
            let path = crate::config::host_config_path();
            if !path.exists()
                && let Err(e) = std::fs::write(&path, "[plugins]\nenabled = []\n")
            {
                eprintln!("ninja: 写默认 ninja.toml 失败：{e}");
            }
            match std::process::Command::new("/usr/bin/open")
                .arg("-t")
                .arg(&path)
                .spawn()
            {
                Ok(_) => {}
                Err(e) => eprintln!("ninja: 打开 ninja.toml 失败：{e}"),
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
        content: RefCell::new(None),
        scroll: RefCell::new(None),
        rows: RefCell::new(Vec::new()),
        refresh_scheduled: RefCell::new(false),
    });
    // SAFETY: super 的 init；ivars 已就位。
    let this: Retained<PluginPanel> = unsafe { msg_send![super(this), init] };
    let p = Box::leak(Box::new(this));
    let raw = &**p as *const PluginPanel as *mut PluginPanel;
    PANEL.store(raw, std::sync::atomic::Ordering::Release);

    // 窗口：透明标题栏 + 全高内容（侧栏顶到窗口顶，cmux/System Settings
    // 同款），固定尺寸。
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::FullSizeContentView;
    let frame = NSRect::new(
        NSPoint::new(160.0, 320.0),
        NSSize::new(SETTINGS_W, SETTINGS_H),
    );
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
    // SAFETY: 布尔/枚举 setter。
    unsafe {
        window.setReleasedWhenClosed(false);
        window.setTitlebarAppearsTransparent(true);
        window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    }
    p.ivars().window.replace(Some(window));
    let win_content = p.window_content();

    // 侧栏：全高振动视图（深一档）+ 唯一条目「Plugins」（选中态常亮）。
    let sidebar = NSVisualEffectView::new(mtm);
    sidebar.setMaterial(NSVisualEffectMaterial::Sidebar);
    sidebar.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    sidebar.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(SIDEBAR_W, SETTINGS_H),
    ));
    win_content.addSubview(&sidebar);
    build_sidebar_entry(mtm, &sidebar);

    // 侧栏/内容分隔线。
    let divider = NSBox::new(mtm);
    divider.setBoxType(NSBoxType::Separator);
    divider.setBorderWidth(1.0);
    divider.setFrame(NSRect::new(
        NSPoint::new(SIDEBAR_W - 1.0, 0.0),
        NSSize::new(1.0, SETTINGS_H),
    ));
    win_content.addSubview(&divider);

    // 内容页宿主：侧栏右侧整块。
    let content = NSView::new(mtm);
    content.setFrame(NSRect::new(
        NSPoint::new(SIDEBAR_W, 0.0),
        NSSize::new(CONTENT_W, SETTINGS_H),
    ));
    win_content.addSubview(&content);
    p.ivars().content.replace(Some(content.clone()));

    build_page_header(p, mtm, &content);

    // 插件行区：页头之下、页脚之上，可滚动。
    let scroll = NSScrollView::new(mtm);
    scroll.setHasVerticalScroller(true);
    scroll.setHasHorizontalScroller(false);
    scroll.setAutohidesScrollers(true);
    scroll.setFrame(NSRect::new(
        NSPoint::new(0.0, FOOTER_H),
        NSSize::new(CONTENT_W, SETTINGS_H - FOOTER_H - HEADER_H),
    ));
    content.addSubview(&scroll);
    p.ivars().scroll.replace(Some(scroll));

    build_footer(p, mtm, &content);

    unsafe { &*raw }
}

/// 侧栏条目：选中高亮（accent 圆角底）+ SF Symbol + 标题。红绿灯在
/// 左上，条目从其下开始。
fn build_sidebar_entry(mtm: MainThreadMarker, sidebar: &NSView) {
    let row_h: f64 = 40.0;
    let y = SETTINGS_H - 52.0 - row_h; // 让出红绿灯区
    let row = NSView::new(mtm);
    row.setFrame(NSRect::new(
        NSPoint::new(12.0, y),
        NSSize::new(SIDEBAR_W - 24.0, row_h),
    ));

    // 选中底：accent 填充圆角框（唯一条目 = 恒选中）。
    let sel = NSBox::new(mtm);
    sel.setBoxType(NSBoxType::Custom);
    sel.setFillColor(&NSColor::controlAccentColor());
    sel.setCornerRadius(8.0);
    sel.setBorderWidth(0.0);
    sel.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(SIDEBAR_W - 24.0, row_h),
    ));
    row.addSubview(&sel);

    // SF Symbol（模板渲染 → 跟着 accent 变色）。
    let icon_x = 10.0;
    if let Some(img) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str("puzzlepiece.fill"),
        None,
    ) {
        img.setTemplate(true);
        let icon = NSImageView::new(mtm);
        icon.setImage(Some(&img));
        icon.setFrame(NSRect::new(
            NSPoint::new(icon_x, 8.0),
            NSSize::new(22.0, 22.0),
        ));
        row.addSubview(&icon);
    }
    let title = NSTextField::labelWithString(&NSString::from_str("Plugins"), mtm);
    title.setFont(Some(&NSFont::systemFontOfSize_weight(13.0, 0.5)));
    title.setTextColor(Some(&NSColor::whiteColor()));
    title.setFrame(NSRect::new(
        NSPoint::new(40.0, 11.0),
        NSSize::new(SIDEBAR_W - 24.0 - 44.0, 20.0),
    ));
    hide_label_chrome(&title);
    row.addSubview(&title);
    sidebar.addSubview(&row);
}

/// 内容页头：标题 + 一句描述 + 右上「打开配置」。
fn build_page_header(p: &PluginPanel, mtm: MainThreadMarker, content: &NSView) {
    let title = NSTextField::labelWithString(&NSString::from_str("Plugins"), mtm);
    title.setFont(Some(&NSFont::systemFontOfSize_weight(17.0, 0.7)));
    title.setFrame(NSRect::new(
        NSPoint::new(24.0, SETTINGS_H - 54.0),
        NSSize::new(220.0, 24.0),
    ));
    hide_label_chrome(&title);
    content.addSubview(&title);

    let desc = NSTextField::labelWithString(
        &NSString::from_str("已安装的插件：开关即启停；把插件二进制放进目录即安装。"),
        mtm,
    );
    desc.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    desc.setTextColor(Some(&NSColor::secondaryLabelColor()));
    desc.setFrame(NSRect::new(
        NSPoint::new(24.0, SETTINGS_H - 74.0),
        NSSize::new(CONTENT_W - 160.0, 16.0),
    ));
    hide_label_chrome(&desc);
    content.addSubview(&desc);

    let open_cfg = NSButton::new(mtm);
    open_cfg.setTitle(&NSString::from_str("打开配置…"));
    // SAFETY: setTarget/setAction 弱引用 target（AppKit 惯例）。
    unsafe {
        open_cfg.setTarget(Some(p));
        open_cfg.setAction(Some(objc2::sel!(ninjaOpenHostConfig:)));
    }
    open_cfg.setFrame(NSRect::new(
        NSPoint::new(CONTENT_W - 118.0, SETTINGS_H - 56.0),
        NSSize::new(94.0, 28.0),
    ));
    content.addSubview(&open_cfg);
}

/// 页脚：分隔线 + 插件目录路径 + 「打开…」。安装位置常驻可发现。
fn build_footer(p: &PluginPanel, mtm: MainThreadMarker, content: &NSView) {
    let line = NSBox::new(mtm);
    line.setBoxType(NSBoxType::Separator);
    line.setBorderWidth(1.0);
    line.setFrame(NSRect::new(
        NSPoint::new(0.0, FOOTER_H - 1.0),
        NSSize::new(CONTENT_W, 1.0),
    ));
    content.addSubview(&line);

    let dir_text = crate::plugins::user_plugin_dir()
        .map(|d| abbreviate_home(d.as_path()))
        .unwrap_or_else(|| "~/.config/ninja/plugins".to_string());
    let path = NSTextField::labelWithString(&NSString::from_str(&dir_text), mtm);
    path.setTextColor(Some(&NSColor::secondaryLabelColor()));
    path.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(
        11.0, 0.0,
    )));
    path.setFrame(NSRect::new(
        NSPoint::new(24.0, 13.0),
        NSSize::new(CONTENT_W - 150.0, 18.0),
    ));
    hide_label_chrome(&path);
    content.addSubview(&path);

    let open = NSButton::new(mtm);
    open.setTitle(&NSString::from_str("打开…"));
    // SAFETY: setTarget/setAction 同上。
    unsafe {
        open.setTarget(Some(p));
        open.setAction(Some(objc2::sel!(ninjaOpenPluginsDir:)));
    }
    open.setFrame(NSRect::new(
        NSPoint::new(CONTENT_W - 98.0, 9.0),
        NSSize::new(74.0, 26.0),
    ));
    content.addSubview(&open);
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

    /// 按会话真值 + 已安装发现重建行集（每次打开重建；期间 1s 拍只刷
    /// 状态不重建）。
    fn rebuild(&self, mtm: MainThreadMarker) {
        let Some(scroll) = self.ivars().scroll.borrow().clone() else {
            return;
        };
        let statuses = crate::plugins::status_snapshot();
        let n = statuses.len();
        let w = CONTENT_W;
        let area_h = SETTINGS_H - FOOTER_H - HEADER_H;

        // 文档视图（行容器）：非翻转坐标系 → 行从顶部排（y = 高 - (i+1)行）。
        let content_h = (n as f64 * ROW_H + 8.0).max(area_h);
        let doc = NSView::new(mtm);
        doc.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(w - 16.0, content_h), // 16 = 预留滚动条
        ));

        let mut rows = Vec::new();
        if statuses.is_empty() {
            let hint = NSTextField::labelWithString(&NSString::from_str("无已安装插件"), mtm);
            hint.setFont(Some(&NSFont::systemFontOfSize_weight(13.0, 0.5)));
            hint.setTextColor(Some(&NSColor::secondaryLabelColor()));
            hint.setFrame(NSRect::new(
                NSPoint::new(24.0, area_h * 0.6),
                NSSize::new(w - 48.0, 20.0),
            ));
            hide_label_chrome(&hint);
            doc.addSubview(&hint);
            let sub = NSTextField::labelWithString(
                &NSString::from_str("把插件二进制放进下面的目录，回来开关启用"),
                mtm,
            );
            sub.setFont(Some(&NSFont::systemFontOfSize(11.0)));
            sub.setTextColor(Some(&NSColor::tertiaryLabelColor()));
            sub.setFrame(NSRect::new(
                NSPoint::new(24.0, area_h * 0.6 - 22.0),
                NSSize::new(w - 48.0, 16.0),
            ));
            hide_label_chrome(&sub);
            doc.addSubview(&sub);
        } else {
            for (i, st) in statuses.iter().enumerate() {
                let top = content_h - (i as f64) * ROW_H;
                let row = build_row(mtm, self, &doc, st, top, i + 1 == n);
                rows.push(row);
            }
        }

        scroll.setDocumentView(Some(&doc));
        *self.ivars().rows.borrow_mut() = rows;
    }

    /// 刷新状态行与状态点（不重建行——名字/开关目标不动）。
    fn refresh(&self) {
        let statuses = crate::plugins::status_snapshot();
        let mono = NSFont::monospacedDigitSystemFontOfSize_weight(11.0, 0.0);
        let mut rows = self.ivars().rows.borrow_mut();
        for row in rows.iter_mut() {
            let Some(st) = statuses.iter().find(|s| s.name == row.name) else {
                continue;
            };
            row.status
                .setStringValue(&NSString::from_str(&status_text(st)));
            row.status.setFont(Some(&mono));
            let (dot_color, _text_color) = status_colors(st);
            row.dot.setTextColor(Some(&dot_color));
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

/// 造一行（cmux 表单行）：● + 名（semibold）+ 状态描述（下方灰字）+
/// 右侧开关；行底发丝线（末行省）。非翻转坐标系，top = 行顶 y。
fn build_row(
    mtm: MainThreadMarker,
    panel: &PluginPanel,
    doc: &NSView,
    st: &crate::plugins::PluginStatus,
    top: f64,
    last: bool,
) -> Row {
    let w = CONTENT_W - 16.0;
    let (dot_color, _) = status_colors(st);

    // ● 状态点：带色文本（自适应深浅）。
    let dot = NSTextField::labelWithString(&NSString::from_str("●"), mtm);
    dot.setFont(Some(&NSFont::systemFontOfSize_weight(10.0, 0.6)));
    dot.setTextColor(Some(&dot_color));
    dot.setFrame(NSRect::new(
        NSPoint::new(24.0, top - 30.0),
        NSSize::new(14.0, 16.0),
    ));
    hide_label_chrome(&dot);
    doc.addSubview(&dot);

    // 名（semibold）+ 状态描述（下方，等宽数字遥测）。
    let name = NSTextField::labelWithString(&NSString::from_str(&st.name), mtm);
    name.setFont(Some(&NSFont::systemFontOfSize_weight(13.0, 0.5)));
    name.setFrame(NSRect::new(
        NSPoint::new(42.0, top - 32.0),
        NSSize::new(w - 42.0 - 90.0, 18.0),
    ));
    hide_label_chrome(&name);
    doc.addSubview(&name);

    let status = NSTextField::labelWithString(&NSString::from_str(&status_text(st)), mtm);
    status.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(
        11.0, 0.0,
    )));
    status.setTextColor(Some(&NSColor::secondaryLabelColor()));
    status.setFrame(NSRect::new(
        NSPoint::new(42.0, top - 50.0),
        NSSize::new(w - 42.0 - 90.0, 16.0),
    ));
    hide_label_chrome(&status);
    doc.addSubview(&status);

    // 右侧开关。
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
        NSPoint::new(w - 64.0, top - 42.0),
        NSSize::new(40.0, 18.0),
    ));
    doc.addSubview(&check);

    // 行底发丝线（末行省）。
    if !last {
        let sep = NSBox::new(mtm);
        sep.setBoxType(NSBoxType::Separator);
        sep.setBorderWidth(1.0);
        sep.setFrame(NSRect::new(
            NSPoint::new(24.0, top - ROW_H),
            NSSize::new(w - 48.0, 1.0),
        ));
        doc.addSubview(&sep);
    }

    Row {
        name: st.name.clone(),
        check,
        dot,
        status,
    }
}

/// 状态文本（行描述）：运行中 = pid · MB（等宽数字刷新不抖）。
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

/// 状态点色：绿=运行中、灰=未启用、橙=已停止（原因在描述里）。
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

/// labelWithString 造的 NSTextField 收拾成纯展示（无边框/背景/不可编辑）。
fn hide_label_chrome(label: &NSTextField) {
    label.setEditable(false);
    label.setSelectable(false);
    label.setBezeled(false);
    label.setDrawsBackground(false);
}
