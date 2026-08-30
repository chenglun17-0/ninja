//! p5 层原语（宿主侧）：插件经 `layer.open` 要层 → 宿主建 **IOSurface**
//!（跨进程共享像素，STACK.md 锁定：宿主建、插件写）→ 包成 Metal 纹理
//! 注册进本模块 → `layer.ready` 把尺寸/DPI/global id 告知插件 → 插件
//! 画完 `layer.present` → 渲染器在 cell pass 之上把纹理按矩形合成进
//! 同一个 CAMetalLayer drawable（合成路径钉死为 **Metal 侧**：
//! IOSurface→MTLTexture 单管线；AppKit 子层方案是 STACK 重开条件 2
//! 触发时的备选，不并行实现）。
//!
//! 生命周期与焦点：同一 pane 至多一个层（p5 简化）；宿主 Esc 兜底关层
//!（PRODUCT「任何插件层都能立刻关掉」），resize/pane 关闭也收层；
//! 层存在期间该 pane 的键盘先给插件（见 view 的 keyDown 分支）。
//!
//! 线程模型：注册表只在主线程碰（点击路径 / runloop pump timer /
//! 渲染）；`static Mutex` 只为满足 static 要求，内容包裹
//! [`Registry`] 的手工 `unsafe impl Send`（纹理句柄与裸 view 指针都
//! 不跨线程，纪律同 view 的 WakeInfo）。

use objc2::rc::Retained;
use objc2_core_foundation::{CFAllocator, CFNumber, CFNumberType, CFRetained};
use objc2_metal::{
    MTLDevice, MTLStorageMode, MTLTexture, MTLTextureDescriptor, MTLTextureType, MTLTextureUsage,
    MTLPixelFormat,
};

use crate::renderer::LayerDraw;
use ninja_protocol::LayerReady;

// ---------------------------------------------------------------------------
// 几何（纯函数，单测钉行为）
// ---------------------------------------------------------------------------

/// overlay 层矩形（drawable 设备像素，左上原点）：全宽，从锚点行往下
/// 至多半屏；锚点下方空间不足 1/4 屏时改向上开。宽高下限 64px 防退化。
pub fn overlay_rect(
    anchor_row: u32,
    anchor_col: u32,
    cell_px: (f64, f64),
    view_px: (f64, f64),
) -> (f64, f64, f64, f64) {
    let (cw, ch) = cell_px;
    let (vw, vh) = view_px;
    let ax = (f64::from(anchor_col) * cw).clamp(0.0, (vw - 64.0).max(0.0));
    let ay = f64::from(anchor_row) * ch;
    let half = (vh * 0.5).floor().max(64.0);
    let quarter = (vh * 0.25).floor();
    // 锚点下方放得下 1/4 屏 → 往下开（至多半屏）；否则向上开。
    let (y, h) = if vh - ay >= quarter {
        (ay, half.min((vh - ay).max(64.0)))
    } else {
        ((ay - half).max(0.0), half.min(ay.max(64.0)))
    };
    let w = (vw - ax).max(64.0);
    (ax, y, w, h)
}

// ---------------------------------------------------------------------------
// 注册表
// ---------------------------------------------------------------------------

/// 一个已打开的层。
pub struct LayerEntry {
    /// 协议层句柄（layer.ready 里发给插件的那个）。
    pub handle: u64,
    /// 拥有该层的 pane（键盘路由 + 重画目标）。
    pub pane: u32,
    /// 层矩形（drawable 设备像素，左上原点）。
    pub rect: (f64, f64, f64, f64),
    /// 像素尺寸（= rect 取整）。
    pub size_px: (u32, u32),
    /// 层纹理（IOSurface 包裹；渲染 pass 采样）。
    pub texture: Retained<objc2::runtime::ProtocolObject<dyn MTLTexture>>,
    /// 所属插件连接 id（input.key / layer.close 回程）。
    pub conn: u64,
    /// 已收到过 present。
    pub presented: bool,
    /// 拥有 pane 的 TerminalView 裸指针（主线程重画用；纪律同 WakeInfo：
    /// view shutdown 时必先经 close_pane 摘层，指针不悬空）。
    view: usize,
}

/// 注册表本体（主线程纪律；static 要求手工 Send）。
struct Registry {
    layers: Vec<LayerEntry>,
    next_handle: u64,
}

unsafe impl Send for Registry {}

