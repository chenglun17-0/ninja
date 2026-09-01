//! 层注册表与层视图（placement × surface；主线程 AppKit）。

use super::classify::{code_to_key_name, modifiers_from_mods, overlay_rect};
use super::{ade_debug, host_close_layers_of_pane, take_dispatcher};

use std::sync::Mutex;

use ninja_protocol::{
    InputFocus, InputKey, InputMouse, InputScroll, LayerMsg, LayerOpen, LayerReady, Message,
    Modifier, MouseAction, MouseButton, Placement, Surface,
};

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class};
use objc2_core_foundation::{
    CFBoolean, CFData, CFDictionary, CFNumber, CFNumberType, CFRetained,
};
use objc2_core_graphics::{
    CGColorRenderingIntent, CGDataProvider, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo,
};
use objc2_app_kit::{NSEvent, NSView, NSWindow};
use objc2_foundation::{NSObject, NSPoint, NSRect, NSSize, NSString};
use objc2_io_surface::IOSurfaceRef;
use objc2_web_kit::{
    WKScriptMessage, WKScriptMessageHandler, WKUserContentController, WKWebView,
    WKWebViewConfiguration,
};

use crate::keymap;
use crate::surface::SurfaceHostView;

// ---------------------------------------------------------------------------
// 层注册表（q0 审计 #4 结构路线：宿主自持 NSView 之上叠 IOSurface 层）
// ---------------------------------------------------------------------------

/// 开层所需的几何（点击路径在主线程收集）。
pub struct LayerGeom {
    /// pane id（键盘路由 + 收层目标）。
    pub pane: u32,
    /// cell 尺寸（points）。
    pub cell_pt: (f64, f64),
    /// 视图尺寸（points）。
    pub view_pt: (f64, f64),
    /// 像素密度（backingScaleFactor；dpi = 72*scale 发给插件）。
    pub scale: f64,
    /// 宿主视图（层叠在其上；Retained 保活）。
    pub view: Retained<SurfaceHostView>,
}

struct LayerEntry {
    /// 协议层句柄（layer.ready 里发给插件的那个）。
    handle: u64,
    /// 拥有该层的 pane（tab 层用 0：不跟终端 pane 的 resize/Esc 走）。
    pane: u32,
    /// 像素层合成视图。html tab 为 None。
    view: Option<Retained<LayerView>>,
    /// 像素层共享 IOSurface。html tab 为 None。
    surface: Option<CFRetained<IOSurfaceRef>>,
    /// html 表面。
    web: Option<Retained<LayerWebView>>,
    /// 所属插件连接 id。
    conn: u64,
    /// tab 层持有 chrome 窗（关层时关窗；窗关时反手收层）。
    tab_window: Option<Retained<NSWindow>>,
    /// layer.open 的 id（resize 重发 layer.ready 时回同一条）。
    open_id: u64,
}

/// 层内容视图：layer-backed NSView，drawRect 把「最新一帧」CGImage 画满
/// bounds（AppKit 把绘制结果进 layer contents——比手设 contents 稳：
/// layer-backed 视图的 contents 由 AppKit 接管，手设会被清空，实测；
/// sublayer 挂 ghostty Metal 层的方案几何又不随 view 坐标系走，也弃）。
/// 帧图像来自 [`surface_to_image`]（present 时像素拷贝）。
#[allow(non_snake_case)]
pub struct LayerViewIvars {
    image: RefCell<Option<CFRetained<CGImage>>>,
    handle: Cell<u64>,
    conn: Cell<u64>,
    is_tab: Cell<bool>,
    presented: Cell<bool>,
}

