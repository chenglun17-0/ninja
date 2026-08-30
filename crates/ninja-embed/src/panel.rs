//! q1 插件面板：App 菜单「Plugins…」（⌘,）入口的**空面板态**。
//!
//! 入口不变（v1 面板 v2 的开关路径），但 q1 空载零插件进程/零 socket：
//! 面板只显示占位说明（插件监督器与 ADE 协议接入是 q3 阶段；主题/配置
//! 系统是 q2）。打开 = 复用同一个窗口对象，关窗只藏不毁
//!（releasedWhenClosed=NO，v1 同款所有权纪律）。

#![allow(non_snake_case)] // ObjC selector 方法名

use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{NSObject, NSRect, NSString};

pub struct PanelIvars {
    window: Retained<NSWindow>,
}

define_class!(
    // SAFETY: NSObject 子类化无要求；只在主线程碰（MainThreadOnly）。
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = PanelIvars]
    pub struct PluginPanel;

    unsafe impl NSWindowDelegate for PluginPanel {}

    unsafe impl NSObjectProtocol for PluginPanel {}

    impl PluginPanel {
        /// 关闭按钮（显式出口；红绿灯同效）：target=窗口，action 内建。
        #[unsafe(method(ninjaPanelClose:))]
        fn close_action(&self, _sender: Option<&objc2::runtime::AnyObject>) {
            self.ivars().window.performClose(None);
        }
    }
);

/// 建空面板（一次性；AppDelegate 持有，重开复用 show）。
pub fn open(mtm: MainThreadMarker) -> Retained<PluginPanel> {
    let frame = NSRect::new(
        objc2_foundation::NSPoint::new(0.0, 0.0),
        objc2_foundation::NSSize::new(420.0, 150.0),
    );
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
    // SAFETY: 布尔 setter；面板对象是唯一 owner（v1 同款红线）。
    unsafe { window.setReleasedWhenClosed(false) };

    let this = PluginPanel::alloc(mtm).set_ivars(PanelIvars {
        window: window.clone(),
    });
    // SAFETY: super 的 init；ivars 已就位。
    let panel: Retained<PluginPanel> = unsafe { msg_send![super(this), init] };
    window.setDelegate(Some(ProtocolObject::from_ref(&*panel)));

    let content = window.contentView().expect("窗口必有 content");
    let label = objc2_app_kit::NSTextField::labelWithString(
        &NSString::from_str("没有已启用的插件。\nADE 插件接入在 q3 阶段重接；配置/主题在 q2。"),
        mtm,
    );
    label.setFrame(NSRect::new(
        objc2_foundation::NSPoint::new(16.0, 60.0),
        objc2_foundation::NSSize::new(388.0, 70.0),
    ));
    content.addSubview(&label);

    let panel_obj: &objc2::runtime::AnyObject = panel.as_super().as_super();
    // SAFETY: button 构造器；target/action 平凡。
    let close = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("关闭"),
            Some(panel_obj),
            Some(objc2::sel!(ninjaPanelClose:)),
            mtm,
        )
    };
    close.setFrame(NSRect::new(
        objc2_foundation::NSPoint::new(330.0, 16.0),
        objc2_foundation::NSSize::new(74.0, 28.0),
    ));
    content.addSubview(&close);
    panel
}

impl PluginPanel {
    /// 显示/前置（窗口常驻对象，重开复用）。
    pub fn show(&self) {
        self.ivars().window.center();
        self.ivars().window.makeKeyAndOrderFront(None);
    }
}