static REGISTRY: std::sync::Mutex<Registry> = std::sync::Mutex::new(Registry {
    layers: Vec::new(),
    next_handle: 1,
});

/// 单测互斥：REGISTRY 是全局的，任何开层/断言注册表状态的测试先拿这
/// 把锁（本模块的空表断言与 plugins::tests 的层生命周期测试串行）。
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 层创建所需几何（view 的 cmd_click 在主线程收集；字段只在主线程用）。
pub struct LayerGeom {
    pub pane: u32,
    /// cell 尺寸（drawable 设备像素）。
    pub cell_px: (f64, f64),
    /// 视图尺寸（drawable 设备像素）。
    pub view_px: (f64, f64),
    /// 像素密度（backingScaleFactor；dpi = 72*scale 发给插件）。
    pub scale: f64,
    /// Metal 设备（纹理创建；view 的 renderer 所有）。
    pub device: Retained<objc2::runtime::ProtocolObject<dyn MTLDevice>>,
    /// TerminalView 裸指针（present 后重画；主线程纪律）。
    pub view: usize,
    /// 连接 id（输入路由回程）。
    pub conn: u64,
}

/// i64 → CFNumber（kIOSurface* 属性字典值用）。
fn cf_i64(v: i64) -> Option<CFRetained<CFNumber>> {
    // SAFETY: 值指针指向合法 i64；SInt64Type 与之匹配。
    unsafe { CFNumber::new(None::<&CFAllocator>, CFNumberType::SInt64Type, (&raw const v).cast()) }
}

/// 开层：建 IOSurface（BGRA8，系统选行对齐）→ MTLTexture → 注册。
/// 返回发给插件的 `layer.ready`（handle/尺寸/dpi/global id）。
pub fn open(geom: &LayerGeom, anchor_row: u32, anchor_col: u32) -> Option<LayerReady> {
    let rect = overlay_rect(anchor_row, anchor_col, geom.cell_px, geom.view_px);
    let w = rect.2.round().max(64.0) as u32;
    let h = rect.3.round().max(64.0) as u32;

    // IOSurface：BGRA8Unorm，与渲染器 drawable 像素格式一致。
    // kIOSurfaceIsGlobal=true：跨进程按 global id 共享（协议 layer.ready
    // 的 io_surface_id；插件 IOSurfaceLookup 靠它，否则查不到）。
    let dict = unsafe {
        let keys: [*const std::ffi::c_void; 5] = [
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceWidth).cast(),
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceHeight).cast(),
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceBytesPerElement).cast(),
            std::ptr::from_ref(objc2_io_surface::kIOSurfacePixelFormat).cast(),
            #[allow(deprecated)] // 跨进程按 global id 共享是 v0 协议钉死机制
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceIsGlobal).cast(),
        ];
        let w_v = cf_i64(w as i64)?;
        let h_v = cf_i64(h as i64)?;
        let bpe = cf_i64(4)?;
        // 'BGRA' fourcc（big-endian 字节序常量）。
        let fmt = cf_i64(0x4247_5241_i64)?;
        // isGlobal 接 CFBoolean（不是 0/1 数字）。
        let global = objc2_core_foundation::CFBoolean::new(true);
        let values: [*const std::ffi::c_void; 5] = [
            std::ptr::from_ref(&*w_v).cast(),
            std::ptr::from_ref(&*h_v).cast(),
            std::ptr::from_ref(&*bpe).cast(),
            std::ptr::from_ref(&*fmt).cast(),
            std::ptr::from_ref(global).cast(),
        ];
        let mut keys_mut = keys;
        let mut values_mut = values;
        objc2_core_foundation::CFDictionary::new(
            None,
            keys_mut.as_mut_ptr(),
            values_mut.as_mut_ptr(),
            5,
            &objc2_core_foundation::kCFTypeDictionaryKeyCallBacks,
            &objc2_core_foundation::kCFTypeDictionaryValueCallBacks,
        )?
    };
    // SAFETY: 字典键值类型匹配（kIOSurface* 键全部接 CFNumber）。
    let surface = unsafe { objc2_io_surface::IOSurfaceRef::new(&dict) }?;
    let surface_id = surface.id() as u64;

    // MTLTexture：包 IOSurface（插件进程往同一块共享内存写像素）。
    // newTextureWithDescriptor:iosurface:plane: 在 objc2-metal 里是
    // 安全方法（协议方法）。
    let tex_desc = MTLTextureDescriptor::new();
    tex_desc.setTextureType(MTLTextureType::Type2D);
    tex_desc.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
    // SAFETY: 尺寸在设备上限内。
    unsafe {
        tex_desc.setWidth(w as usize);
        tex_desc.setHeight(h as usize);
    }
    tex_desc.setStorageMode(MTLStorageMode::Shared);
    tex_desc.setUsage(MTLTextureUsage::ShaderRead);
    let texture = geom
        .device
        .newTextureWithDescriptor_iosurface_plane(&tex_desc, &surface, 0)?;

    let mut reg = REGISTRY.lock().ok()?;
    let handle = reg.next_handle;
    reg.next_handle += 1;
    // 同 pane 旧层直接替换（p5：一次一层）。
    reg.layers.retain(|l| l.pane != geom.pane);
    reg.layers.push(LayerEntry {
        handle,
        pane: geom.pane,
        rect,
        size_px: (w, h),
        texture,
        conn: geom.conn,
        presented: false,
        view: geom.view,
    });
    Some(LayerReady::new(
        0, // id 由调用方（plugins）填 layer.open 的回执
        handle,
        w,
        h,
        (geom.scale * 72.0).round() as u32,
        surface_id,
    ))
}