define_class!(
    // SAFETY: NSView 子类化无强约束；drawRect/isFlipped 纯自算；ivars
    // 只经 RefCell 访问（主线程）。
    #[unsafe(super(objc2_app_kit::NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = LayerViewIvars]
    pub struct LayerView;

    impl LayerView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true // 与 SurfaceHostView 同系：左上原点
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            self.ivars().is_tab.get()
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let Some(nsctx) = objc2_app_kit::NSGraphicsContext::currentContext() else {
                return;
            };
            let ctx = nsctx.CGContext();
            let b = self.bounds();
            let rect = objc2_core_foundation::CGRect {
                origin: objc2_core_foundation::CGPoint::new(0.0, 0.0),
                size: objc2_core_foundation::CGSize::new(b.size.width, b.size.height),
            };
            objc2_core_graphics::CGContext::set_rgb_fill_color(
                Some(&ctx),
                40.0 / 255.0,
                44.0 / 255.0,
                52.0 / 255.0,
                1.0,
            );
            objc2_core_graphics::CGContext::fill_rect(Some(&ctx), rect);
            let Some(image) = self.ivars().image.borrow().clone() else {
                return;
            };
            // AppKit flipped 视图的原生 CG 仍是 y-up：先翻 CTM 再画 CGImage。
            objc2_core_graphics::CGContext::save_g_state(Some(&ctx));
            objc2_core_graphics::CGContext::translate_ctm(Some(&ctx), 0.0, rect.size.height);
            objc2_core_graphics::CGContext::scale_ctm(Some(&ctx), 1.0, -1.0);
            objc2_core_graphics::CGContext::draw_image(Some(&ctx), rect, Some(&image));
            objc2_core_graphics::CGContext::restore_g_state(Some(&ctx));
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            let ok: bool = unsafe { objc2::msg_send![super(self), becomeFirstResponder] };
            if ok {
                send_layer_focus(self.ivars().handle.get(), self.ivars().conn.get(), true);
            }
            ok
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            let ok: bool = unsafe { objc2::msg_send![super(self), resignFirstResponder] };
            if ok {
                send_layer_focus(self.ivars().handle.get(), self.ivars().conn.get(), false);
            }
            ok
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if !self.ivars().is_tab.get() {
                let _: () = unsafe { objc2::msg_send![super(self), keyDown: event] };
                return;
            }
            tab_key_down(self, event);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            send_layer_scroll(self, event);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            send_layer_mouse(self, event, MouseAction::Down);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            send_layer_mouse(self, event, MouseAction::Up);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            send_layer_mouse(self, event, MouseAction::Move);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            send_layer_mouse(self, event, MouseAction::Down);
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) {
            send_layer_mouse(self, event, MouseAction::Up);
        }

        #[unsafe(method(rightMouseDragged:))]
        fn right_mouse_dragged(&self, event: &NSEvent) {
            send_layer_mouse(self, event, MouseAction::Move);
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            send_layer_mouse(self, event, MouseAction::Down);
        }

        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, event: &NSEvent) {
            send_layer_mouse(self, event, MouseAction::Up);
        }

        #[unsafe(method(otherMouseDragged:))]
        fn other_mouse_dragged(&self, event: &NSEvent) {
            send_layer_mouse(self, event, MouseAction::Move);
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            let prev = self.frame().size;
            let _: () = unsafe { objc2::msg_send![super(self), setFrameSize: size] };
            if self.ivars().is_tab.get()
                && self.ivars().presented.get()
                && self.ivars().handle.get() != 0
                && !self.inLiveResize()
                && ((prev.width - size.width).abs() > 0.5 || (prev.height - size.height).abs() > 0.5)
            {
                rebuild_tab_layer(self.ivars().handle.get());
            }
        }

        #[unsafe(method(viewDidEndLiveResize))]
        fn view_did_end_live_resize(&self) {
            let _: () = unsafe { objc2::msg_send![super(self), viewDidEndLiveResize] };
            if self.ivars().is_tab.get()
                && self.ivars().presented.get()
                && self.ivars().handle.get() != 0
            {
                rebuild_tab_layer(self.ivars().handle.get());
            }
        }
    }
);

/// html 表面：WKWebView 加载插件 HTML。Esc / ⌘W 宿主关层（PRODUCT）。
/// 其它键留给 WebKit。JS 只能经 `webkit.messageHandlers.layer` 出站。
#[allow(non_snake_case)]
pub struct LayerWebIvars {
    handle: Cell<u64>,
    conn: Cell<u64>,
    handler: RefCell<Option<Retained<LayerMsgHandler>>>,
}

#[allow(non_snake_case)]
pub struct LayerMsgIvars {
    handle: Cell<u64>,
    conn: Cell<u64>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NinjaLayerMsgHandler"]
    #[thread_kind = MainThreadOnly]
    #[ivars = LayerMsgIvars]
    pub struct LayerMsgHandler;

    unsafe impl NSObjectProtocol for LayerMsgHandler {}

    unsafe impl WKScriptMessageHandler for LayerMsgHandler {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        unsafe fn user_content_controller_did_receive_script_message(
            &self,
            _controller: &WKUserContentController,
            message: &WKScriptMessage,
        ) {
            on_layer_script_message(self, message);
        }
    }
);

impl LayerMsgHandler {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = LayerMsgHandler::alloc(mtm).set_ivars(LayerMsgIvars {
            handle: Cell::new(0),
            conn: Cell::new(0),
        });
        unsafe { objc2::msg_send![super(this), init] }
    }
}

define_class!(
    #[unsafe(super(WKWebView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = LayerWebIvars]
    pub struct LayerWebView;

    impl LayerWebView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            let ok: bool = unsafe { objc2::msg_send![super(self), becomeFirstResponder] };
            if ok {
                send_layer_focus(self.ivars().handle.get(), self.ivars().conn.get(), true);
            }
            ok
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            let ok: bool = unsafe { objc2::msg_send![super(self), resignFirstResponder] };
            if ok {
                send_layer_focus(self.ivars().handle.get(), self.ivars().conn.get(), false);
            }
            ok
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let mods = keymap::mods_from_flags(event.modifierFlags().0 as u64);
            let proto = modifiers_from_mods(mods);
            let chars = event.characters().map(|c| c.to_string()).unwrap_or_default();
            let fallback = chars.chars().next();
            let key = code_to_key_name(event.keyCode(), fallback);
            if (key == "esc" && !proto.contains(&Modifier::Cmd))
                || (key == "w"
                    && proto.contains(&Modifier::Cmd)
                    && !proto.contains(&Modifier::Shift))
            {
                if let Some(w) = self.window() {
                    w.performClose(None);
                }
                return;
            }
            let _: () = unsafe { objc2::msg_send![super(self), keyDown: event] };
        }
    }
);

impl LayerWebView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let config = unsafe { WKWebViewConfiguration::new(mtm) };
        let handler = LayerMsgHandler::new(mtm);
        let ucc = unsafe { config.userContentController() };
        let proto = ProtocolObject::from_ref(&*handler);
        unsafe {
            ucc.addScriptMessageHandler_name(proto, &NSString::from_str("layer"));
        }
        let this = LayerWebView::alloc(mtm).set_ivars(LayerWebIvars {
            handle: Cell::new(0),
            conn: Cell::new(0),
            handler: RefCell::new(Some(handler)),
        });
        // SAFETY: WKWebView 指定初始化器；ivars 已就位。
        unsafe {
            objc2::msg_send![super(this), initWithFrame: frame, configuration: &*config]
        }
    }

    fn bind(&self, handle: u64, conn: u64) {
        self.ivars().handle.set(handle);
        self.ivars().conn.set(conn);
        if let Some(h) = self.ivars().handler.borrow().as_ref() {
            h.ivars().handle.set(handle);
            h.ivars().conn.set(conn);
        }
    }
}

/// 注册表本体（只在主线程碰；static 要求手工 Send）。
struct Registry {
    layers: Vec<LayerEntry>,
    next_handle: u64,
}

unsafe impl Send for Registry {}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    layers: Vec::new(),
    next_handle: 1,
});

/// present：IOSurface 像素 → CGImage（LayerView 的 drawRect 画进
/// layer-backed 内容）。
///
/// **适配器取舍（实测）**：`layer.contents = IOSurfaceRef` 与「sublayer
/// 挂 ghostty Metal 层」两条路都不稳（前者在本宿主的 layer-hosting 树
/// 不渲染——宿主直写不透明像素也不显示；后者几何不随 view 坐标系走）；
/// v0 走 **present 拷贝**——lock → 拷出 BGRA 字节 →
/// CGDataProvider(CFData) → CGImage(PremultipliedFirst|ByteOrder32Little)
/// → LayerView 重画。每帧一次 CPU 拷贝（616x220x4 ≈ 0.5MiB），公开
/// API，稳定可见；协议面不变（插件仍写 IOSurface）。
fn surface_to_image(surface: &CFRetained<IOSurfaceRef>) -> Option<CFRetained<CGImage>> {
    let w = surface.width();
    let h = surface.height();
    if w == 0 || h == 0 {
        return None;
    }
    let bpr = surface.bytes_per_row();
    // SAFETY: read/write 锁成对；base_address 在锁内有效。
    let kr = unsafe { surface.lock(objc2_io_surface::IOSurfaceLockOptions::empty(), std::ptr::null_mut()) };
    if kr != 0 {
        return None;
    }
    // SAFETY: 锁内读 w*h*bpr 的共享内存（越界防护按 bpr×h 拷贝）。
    let bytes: Vec<u8> = unsafe {
        std::slice::from_raw_parts(surface.base_address().as_ptr() as *const u8, bpr * h).to_vec()
    };
    // SAFETY: 与 lock 成对。
    let _ = unsafe { surface.unlock(objc2_io_surface::IOSurfaceLockOptions::empty(), std::ptr::null_mut()) };

    let data = CFData::from_bytes(&bytes);
    let provider = CGDataProvider::with_cf_data(Some(&data))?;
    let space = objc2_core_graphics::CGColorSpace::new_device_rgb()?;
    let info = objc2_core_graphics::CGBitmapInfo(
        CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0,
    );
    // SAFETY: 参数与位图布局匹配（BGRA 预乘 32bpp）。
    unsafe {
        CGImage::new(
            w,
            h,
            8,
            32,
            bpr,
            Some(&space),
            info,
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentPerceptual,
        )
    }
}

/// i64 → CFNumber（kIOSurface* 属性字典值用）。
fn cf_i64(v: i64) -> Option<CFRetained<CFNumber>> {
    // SAFETY: 值指针指向合法 i64；SInt64Type 与之匹配。
    unsafe { CFNumber::new(None::<&objc2_core_foundation::CFAllocator>, CFNumberType::SInt64Type, (&raw const v).cast()) }
}