/// present：标记已呈现并让拥有 pane 的 view 重画（合成发生在渲染器）。
/// 返回 pane（无此层 = None）。注意：先放注册表锁再重画——
/// render_now 会经 draw_list 再锁同一把锁（std Mutex 不可重入，
/// 持锁回调 = 宿主主线程冻死）。
pub fn present(handle: u64) -> Option<u32> {
    let (view, pane) = {
        let mut reg = REGISTRY.lock().ok()?;
        let l = reg.layers.iter_mut().find(|l| l.handle == handle)?;
        l.presented = true;
        (l.view, l.pane)
    };
    repaint_view(view);
    Some(pane)
}

/// 摘层后对受影响 pane 重画（陈旧层纹理若不主动重画，会一直合成在
/// 最后一次渲染的 drawable 上——空闲终端没有下一帧，层就成了永久
/// 残留的「隐藏窗口」，p6 门禁）。`view==0`（单测注入的假几何，无宿主
/// 视图）跳过。只能在主线程调（纪律同 present）。
fn repaint_view(view: usize) {
    if view == 0 {
        return;
    }
    // SAFETY: view 指针由主线程注册（LayerGeom.view），本函数只在
    // 主线程调用；view shutdown 先经 close_pane 摘层，指针不悬空。
    let view: &crate::view::TerminalView = unsafe { &*(view as *const _) };
    view.layer_needs_display();
}