/// 开层：几何（overlay_rect）→ 全局 IOSurface（BGRA8，跨进程共享）→
/// 宿主自持 LayerView（layer-backed，drawRect 画最新帧）叠在
/// SurfaceHostView 之上（ghostty 的 Metal 层是同一 view 的 layer，
/// q0 审计 #4：宿主 subview 天然在终端之上）。返回发给插件的
/// `layer.ready`。同一 pane 至多一层（重复 open 先收旧层）。
pub(crate) fn layer_open(geom: &LayerGeom, m: &LayerOpen, conn: u64) -> Option<LayerReady> {
    match (m.placement, m.surface) {
        (Placement::Tab, Surface::Html) => layer_open_tab_html(geom, m, conn),
        (Placement::Tab, Surface::Pixels) => layer_open_tab_pixels(geom, m, conn),
        (Placement::Overlay | Placement::Side, Surface::Pixels) => {
            layer_open_overlay(geom, m, conn)
        }
        (Placement::Overlay | Placement::Side, Surface::Html) => {
            eprintln!("ninja: overlay/side 不接 html 表面");
            None
        }
    }
}

fn layer_open_overlay(geom: &LayerGeom, m: &LayerOpen, conn: u64) -> Option<LayerReady> {
    let mtm = MainThreadMarker::new()?;
    // 挂窗视图才有意义（无窗 = 无合成面）。
    let _window = geom.view.window()?;

    // 同 pane 先收旧层（v0 简化：一次一层）。
    host_close_layers_of_pane(geom.pane);

    let rect = match m.placement {
        Placement::Overlay => overlay_rect(m.anchor_row, m.anchor_col, geom.cell_pt, geom.view_pt),
        Placement::Side => overlay_rect(m.anchor_row, 0, geom.cell_pt, geom.view_pt),
        Placement::Tab => unreachable!("tab 走 layer_open_tab"),
    };
    let w_pt = rect.2.round().max(64.0);
    let h_pt = rect.3.round().max(64.0);
    let w_px = (w_pt * geom.scale).round().max(64.0) as u32;
    let h_px = (h_pt * geom.scale).round().max(64.0) as u32;

    // IOSurface：BGRA8Unorm（与插件侧 CGContext 位图布局一致）。
    // kIOSurfaceIsGlobal=true：跨进程按 global id 共享（协议 layer.ready
    // 的 io_surface_id；插件 IOSurfaceLookup 靠它）。
    let dict = unsafe {
        let keys: [*const std::ffi::c_void; 5] = [
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceWidth).cast(),
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceHeight).cast(),
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceBytesPerElement).cast(),
            std::ptr::from_ref(objc2_io_surface::kIOSurfacePixelFormat).cast(),
            #[allow(deprecated)] // 跨进程按 global id 共享是 v0 协议钉死机制
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceIsGlobal).cast(),
        ];
        let w_v = cf_i64(w_px as i64)?;
        let h_v = cf_i64(h_px as i64)?;
        let bpe = cf_i64(4)?;
        // 'BGRA' fourcc（big-endian 字节序常量）。
        let fmt = cf_i64(0x4247_5241_i64)?;
        // isGlobal 接 CFBoolean（不是 0/1 数字）。
        let global = CFBoolean::new(true);
        let values: [*const std::ffi::c_void; 5] = [
            std::ptr::from_ref(&*w_v).cast(),
            std::ptr::from_ref(&*h_v).cast(),
            std::ptr::from_ref(&*bpe).cast(),
            std::ptr::from_ref(&*fmt).cast(),
            std::ptr::from_ref(global).cast(),
        ];
        let mut keys_mut = keys;
        let mut values_mut = values;
        CFDictionary::new(
            None,
            keys_mut.as_mut_ptr(),
            values_mut.as_mut_ptr(),
            5,
            &objc2_core_foundation::kCFTypeDictionaryKeyCallBacks,
            &objc2_core_foundation::kCFTypeDictionaryValueCallBacks,
        )?
    };
    // SAFETY: 字典键值类型匹配（kIOSurface* 键全部接 CF 类型）。
    let surface = unsafe { IOSurfaceRef::new(&dict) }?;
    let surface_id = surface.id() as u64;

    // 合成视图：frame 在（翻转的）父视图坐标系 = 左上原点直配。
    let view = LayerView::new(mtm);
    view.setWantsLayer(true);
    view.setFrame(NSRect::new(
        NSPoint::new(rect.0, rect.1),
        NSSize::new(w_pt, h_pt),
    ));
    geom.view.addSubview(&view); // 父 view 持有 subview

    let mut reg = REGISTRY.lock().ok()?;
    let handle = reg.next_handle;
    reg.next_handle += 1;
    view.ivars().handle.set(handle);
    view.ivars().conn.set(conn);
    view.ivars().is_tab.set(false);
    reg.layers.push(LayerEntry {
        handle,
        pane: geom.pane,
        view: Some(view),
        surface: Some(surface),
        web: None,
        conn,
        tab_window: None,
        open_id: m.id,
    });
    drop(reg);
    ade_debug(&format!(
        "layer.open overlay → ready handle={handle} {w_px}x{h_px}px dpi={} iosurface={surface_id}",
        (72.0 * geom.scale) as u32
    ));
    Some(LayerReady::new(
        m.id,
        handle,
        w_px,
        h_px,
        (72.0 * geom.scale) as u32,
        surface_id,
    ))
}

/// 预览标签不挂终端 pane（避免终端 resize/Esc 误收）。
const TAB_PANE: u32 = u32::MAX;

thread_local! {
    static CLOSING_LAYER_TAB: Cell<bool> = const { Cell::new(false) };
}

fn new_global_iosurface(w_px: u32, h_px: u32) -> Option<CFRetained<IOSurfaceRef>> {
    let dict = unsafe {
        let keys: [*const std::ffi::c_void; 5] = [
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceWidth).cast(),
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceHeight).cast(),
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceBytesPerElement).cast(),
            std::ptr::from_ref(objc2_io_surface::kIOSurfacePixelFormat).cast(),
            #[allow(deprecated)]
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceIsGlobal).cast(),
        ];
        let w_v = cf_i64(w_px as i64)?;
        let h_v = cf_i64(h_px as i64)?;
        let bpe = cf_i64(4)?;
        let fmt = cf_i64(0x4247_5241_i64)?;
        let global = CFBoolean::new(true);
        let values: [*const std::ffi::c_void; 5] = [
            std::ptr::from_ref(&*w_v).cast(),
            std::ptr::from_ref(&*h_v).cast(),
            std::ptr::from_ref(&*bpe).cast(),
            std::ptr::from_ref(&*fmt).cast(),
            std::ptr::from_ref(global).cast(),
        ];
        let mut keys_mut = keys;
        let mut values_mut = values;
        CFDictionary::new(
            None,
            keys_mut.as_mut_ptr(),
            values_mut.as_mut_ptr(),
            5,
            &objc2_core_foundation::kCFTypeDictionaryKeyCallBacks,
            &objc2_core_foundation::kCFTypeDictionaryValueCallBacks,
        )?
    };
    unsafe { IOSurfaceRef::new(&dict) }
}

fn tab_title(m: &LayerOpen) -> &str {
    if m.title.is_empty() {
        "Tab"
    } else {
        m.title.as_str()
    }
}

fn tab_content_size(geom: &LayerGeom) -> NSSize {
    geom.view
        .window()
        .map(|w| w.contentRectForFrameRect(w.frame()).size)
        .unwrap_or(NSSize::new(800.0, 600.0))
}

fn layer_open_tab_html(geom: &LayerGeom, m: &LayerOpen, conn: u64) -> Option<LayerReady> {
    let mtm = MainThreadMarker::new()?;
    let parent = geom.view.window();
    let cs = tab_content_size(geom);
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), cs);
    let web = LayerWebView::new(mtm, frame);
    let window = crate::shell::new_chrome_tab(mtm, tab_title(m), web.as_super(), parent.as_deref());
    let scale = window.backingScaleFactor().max(1.0);
    let b = web.bounds();
    let w_pt = b.size.width.max(64.0);
    let h_pt = b.size.height.max(64.0);
    let w_px = (w_pt * scale).round().max(64.0) as u32;
    let h_px = (h_pt * scale).round().max(64.0) as u32;

    let mut reg = REGISTRY.lock().ok()?;
    let handle = reg.next_handle;
    reg.next_handle += 1;
    web.bind(handle, conn);
    let _ = window.makeFirstResponder(Some(web.as_super()));
    reg.layers.push(LayerEntry {
        handle,
        pane: TAB_PANE,
        view: None,
        surface: None,
        web: Some(web),
        conn,
        tab_window: Some(window),
        open_id: m.id,
    });
    drop(reg);
    eprintln!("ninja: layer tab handle={handle} {w_pt:.0}x{h_pt:.0}pt html");
    Some(LayerReady::new(
        m.id,
        handle,
        w_px,
        h_px,
        (72.0 * scale) as u32,
        0,
    ))
}