/// 注册表按谓词摘层的实现核心：摘层 + 删层探针 + **放锁后**重画
///（render_now → draw_list 会再锁同一把锁，std Mutex 不可重入——
/// 纪律同 present）。
fn close_where<F: Fn(&LayerEntry) -> bool>(pred: F) -> Vec<(u64, u64, u32)> {
    let (closed, views) = {
        let mut reg = match REGISTRY.lock() {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        let closed: Vec<(u64, u64, u32)> = reg
            .layers
            .iter()
            .filter(|l| pred(l))
            .map(|l| (l.handle, l.conn, l.pane))
            .collect();
        let views: Vec<usize> = reg
            .layers
            .iter()
            .filter(|l| pred(l))
            .map(|l| l.view)
            .collect();
        reg.layers.retain(|l| !pred(l));
        for (h, _, _) in &closed {
            remove_probe(*h);
        }
        (closed, views)
    };
    for v in views {
        repaint_view(v);
    }
    closed
}

/// 摘层（不通知插件——通知由 plugins 层补发 layer.close）。返回被摘的
/// (handle, conn, pane) 列表。p6 起对受影响 pane 重画（pump 路径上插件
/// 自发 layer.close 后不再有别的重画时机）。
pub fn close(handle: u64) -> Vec<(u64, u64, u32)> {
    close_where(|l| l.handle == handle)
}

/// 摘某插件连接拥有的全部层（**p6 监督器**：插件进程死亡/坏协议被断
/// 时，它的层就是无主陈旧 overlay，不摘会永久残留且 `any_layers()`
/// 恒真 → 泵 timer 永不停转）。语义同 [`close`]，返回被摘的
/// (handle, conn, pane) 列表；受影响 pane 重画。
pub fn close_by_conn(conn: u64) -> Vec<(u64, u64, u32)> {
    close_where(|l| l.conn == conn)
}

/// 摘全部层（同会话禁用 / 宿主退出：`PluginHost::shutdown` 用）。
/// 返回被摘列表（调用方按 conn 尽力通知还连着的插件 layer.close）。
pub fn close_all() -> Vec<(u64, u64, u32)> {
    close_where(|_| true)
}

/// 摘某 pane 的全部层（resize / pane 关闭路径）。返回同上。有意**不**
/// 在此重画：调用方（Esc → needs_render、resize → grid_changed）自带
/// 重画，而 shutdown 路径视图可能正在拆，主动重画不安全。
pub fn close_pane(pane: u32) -> Vec<(u64, u64, u32)> {
    let mut reg = match REGISTRY.lock() {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let closed: Vec<(u64, u64, u32)> = reg
        .layers
        .iter()
        .filter(|l| l.pane == pane)
        .map(|l| (l.handle, l.conn, l.pane))
        .collect();
    reg.layers.retain(|l| l.pane != pane);
    for (h, _, _) in &closed {
        remove_probe(*h);
    }
    closed
}

/// 取证钩子配套：层关闭时删 <NINJA_LAYER_PROBE>/<handle>.ppm（E2E
/// 用「文件消失」证关层；无 env 时 no-op）。
fn remove_probe(handle: u64) {
    if let Some(dir) = std::env::var_os("NINJA_LAYER_PROBE") {
        let _ = std::fs::remove_file(std::path::Path::new(&dir).join(format!("{handle}.ppm")));
    }
}

/// 某 pane 的前台层（键盘路由：层在 = 键盘先给插件）。
pub fn foreground(pane: u32) -> Option<(u64, u64)> {
    let reg = REGISTRY.lock().ok()?;
    reg.layers
        .iter()
        .find(|l| l.pane == pane)
        .map(|l| (l.handle, l.conn))
}

/// 是否还有任何层（pump timer 的启停依据）。
pub fn any_layers() -> bool {
    REGISTRY.lock().map(|r| !r.layers.is_empty()).unwrap_or(false)
}

/// 渲染数据快照：该 pane 的层纹理 + 矩形（draw 前主线程取）。
pub fn draw_list(pane: u32) -> Vec<LayerDraw> {
    let reg = match REGISTRY.lock() {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    reg.layers
        .iter()
        .filter(|l| l.pane == pane && l.presented)
        .map(|l| LayerDraw {
            handle: l.handle,
            texture: l.texture.clone(),
            rect: l.rect,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 单测（几何纯函数 + 注册表主线程行为）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_geometry() {
        // 80x24 cell、cell 16x32px、视图 1280x768。
        let cell = (16.0, 32.0);
        let view = (1280.0, 768.0);
        // 锚在第 2 行：往下开，至多半屏（384）。
        let (x, y, w, h) = overlay_rect(2, 0, cell, view);
        assert_eq!((x, y), (0.0, 64.0));
        assert_eq!(h, 384.0);
        assert_eq!(w, 1280.0);
        // 锚在末行（下方不足 1/4 屏）→ 向上开。
        let (_, y, _, h) = overlay_rect(23, 0, cell, view);
        assert_eq!(y, 736.0 - 384.0);
        assert_eq!(h, 384.0);
        // 锚列偏右：x 跟着锚走，宽度补到右边沿（下限 64）。
        let (x, _, w, _) = overlay_rect(0, 70, cell, view);
        assert_eq!(x, 1120.0);
        assert_eq!(w, 160.0);
    }

    #[test]
    fn registry_lifecycle() {
        // 与会开层的 plugins::tests 测试互斥（REGISTRY 全局）。
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // 无 Metal 设备（headless 单测）：直接验证注册表的摘除/查询语义
        // 不足以开层——open 需要 device，这里只测 close/foreground 在
        // 空表上的行为。真正的开层在 E2E（tests/layer_preview.rs）。
        assert!(close_pane(4242).is_empty());
        assert!(close(99).is_empty());
        // p6：按连接摘层在空表上同样是 no-op。
        assert!(close_by_conn(7).is_empty());
        assert!(close_all().is_empty());
        assert!(!any_layers());
        assert!(foreground(4242).is_none());
        assert!(draw_list(4242).is_empty());
    }
}