fn layer_open_tab_pixels(geom: &LayerGeom, m: &LayerOpen, conn: u64) -> Option<LayerReady> {
    let mtm = MainThreadMarker::new()?;
    let parent = geom.view.window();
    let cs = tab_content_size(geom);
    let view = LayerView::new(mtm);
    view.setWantsLayer(true);
    view.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), cs));
    view.ivars().is_tab.set(true);
    let window = crate::shell::new_chrome_tab(mtm, tab_title(m), view.as_super(), parent.as_deref());
    let scale = window.backingScaleFactor().max(1.0);
    let b = view.bounds();
    let w_pt = b.size.width.max(64.0);
    let h_pt = b.size.height.max(64.0);
    let w_px = (w_pt * scale).round().max(64.0) as u32;
    let h_px = (h_pt * scale).round().max(64.0) as u32;
    let surface = new_global_iosurface(w_px, h_px)?;
    let surface_id = surface.id() as u64;

    let mut reg = REGISTRY.lock().ok()?;
    let handle = reg.next_handle;
    reg.next_handle += 1;
    view.ivars().handle.set(handle);
    view.ivars().conn.set(conn);
    let _ = window.makeFirstResponder(Some(view.as_super()));
    reg.layers.push(LayerEntry {
        handle,
        pane: TAB_PANE,
        view: Some(view),
        surface: Some(surface),
        web: None,
        conn,
        tab_window: Some(window),
        open_id: m.id,
    });
    drop(reg);
    eprintln!("ninja: layer tab handle={handle} {w_px}x{h_px}px pixels iosurface={surface_id}");
    Some(LayerReady::new(
        m.id,
        handle,
        w_px,
        h_px,
        (72.0 * scale) as u32,
        surface_id,
    ))
}

fn tab_key_down(view: &LayerView, event: &NSEvent) {
    let mods = keymap::mods_from_flags(event.modifierFlags().0 as u64);
    let proto = modifiers_from_mods(mods);
    let chars = event.characters().map(|c| c.to_string()).unwrap_or_default();
    let fallback = chars.chars().next();
    let key = code_to_key_name(event.keyCode(), fallback);
    if (key == "esc" && !proto.contains(&Modifier::Cmd))
        || (key == "w" && proto.contains(&Modifier::Cmd) && !proto.contains(&Modifier::Shift))
    {
        if let Some(w) = view.window() {
            w.performClose(None);
        }
        return;
    }
    send_tab_key(view, key, chars, proto);
}

fn send_to_plugin(conn: u64, msg: &Message) {
    if let Some(host) = take_dispatcher()
        && let Ok(mut h) = host.lock()
    {
        let _ = h.send_message(conn, msg);
    }
}

fn send_tab_key(view: &LayerView, key: String, text: String, mods: Vec<Modifier>) {
    let handle = view.ivars().handle.get();
    let conn = view.ivars().conn.get();
    if handle == 0 {
        return;
    }
    send_to_plugin(conn, &Message::InputKey(InputKey::new(handle, key, text, mods)));
}

fn send_layer_focus(handle: u64, conn: u64, focused: bool) {
    if handle == 0 {
        return;
    }
    send_to_plugin(conn, &Message::InputFocus(InputFocus::new(handle, focused)));
}

fn layer_event_px(view: &NSView, event: &NSEvent) -> (u32, u32) {
    let loc = view.convertPoint_fromView(event.locationInWindow(), None);
    let scale = view
        .window()
        .map(|w| w.backingScaleFactor())
        .unwrap_or(1.0)
        .max(1.0);
    let x = (loc.x * scale).round().max(0.0) as u32;
    let y = (loc.y * scale).round().max(0.0) as u32;
    (x, y)
}

fn mouse_button_of(event: &NSEvent) -> MouseButton {
    match event.buttonNumber() {
        1 => MouseButton::Right,
        2 => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

fn send_layer_mouse(view: &LayerView, event: &NSEvent, action: MouseAction) {
    let handle = view.ivars().handle.get();
    let conn = view.ivars().conn.get();
    if handle == 0 {
        return;
    }
    let (x_px, y_px) = layer_event_px(view.as_super(), event);
    let mods = modifiers_from_mods(keymap::mods_from_flags(event.modifierFlags().0 as u64));
    send_to_plugin(
        conn,
        &Message::InputMouse(InputMouse::new(
            handle,
            mouse_button_of(event),
            action,
            x_px,
            y_px,
            mods,
        )),
    );
}

fn send_layer_scroll(view: &LayerView, event: &NSEvent) {
    let handle = view.ivars().handle.get();
    let conn = view.ivars().conn.get();
    if handle == 0 {
        return;
    }
    let dx = event.scrollingDeltaX().round() as i32;
    let dy = event.scrollingDeltaY().round() as i32;
    if dx == 0 && dy == 0 {
        return;
    }
    let mods = modifiers_from_mods(keymap::mods_from_flags(event.modifierFlags().0 as u64));
    send_to_plugin(
        conn,
        &Message::InputScroll(InputScroll::new(handle, dx, dy, mods)),
    );
}

fn rebuild_tab_layer(handle: u64) {
    let Ok(mut reg) = REGISTRY.lock() else {
        return;
    };
    let Some(e) = reg.layers.iter_mut().find(|e| e.handle == handle) else {
        return;
    };
    let Some(view) = e.view.as_ref() else {
        return; // html 表面跟窗口尺寸，不必重建
    };
    let Some(window) = view.window() else {
        return;
    };
    let scale = window.backingScaleFactor().max(1.0);
    let b = view.bounds();
    let w_px = (b.size.width.max(64.0) * scale).round().max(64.0) as u32;
    let h_px = (b.size.height.max(64.0) * scale).round().max(64.0) as u32;
    let Some(surface) = new_global_iosurface(w_px, h_px) else {
        return;
    };
    let surface_id = surface.id() as u64;
    let open_id = e.open_id;
    let conn = e.conn;
    e.surface = Some(surface);
    drop(reg);
    send_to_plugin(
        conn,
        &Message::LayerReady(LayerReady::new(
            open_id,
            handle,
            w_px,
            h_px,
            (72.0 * scale) as u32,
            surface_id,
        )),
    );
}

pub(crate) fn layer_load_html(handle: u64, html: &str) {
    let Ok(reg) = REGISTRY.lock() else { return };
    let Some(e) = reg.layers.iter().find(|e| e.handle == handle) else {
        eprintln!("ninja: layer.html handle={handle} 无此层");
        return;
    };
    let Some(web) = e.web.as_ref() else {
        eprintln!("ninja: layer.html handle={handle} 不是 html 表面");
        return;
    };
    unsafe { web.loadHTMLString_baseURL(&NSString::from_str(html), None) };
    eprintln!("ninja: layer.html handle={handle} {} bytes", html.len());
}

pub(crate) fn layer_post_msg(handle: u64, name: &str, body: &str) {
    let Ok(reg) = REGISTRY.lock() else {
        return;
    };
    let Some(e) = reg.layers.iter().find(|e| e.handle == handle) else {
        return;
    };
    let Some(web) = e.web.as_ref() else {
        return; // 像素层：不透明邮箱不适用
    };
    let payload = serde_json::json!({ "name": name, "body": body });
    let js = format!(
        "(function(){{var d={payload};window.dispatchEvent(new CustomEvent('layer-msg',{{detail:d}}));}})();"
    );
    unsafe {
        web.evaluateJavaScript_completionHandler(&NSString::from_str(&js), None);
    }
}

fn on_layer_script_message(handler: &LayerMsgHandler, message: &WKScriptMessage) {
    let handle = handler.ivars().handle.get();
    let conn = handler.ivars().conn.get();
    if handle == 0 {
        return;
    }
    let raw = unsafe { message.body() };
    let (name, body) = parse_script_msg_body(&raw);
    send_to_plugin(conn, &Message::LayerMsg(LayerMsg::new(handle, name, body)));
}

fn parse_script_msg_body(obj: &AnyObject) -> (String, String) {
    if let Some(s) = obj.downcast_ref::<NSString>() {
        return (String::new(), s.to_string());
    }
    let key_name = NSString::from_str("name");
    let key_body = NSString::from_str("body");
    let name: Option<Retained<NSObject>> = unsafe { objc2::msg_send![obj, objectForKey: &*key_name] };
    let body: Option<Retained<NSObject>> = unsafe { objc2::msg_send![obj, objectForKey: &*key_body] };
    let name = name
        .and_then(|v| v.downcast_ref::<NSString>().map(|s| s.to_string()))
        .unwrap_or_default();
    let body = body
        .and_then(|v| v.downcast_ref::<NSString>().map(|s| s.to_string()))
        .unwrap_or_default();
    (name, body)
}

/// 插件层标签 windowWillClose：收层并通知插件（不再 performClose）。
pub fn layer_tab_closed(content: &NSView) {
    let handle = if content.class() == LayerWebView::class() {
        let view: &LayerWebView = unsafe { &*std::ptr::from_ref(content).cast() };
        view.ivars().handle.get()
    } else if content.class() == LayerView::class() {
        let view: &LayerView = unsafe { &*std::ptr::from_ref(content).cast() };
        view.ivars().handle.get()
    } else {
        return;
    };
    if handle == 0 {
        return;
    }
    CLOSING_LAYER_TAB.set(true);
    let conn = {
        let Ok(reg) = REGISTRY.lock() else {
            CLOSING_LAYER_TAB.set(false);
            return;
        };
        reg.layers
            .iter()
            .find(|e| e.handle == handle)
            .map(|e| e.conn)
    };
    layer_close(handle);
    CLOSING_LAYER_TAB.set(false);
    if let Some(conn) = conn
        && let Some(host) = take_dispatcher()
        && let Ok(mut h) = host.try_lock()
    {
        let _ = h.send_layer_close(conn, handle);
    }
}

/// present：插件画完本帧——重设 contents（nil→surface）强制 CA 重采样
/// 共享内存并重合成（同一 IOSurfaceRef 反复写，CA 不会自己发现变化）。
pub(crate) fn layer_present(handle: u64) {
    let Ok(reg) = REGISTRY.lock() else { return };
    let Some(e) = reg.layers.iter().find(|e| e.handle == handle) else {
        return;
    };
    let (Some(surface), Some(view)) = (e.surface.as_ref(), e.view.as_ref()) else {
        return;
    };
    let Some(image) = surface_to_image(surface) else {
        ade_debug(&format!("layer.present handle={handle}：像素拷贝失败"));
        return;
    };
    *view.ivars().image.borrow_mut() = Some(image);
    view.ivars().presented.set(true);
    view.setNeedsDisplay(true);
    eprintln!(
        "ninja: layer.present handle={handle} image={}x{} view={:.0}x{:.0}",
        surface.width(),
        surface.height(),
        view.bounds().size.width,
        view.bounds().size.height
    );
}

/// 摘一个层（返回 是否摘到）。
pub(crate) fn layer_close(handle: u64) -> bool {
    let Ok(mut reg) = REGISTRY.lock() else {
        return false;
    };
    let Some(pos) = reg.layers.iter().position(|e| e.handle == handle) else {
        return false;
    };
    let e = reg.layers.remove(pos);
    drop(reg);
    remove_overlay(&e);
    ade_debug(&format!("layer.close handle={handle}（已摘）"));
    true
}

/// 把合成视图从父视图摘掉（主线程）。
fn remove_overlay(e: &LayerEntry) {
    if let Some(view) = e.view.as_ref() {
        view.discard_frame();
    }
    if let Some(w) = &e.tab_window {
        if !CLOSING_LAYER_TAB.get() {
            w.performClose(None);
        }
    } else if let Some(view) = e.view.as_ref() {
        view.removeFromSuperview();
    }
}

impl LayerView {
    /// 收层时立即丢弃帧像素（CGImage）——不等 AppKit dealloc 视图。
    fn discard_frame(&self) {
        *self.ivars().image.borrow_mut() = None;
    }

    /// 建视图（ivars 先就位再 super init；无自定义 initWithFrame 需求）。
    fn new(mtm: objc2::MainThreadMarker) -> Retained<Self> {
        let this = LayerView::alloc(mtm).set_ivars(LayerViewIvars {
            image: RefCell::new(None),
            handle: Cell::new(0),
            conn: Cell::new(0),
            is_tab: Cell::new(false),
            presented: Cell::new(false),
        });
        // SAFETY: super 的 initWithFrame:；ivars 已就位（零尺寸——
        // 调用方随即 setFrame）。
        unsafe { objc2::msg_send![super(this), initWithFrame: NSRect::ZERO] }
    }
}

/// 收某连接的全部层（连接死亡）。返回是否收过。
pub(crate) fn layer_close_by_conn(conn: u64) -> bool {
    let Ok(mut reg) = REGISTRY.lock() else {
        return false;
    };
    let all: Vec<LayerEntry> = std::mem::take(&mut reg.layers);
    let mut removed = Vec::new();
    for e in all {
        if e.conn == conn {
            removed.push(e);
        } else {
            reg.layers.push(e);
        }
    }
    drop(reg);
    for e in &removed {
        remove_overlay(e);
    }
    !removed.is_empty()
}

/// 收某 pane 的全部层（pane 关闭 / resize / Esc 兜底）。返回 (handle, conn)
/// 列表（调用方负责通知插件 layer.close）。
pub(crate) fn layer_close_pane(pane: u32) -> Vec<(u64, u64)> {
    let Ok(mut reg) = REGISTRY.lock() else {
        return Vec::new();
    };
    let all: Vec<LayerEntry> = std::mem::take(&mut reg.layers);
    let mut removed = Vec::new();
    for e in all {
        if e.pane == pane {
            removed.push(e);
        } else {
            reg.layers.push(e);
        }
    }
    drop(reg);
    for e in &removed {
        remove_overlay(e);
    }
    removed.iter().map(|e| (e.handle, e.conn)).collect()
}

/// 收全部层（禁用/退出）。返回 (handle, conn) 列表。
pub(crate) fn layer_close_all() -> Vec<(u64, u64)> {
    let removed: Vec<LayerEntry> = REGISTRY
        .lock()
        .ok()
        .map(|mut reg| std::mem::take(&mut reg.layers))
        .unwrap_or_default();
    for e in &removed {
        remove_overlay(e);
    }
    removed.iter().map(|e| (e.handle, e.conn)).collect()
}

/// pane 是否有前台层（键盘先给插件）。返回 (layer, conn)。
pub(crate) fn layer_foreground(pane: u32) -> Option<(u64, u64)> {
    REGISTRY.lock().ok().and_then(|reg| {
        reg.layers
            .iter()
            .find(|e| e.pane == pane)
            .map(|e| (e.handle, e.conn))
    })
}
