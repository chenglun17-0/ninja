//! 自研 Metal cell 绘制（STACK.md：不吃 Skia/WebRender，直接吃 vt 的帧）。
//!
//! 一个管线：atlas 采样 × 顶点色。实心 quad（背景/选区/光标/下划线）用
//! atlas 里的 1x1 白块采样，字形 quad 用字形槽位采样，同一条 blend 路径。
//! 顶点走 setVertexBytes + vertex_id 解引用，无需 vertex descriptor。
//! 坐标系：设备像素，左上原点；viewport uniform 换算 NDC。

use libghostty_vt::render::{CursorVisualStyle, Dirty};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::{NSString};
use objc2_core_foundation::CGSize;
use objc2_metal::{
    MTLCommandEncoder, MTLBlendFactor, MTLBlitCommandEncoder, MTLClearColor, MTLCommandBuffer,
    MTLCommandQueue, MTLCompileOptions, MTLDevice, MTLLibrary, MTLPrimitiveType,
    MTLRenderCommandEncoder, MTLRenderPipelineDescriptor, MTLRenderPipelineState,
    MTLRenderPassDescriptor, MTLResourceOptions, MTLSamplerDescriptor, MTLSamplerMinMagFilter,
    MTLSamplerState, MTLStorageMode, MTLTexture, MTLTextureDescriptor, MTLTextureType,
    MTLTextureUsage, MTLCreateSystemDefaultDevice, MTLLoadAction, MTLStoreAction,
    MTLPixelFormat,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};

use crate::atlas::{GlyphAtlas, GlyphRect};
use crate::font::{Font, Weight};
use crate::term::{CellWideKind, Frame, Marked, Rgb};

/// 顶点：pos(px) + uv(atlas 归一化) + rgba，8 个 float。
#[repr(C)]
#[derive(Clone, Copy)]
struct Vertex {
    x: f32,
    y: f32,
    u: f32,
    v: f32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

const SHADER: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct VOut {
    float4 pos [[position]];
    float2 uv;
    float4 color;
};

vertex VOut cell_vs(
    uint vid [[vertex_id]],
    constant float *vertices [[buffer(0)]],   // 8 floats per vertex
    constant float2 &viewport [[buffer(1)]]
) {
    constant float *v = vertices + vid * 8;
    VOut out;
    float2 px = float2(v[0], v[1]);
    out.pos = float4(px.x / viewport.x * 2.0 - 1.0,
                     1.0 - px.y / viewport.y * 2.0,
                     0.0, 1.0);
    out.uv = float2(v[2], v[3]);
    out.color = float4(v[4], v[5], v[6], v[7]);
    return out;
}

fragment float4 cell_fs(
    VOut in [[stage_in]],
    texture2d<float> atlas [[texture(0)]],
    sampler smp [[sampler(0)]]
) {
    float cov = atlas.sample(smp, in.uv).r;
    return float4(in.color.rgb, in.color.a * cov);
}

// p5 层：插件画进 IOSurface 的 BGRA 直接采样（alpha 预乘，quad 顶点
// 色恒白）。uv 与 cell_vs 同一顶点布局，无断字形路径。
fragment float4 layer_fs(
    VOut in [[stage_in]],
    texture2d<float> content [[texture(0)]],
    sampler smp [[sampler(0)]]
) {
    return content.sample(smp, in.uv);
}
"#;

/// 渲染主题覆盖（T-主题/T2：**字段级** TOML 覆盖，不是主题系统）。
/// None = 跟随**当前生效色板**（[`crate::theme::current`]：内置 One
/// Dark Pro 基线，或插件 theme.set 覆盖——渲染读它而非编译期常量，
/// 换色板即生效）。选区 alpha 永远来自生效色板（ODP 基线 = 官方
/// `#abb2bf30` 的 0x30）。
pub struct Theme {
    /// `[theme] selection-bg` 覆盖；None = 生效色板的选区色。
    pub selection_bg: Option<Rgb>,
    /// `[theme] cursor` 覆盖；None = 生效色板的光标色。
    pub cursor: Option<Rgb>,
}

/// p5 层的渲染快照（layer::draw_list 产出）：纹理（IOSurface 包裹）
/// + drawable 像素矩形（左上原点）。
pub struct LayerDraw {
    pub handle: u64,
    pub texture: Retained<ProtocolObject<dyn MTLTexture>>,
    pub rect: (f64, f64, f64, f64),
}

impl Default for Theme {
    fn default() -> Self {
        // 空 = 全跟随生效色板（ODP 基线下的所见 = T1 行为不变）。
        Self {
            selection_bg: None,
            cursor: None,
        }
    }
}

/// 上次已呈现帧的视觉签名（D-C 跳帧判据）。vt 对纯光标移动（`\r`、
/// CSI 列定位）与 OSC 10/11 颜色变更不标脏（Dirty::Clean 但屏幕要变），
/// 所以光标/颜色必须进对比；尺寸/层存在性由宿主侧变化驱动，一并对比。
#[derive(Clone, Debug)]
struct LastPresent {
    drawable_size: (f64, f64),
    cols: u16,
    rows: u16,
    fg: Rgb,
    bg: Rgb,
    /// 已画出的光标（含闪烁抑制后的 None）；坐标 + 样式。
    cursor: Option<(u16, u16)>,
    cursor_style: CursorVisualStyle,
    /// IME 预编辑串（文本/选区/落点）。
    marked: Option<(String, usize, usize, u16, u16)>,
    /// 呈现时是否合成过层（层摘除后必须重画一次把层抹掉）。
    had_layers: bool,
}

pub struct Renderer {
    pub device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    /// p5 层管线（同一顶点看色器 + layer_fs 采样 BGRA）。
    layer_pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    sampler: Retained<ProtocolObject<dyn MTLSamplerState>>,
    atlas_texture: Retained<ProtocolObject<dyn MTLTexture>>,
    pub layer: Retained<CAMetalLayer>,
    pub theme: Theme,
    /// drawable 尺寸（设备像素），view resize 时更新。
    pub drawable_size: (f64, f64),
    /// cell 几何（设备像素）：宽、高、基线偏移。view resize 时更新。
    pub cell_px: (f64, f64, f64),
    /// 上传过的 atlas 版本号（白块只需传一次，reset 后要重传——
    /// 由 atlas pending 列表自动覆盖，这里只做统计观察）。
    pub frames_drawn: u64,
    /// 跳帧计数（D-C 取证：Clean 且视觉未变而不提交 drawable 的次数）。
    pub frames_skipped: u64,
    /// 顶点复用缓冲（跨帧保留容量；跳帧时不碰）。
    verts: Vec<Vertex>,
    /// 上次已呈现帧的视觉签名（None = 还没画成过任何帧）。
    last_present: Option<LastPresent>,
}

impl Renderer {
    pub fn new(
        layer: Retained<CAMetalLayer>,
        atlas_edge: u32,
        cell_px: (f64, f64, f64),
    ) -> Option<Self> {
        let device = MTLCreateSystemDefaultDevice()?;
        layer.setDevice(Some(&device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        layer.setMaximumDrawableCount(3);

        let queue = device.newCommandQueue()?;

        // 运行时编译着色器：构建链不加 metallc 步骤。
        let source = NSString::from_str(SHADER);
        let options = MTLCompileOptions::new();
        let library: Retained<ProtocolObject<dyn MTLLibrary>> = device
            .newLibraryWithSource_options_error(&source, Some(&options))
            .ok()?;
        let vs = library.newFunctionWithName(&NSString::from_str("cell_vs"))?;
        let fs = library.newFunctionWithName(&NSString::from_str("cell_fs"))?;
        let layer_fs_fn = library.newFunctionWithName(&NSString::from_str("layer_fs"))?;

        let pipeline_desc = MTLRenderPipelineDescriptor::new();
        pipeline_desc.setVertexFunction(Some(&vs));
        pipeline_desc.setFragmentFunction(Some(&fs));
        let color_attachment = unsafe {
            pipeline_desc
                .colorAttachments()
                .objectAtIndexedSubscript(0)
        };
        color_attachment.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        color_attachment.setBlendingEnabled(true);
        color_attachment.setSourceRGBBlendFactor(MTLBlendFactor::SourceAlpha);
        color_attachment.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        color_attachment.setSourceAlphaBlendFactor(MTLBlendFactor::One);
        color_attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        let pipeline = device
            .newRenderPipelineStateWithDescriptor_error(&pipeline_desc)
            .ok()?;

        // p5 层管线：同顶点/blend 配置，只换片段函数（BGRA 直采样）。
        let layer_desc = MTLRenderPipelineDescriptor::new();
        layer_desc.setVertexFunction(Some(&vs));
        layer_desc.setFragmentFunction(Some(&layer_fs_fn));
        let layer_attachment = unsafe {
            layer_desc
                .colorAttachments()
                .objectAtIndexedSubscript(0)
        };
        layer_attachment.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        layer_attachment.setBlendingEnabled(true);
        layer_attachment.setSourceRGBBlendFactor(MTLBlendFactor::SourceAlpha);
        layer_attachment.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        layer_attachment.setSourceAlphaBlendFactor(MTLBlendFactor::One);
        layer_attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        let layer_pipeline = device
            .newRenderPipelineStateWithDescriptor_error(&layer_desc)
            .ok()?;

        let sampler_desc = MTLSamplerDescriptor::new();
        sampler_desc.setMinFilter(MTLSamplerMinMagFilter::Nearest);
        sampler_desc.setMagFilter(MTLSamplerMinMagFilter::Nearest);
        let sampler = device.newSamplerStateWithDescriptor(&sampler_desc)?;

        // atlas 纹理：R8Unorm。
        let tex_desc = MTLTextureDescriptor::new();
        tex_desc.setTextureType(MTLTextureType::Type2D);
        tex_desc.setPixelFormat(MTLPixelFormat::R8Unorm);
        // SAFETY: 尺寸在设备上限内（2048）。
        unsafe {
            tex_desc.setWidth(atlas_edge as usize);
            tex_desc.setHeight(atlas_edge as usize);
        }
        tex_desc.setStorageMode(MTLStorageMode::Shared);
        tex_desc.setUsage(MTLTextureUsage::ShaderRead);
        let atlas_texture = device.newTextureWithDescriptor(&tex_desc)?;

        Some(Self {
            device,
            queue,
            pipeline,
            layer_pipeline,
            sampler,
            atlas_texture,
            layer,
            theme: Theme::default(),
            drawable_size: (0.0, 0.0),
            cell_px,
            frames_drawn: 0,
            frames_skipped: 0,
            verts: Vec::new(),
            last_present: None,
        })
    }
}

fn should_present_with(
    last: Option<&LastPresent>,
    drawable_size: (f64, f64),
    frame: &Frame,
    layers_nonempty: bool,
    atlas_has_pending: bool,
) -> bool {
    let Some(last) = last else {
        return true;
    };
    if frame.dirty != Dirty::Clean {
        return true;
    }
    if layers_nonempty || last.had_layers {
        return true;
    }
    if atlas_has_pending {
        return true;
    }
    if drawable_size != last.drawable_size
        || frame.cols != last.cols
        || frame.rows != last.rows
        || frame.fg != last.fg
        || frame.bg != last.bg
        || frame.cursor_style != last.cursor_style
    {
        return true;
    }
    let cursor = frame.cursor.map(|c| (c.x, c.y));
    if cursor != last.cursor {
        return true;
    }
    match (&frame.marked, &last.marked) {
        (None, None) => {}
        (Some(m), Some(l)) => {
            if m.text != l.0
                || m.selected.0 != l.1
                || m.selected.1 != l.2
                || m.x != l.3
                || m.y != l.4
            {
                return true;
            }
        }
        _ => return true,
    }
    false
}

/// 跳帧判据入口（薄封装，见 [`should_present_with`]）。
fn renderer_should_present(
    r: &Renderer,
    frame: &Frame,
    layers_nonempty: bool,
    atlas_has_pending: bool,
) -> bool {
    should_present_with(
        r.last_present.as_ref(),
        r.drawable_size,
        frame,
        layers_nonempty,
        atlas_has_pending,
    )
}

impl Renderer {
    /// 画一帧：先组顶点（cell 循环里按需光栅化新字形），随后把
    /// atlas pending（含本帧新字形）上传纹理，最后才编码 draw。
    /// p5：cell pass 之后编码层 pass（插件 IOSurface 纹理按矩形盖上，
    /// 同一 encoder 换管线，无额外 render pass）。
    /// 顺序是 p1 黑屏缺陷的修复本体：`replaceRegion` 是 CPU 侧立即写入
    /// （Shared 存储），GPU 在同一命令缓冲 commit 后才采样 —— 本帧
    /// 光栅化的字形当帧可见，不需要「下一帧」来补上传（空闲终端没有
    /// 下一帧：无 PTY 字节、光标不闪烁时不重画）。
    /// 不额外调度重画：无新增槽位时 pending 为空、上传 no-op，无自旋。
    /// 光标可见性/闪烁相位由 view 折进 frame.cursor（None = 不画）。
    ///
    /// D-C：跳帧判据（`renderer_should_present`）判为不必画时直接返回（不 nextDrawable、
    /// 不组顶点、不提交命令缓冲）——Clean 且视觉未变的帧零成本跳过；
    /// 空闲 CPU 为零的红线由此保证（跳帧不调度任何后续工作）。
    /// 返回是否真的提交了一帧。
    pub fn draw(
        &mut self,
        frame: &Frame,
        atlas: &mut GlyphAtlas,
        font: &mut Font,
        layers: &[LayerDraw],
    ) -> bool {
        let (dw, dh) = self.drawable_size;
        if dw < 1.0 || dh < 1.0 || frame.cols == 0 || frame.rows == 0 {
            return false;
        }
        if !renderer_should_present(self, frame, !layers.is_empty(), atlas.has_pending()) {
            self.frames_skipped += 1;
            return false;
        }
        self.layer.setDrawableSize(CGSize {
            width: dw,
            height: dh,
        });
        let Some(drawable) = self.layer.nextDrawable() else {
            return false;
        };

        // ---- 组顶点（复用缓冲，跨帧保容量；编码完归还 self）----
        // atlas 满版 reset 会作废本 pass 已建 quad 的槽位（map 清空、
        // 槽位重分）→ 有 reset 就重建一遍顶点。上限 3 pass：reset 只
        // 由光栅化新字形触发，空闲帧不退循环，无自旋。3 pass 仍未收敛
        // （单帧不同形数超过整版容量）时如实取证——顶点可能引用已被
        // 重新分配的槽位，字形会错乱，不能无声吞掉（X1 取证路径）。
        let mut verts = std::mem::take(&mut self.verts);
        verts.clear();
        verts.reserve(frame.cells.len() * 6);
        for pass in 0..3 {
            let resets_before = atlas.resets();
            verts.clear();
            self.build_verts(&mut verts, frame, atlas, font);
            if atlas.resets() == resets_before {
                break;
            }
            if pass == 2 {
                eprintln!(
                    "ninja: 本帧不同字形数超过 atlas 容量（{} 次 reset 仍未收敛），字形可能错乱",
                    atlas.resets()
                );
            }
        }

        // ---- atlas 上传：组完顶点后、编码前，本帧字形当帧进纹理 ----
        self.upload_pending(atlas.take_pending());

        // 取证开关：NINJA_DUMP_ATLAS=/path.pgm 读回 atlas 纹理落盘
        // （验证字形上传内容用，上传后读，所见即 GPU 所采，默认关闭，
        // 见 NOTES.md 复跑命令）。
        if let Some(path) = std::env::var_os("NINJA_DUMP_ATLAS") {
            let edge = atlas.edge() as usize;
            let mut buf = vec![0u8; edge * edge];
            // SAFETY: 布局与 R8Unorm 匹配，读回整版。
            unsafe {
                self.atlas_texture.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                    std::ptr::NonNull::new(buf.as_mut_ptr().cast()).unwrap(),
                    edge,
                    objc2_metal::MTLRegion {
                        origin: objc2_metal::MTLOrigin { x: 0, y: 0, z: 0 },
                        size: objc2_metal::MTLSize { width: edge, height: edge, depth: 1 },
                    },
                    0,
                );
            }
            let mut out = format!("P5\n{edge} {edge}\n255\n").into_bytes();
            out.extend_from_slice(&buf);
            let _ = std::fs::write(&path, out);
        }

        // ---- 编码并呈现 ----
        let Some(cmdbuf) = self.queue.commandBuffer() else {
            self.verts = verts;
            return false;
        };
        let pass = MTLRenderPassDescriptor::new();
        let attachment = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        let texture = drawable.texture();
        attachment.setTexture(Some(&texture));
        attachment.setLoadAction(MTLLoadAction::Clear);
        attachment.setStoreAction(MTLStoreAction::Store);
        attachment.setClearColor(MTLClearColor {
            red: f64::from(frame.bg.0) / 255.0,
            green: f64::from(frame.bg.1) / 255.0,
            blue: f64::from(frame.bg.2) / 255.0,
            alpha: 1.0,
        });

        let Some(encoder) = cmdbuf.renderCommandEncoderWithDescriptor(&pass) else {
            self.verts = verts;
            return false;
        };
        // SAFETY: verts 生存期覆盖 commit；顶点字节数 < 4KB 限制之外时
        // setVertexBytes 仍允许更大上限（Metal 文档上限 4KB，超限走 buffer）。
        // p1 终端尺寸上限下先按 buffer 走，避免踩 4KB 语义争议。
        let byte_len = verts.len() * std::mem::size_of::<Vertex>();
        let vertex_buffer = if byte_len == 0 {
            None
        } else {
            // SAFETY: verts 指针与长度匹配，生存期覆盖 commit。
            unsafe {
                self.device.newBufferWithBytes_length_options(
                    std::ptr::NonNull::new(verts.as_ptr() as *mut std::ffi::c_void).unwrap(),
                    byte_len,
                    MTLResourceOptions::StorageModeShared,
                )
            }
        };
        if byte_len > 0 && vertex_buffer.is_none() {
            self.verts = verts;
            return false;
        }
        encoder.setRenderPipelineState(&self.pipeline);
        unsafe {
            if let Some(vb) = &vertex_buffer {
                encoder.setVertexBuffer_offset_atIndex(Some(vb), 0, 0);
            }
            let vp = [dw as f32, dh as f32];
            encoder.setVertexBytes_length_atIndex(std::ptr::NonNull::new(vp.as_ptr() as *mut std::ffi::c_void).unwrap(), 8, 1);
            encoder.setFragmentTexture_atIndex(Some(&self.atlas_texture), 0);
            encoder.setFragmentSamplerState_atIndex(Some(&self.sampler), 0);
            encoder.drawPrimitives_vertexStart_vertexCount_instanceCount(
                MTLPrimitiveType::Triangle,
                0,
                verts.len() as u64 as usize,
                1,
            );

            // ---- p5 层 pass：同一 encoder，换管线逐层盖上 ----
            // 层顶点：uv 全幅 0..1（uv0,0 = 表面左上 = 层矩形左上），
            // 顶点色恒白（alpha 由纹理自带）。
            for l in layers {
                let (x, y, w, h) = l.rect;
                let (x0, y0) = (x as f32, y as f32);
                let (x1, y1) = ((x + w) as f32, (y + h) as f32);
                let (r, g, b, a) = (1.0f32, 1.0, 1.0, 1.0);
                let lv: [Vertex; 6] = [
                    Vertex { x: x0, y: y0, u: 0.0, v: 0.0, r, g, b, a },
                    Vertex { x: x1, y: y0, u: 1.0, v: 0.0, r, g, b, a },
                    Vertex { x: x1, y: y1, u: 1.0, v: 1.0, r, g, b, a },
                    Vertex { x: x0, y: y0, u: 0.0, v: 0.0, r, g, b, a },
                    Vertex { x: x1, y: y1, u: 1.0, v: 1.0, r, g, b, a },
                    Vertex { x: x0, y: y1, u: 0.0, v: 1.0, r, g, b, a },
                ];
                encoder.setRenderPipelineState(&self.layer_pipeline);
                encoder.setFragmentTexture_atIndex(Some(&l.texture), 0);
                encoder.setFragmentSamplerState_atIndex(Some(&self.sampler), 0);
                encoder.setVertexBytes_length_atIndex(
                    std::ptr::NonNull::new(lv.as_ptr() as *mut std::ffi::c_void).unwrap(),
                    std::mem::size_of::<[Vertex; 6]>(),
                    0,
                );
                encoder.drawPrimitives_vertexStart_vertexCount_instanceCount(
                    MTLPrimitiveType::Triangle,
                    0,
                    6,
                    1,
                );
            }
            encoder.endEncoding();
        }
        let drawable_texture = drawable.texture();
        let drawable: &ProtocolObject<dyn objc2_metal::MTLDrawable> = drawable.as_ref();
        cmdbuf.presentDrawable(drawable);
        cmdbuf.commit();
        // T-主题取证开关：NINJA_DUMP_DRAWABLE=<dir> 时把本帧 drawable
        // 读回落盘（blit 拷到自建 Shared 纹理，见 dump_drawable_ppm）。
        // E2E 像素探针：背景 #282C34 / ANSI 官方色。
        if drawable_probe_dir().is_some() {
            self.dump_drawable_ppm(&drawable_texture);
        }
        self.frames_drawn += 1;
        // 已呈现：记录视觉签名（后续 Clean 帧跳帧判据）。
        self.last_present = Some(LastPresent {
            drawable_size: (dw, dh),
            cols: frame.cols,
            rows: frame.rows,
            fg: frame.fg,
            bg: frame.bg,
            cursor: frame.cursor.map(|c| (c.x, c.y)),
            cursor_style: frame.cursor_style,
            marked: frame
                .marked
                .as_ref()
                .map(|m| (m.text.clone(), m.selected.0, m.selected.1, m.x, m.y)),
            had_layers: !layers.is_empty(),
        });
        self.verts = verts;

        // 取证开关：NINJA_LAYER_PROBE=<dir> 时把每个已呈现层的纹理读回
        // 落盘 <dir>/<handle>.ppm（E2E 断言「层内确有文本墨迹」用；
        // layer::close 摘层时删除对应文件——close 在 plugins 侧调
        // renderer 的静态助手，这里只管 dump）。
        if let Some(dir) = std::env::var_os("NINJA_LAYER_PROBE") {
            for l in layers {
                self.dump_layer_ppm(l, std::path::Path::new(&dir));
            }
        }
        true
    }

    /// 层纹理读回 → PPM（BGRA→RGB）。失败静默（取证钩子不炸产品路径）。
    fn dump_layer_ppm(&self, l: &LayerDraw, dir: &std::path::Path) {
        let w = l.rect.2.round().max(1.0) as usize;
        let h = l.rect.3.round().max(1.0) as usize;
        let mut buf = vec![0u8; w * h * 4];
        // SAFETY: 布局与 BGRA8Unorm 匹配，读回整幅。
        unsafe {
            l.texture.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                std::ptr::NonNull::new(buf.as_mut_ptr().cast()).unwrap(),
                w * 4,
                objc2_metal::MTLRegion {
                    origin: objc2_metal::MTLOrigin { x: 0, y: 0, z: 0 },
                    size: objc2_metal::MTLSize { width: w, height: h, depth: 1 },
                },
                0,
            );
        }
        let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
        out.reserve(w * h * 3);
        for px in buf.chunks_exact(4) {
            out.extend_from_slice(&[px[2], px[1], px[0]]); // BGRA→RGB
        }
        let _ = std::fs::write(dir.join(format!("{}.ppm", l.handle)), out);
    }

    /// T-主题取证：把已呈现的 drawable 读回落盘 PPM（cyclic 3 槽位
    /// 覆盖写：`<dir>/frame_<n%3>.ppm`，n 为全局已呈现序号）。drawable
    /// 纹理的存储模式不保证 CPU 直读——先 GPU blit 拷进自建 Shared
    /// 纹理，等拷贝命令缓冲完成后 getBytes，所见即已呈现像素。
    /// 失败静默（取证钩子不炸产品路径；仅 NINJA_DUMP_DRAWABLE 开启）。
    fn dump_drawable_ppm(&self, drawable_texture: &ProtocolObject<dyn MTLTexture>) {
        let Some(dir) = drawable_probe_dir() else { return };
        let texture = drawable_texture;
        let (w, h) = (texture.width(), texture.height());
        if w == 0 || h == 0 {
            return;
        }
        let desc = MTLTextureDescriptor::new();
        // SAFETY: 尺寸/格式与源 drawable 一致，仅取证读回用。
        unsafe {
            desc.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
            desc.setWidth(w);
            desc.setHeight(h);
            desc.setStorageMode(MTLStorageMode::Shared);
            desc.setUsage(MTLTextureUsage::ShaderRead);
        }
        let Some(dest) = self.device.newTextureWithDescriptor(&desc) else {
            return;
        };
        let Some(copybuf) = self.queue.commandBuffer() else {
            return;
        };
        let Some(enc) = copybuf.blitCommandEncoder() else {
            return;
        };
        // SAFETY: 区域在两张同格式/同尺寸纹理界内，拷贝后 CPU 读 Shared
        // 存储（无需 synchronize）。
        unsafe {
            enc.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                texture,
                0,
                0,
                objc2_metal::MTLOrigin { x: 0, y: 0, z: 0 },
                objc2_metal::MTLSize { width: w, height: h, depth: 1 },
                &dest,
                0,
                0,
                objc2_metal::MTLOrigin { x: 0, y: 0, z: 0 },
            );
            enc.endEncoding();
        }
        copybuf.commit();
        copybuf.waitUntilCompleted();
        let mut buf = vec![0u8; w * h * 4];
        // SAFETY: 布局与 BGRA8Unorm 匹配，读回整幅。
        unsafe {
            dest.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                std::ptr::NonNull::new(buf.as_mut_ptr().cast()).unwrap(),
                w * 4,
                objc2_metal::MTLRegion {
                    origin: objc2_metal::MTLOrigin { x: 0, y: 0, z: 0 },
                    size: objc2_metal::MTLSize { width: w, height: h, depth: 1 },
                },
                0,
            );
        }
        let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
        out.reserve(w * h * 3);
        for px in buf.chunks_exact(4) {
            out.extend_from_slice(&[px[2], px[1], px[0]]); // BGRA→RGB
        }
        let n = DRAWABLE_PROBE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let slot = n % 3;
        // 原子替换写：先写临时名再 rename，读侧不会看到半截 PPM。
        let tmp = dir.join(format!("frame_{slot}.ppm.tmp"));
        if std::fs::write(&tmp, out).is_ok() {
            let _ = std::fs::rename(&tmp, dir.join(format!("frame_{slot}.ppm")));
        }
    }

    /// 组一帧的全部顶点：cell 循环（背景/字形/装饰）→ IME 预编辑 →
    /// 非块光标。会经 `atlas.get_or_rasterize` 光栅化新字形并压进
    /// atlas pending（上传由 `upload_pending` 在 draw 内随后消费）。
    /// cell 循环本体在 [`build_cell_pass`]（自由函数，无 Metal 依赖，
    /// 宽字形/占位格回归测试直接看顶点）。
    fn build_verts(
        &mut self,
        verts: &mut Vec<Vertex>,
        frame: &Frame,
        atlas: &mut GlyphAtlas,
        font: &mut Font,
    ) {
        let selection = self.selection_rgba();
        let cursor_color = self.effective_cursor();
        build_cell_pass(
            verts,
            frame,
            self.cell_px,
            selection,
            cursor_color,
            atlas,
            font,
        );

        // IME 预编辑串：从光标 cell 起按字符宽度逐字落格，下划线标记。
        if let Some(marked) = &frame.marked {
            let white = atlas.white();
            let edge = atlas.edge() as f32;
            self.draw_marked(
                verts,
                white,
                edge,
                marked,
                frame,
                rgb_to_f32s(frame.fg),
                atlas,
                font,
            );
        }

        // 非块光标样式：条 / 下划线 / 空心块。
        if let Some(c) = frame.cursor {
            let white = atlas.white();
            let edge = atlas.edge() as f32;
            let (cw, ch, baseline) = self.cell_px;
            let x0 = f64::from(c.x) * cw;
            let y0 = f64::from(c.y) * ch;
            let color = rgb_to_f32s(self.effective_cursor());
            match frame.cursor_style {
                CursorVisualStyle::Bar => {
                    push_quad(verts, white, edge, x0, y0, 2.0, ch, color);
                }
                CursorVisualStyle::Underline => {
                    push_quad(verts, white, edge, x0, y0 + baseline + 1.0, cw, 2.0, color);
                }
                CursorVisualStyle::BlockHollow => {
                    let t = 2.0;
                    push_quad(verts, white, edge, x0, y0, cw, t, color);
                    push_quad(verts, white, edge, x0, y0 + ch - t, cw, t, color);
                    push_quad(verts, white, edge, x0, y0, t, ch, color);
                    push_quad(verts, white, edge, x0 + cw - t, y0, t, ch, color);
                }
                _ => {}
            }
        }
    }

    /// 选区 quad 色：生效色板的选区色 + alpha（ODP 基线 = 官方
    /// terminal.selectionBackground `#abb2bf30` = 前景 #abb2bf + 0x30
    /// alpha；TOML 覆盖只换 RGB；插件换色板则整套跟随插件值）。
    fn selection_rgba(&self) -> [f32; 4] {
        let pal = crate::theme::current();
        let mut c = rgb_to_f32s(self.theme.selection_bg.unwrap_or(pal.selection_bg));
        c[3] = f32::from(pal.selection_alpha) / 255.0;
        c
    }

    /// 当前生效光标色（TOML 覆盖优先；否则生效色板）。
    fn effective_cursor(&self) -> Rgb {
        self.theme.cursor.unwrap_or_else(|| crate::theme::current().cursor)
    }

    /// 把 atlas 的 pending 槽位写入纹理（CPU 侧立即写入，Shared 存储；
    /// 在本帧命令缓冲编码前调用，commit 后 GPU 采样即所见）。
    fn upload_pending(&mut self, uploads: Vec<crate::atlas::PendingUpload>) {
        for up in uploads {
            let region = objc2_metal::MTLRegion {
                origin: objc2_metal::MTLOrigin {
                    x: up.region.x as usize,
                    y: up.region.y as usize,
                    z: 0,
                },
                size: objc2_metal::MTLSize {
                    width: up.region.w as usize,
                    height: up.region.h as usize,
                    depth: 1,
                },
            };
            // SAFETY: bytes 长度 = w*h，行距 w，与 R8Unorm 匹配。
            unsafe {
                self.atlas_texture
                    .replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                        region,
                        0,
                        std::ptr::NonNull::new(up.bytes.as_ptr() as *mut std::ffi::c_void)
                            .unwrap(),
                        up.region.w as usize,
                    );
            }
        }
    }
    fn draw_marked(
        &mut self,
        verts: &mut Vec<Vertex>,
        white: GlyphRect,
        edge: f32,
        marked: &Marked,
        frame: &Frame,
        default_fg: [f32; 4],
        atlas: &mut GlyphAtlas,
        font: &mut Font,
    ) {
        let (cw, ch, baseline) = self.cell_px;
        draw_marked_into(
            verts,
            white,
            edge,
            cw,
            ch,
            baseline,
            marked,
            frame,
            default_fg,
            self.selection_rgba(),
            atlas,
            font,
        );
    }
}

fn draw_marked_into(
    verts: &mut Vec<Vertex>,
    white: GlyphRect,
    edge: f32,
    cw: f64,
    ch: f64,
    baseline: f64,
    marked: &Marked,
    frame: &Frame,
    default_fg: [f32; 4],
    selection: [f32; 4],
    atlas: &mut GlyphAtlas,
    font: &mut Font,
) {
    let cols = usize::from(frame.cols);
    let rows = usize::from(frame.rows);
    let row = usize::from(marked.y);
    if row >= rows {
        return;
    }
    let (sel_loc, sel_len) = marked.selected;
    let mut col = usize::from(marked.x).min(cols.saturating_sub(1));
    for (char_idx, c) in marked.text.chars().enumerate() {
        if col >= cols {
            break; // 超出行尾就截断（真实终端会把光标挤到下一行；p1 够用）
        }
        let span = usize::from(libghostty_vt::unicode::codepoint_width(c).max(1));
        let x0 = col as f64 * cw;
        let y0 = row as f64 * ch;
        if char_idx >= sel_loc && char_idx < sel_loc.saturating_add(sel_len) {
            push_quad(
                verts,
                white,
                edge,
                x0,
                y0,
                cw * span as f64,
                ch,
                selection,
            );
        }
        let mut s = String::new();
        s.push(c);
        if let Some(rect) = atlas.get_or_rasterize(&s, Weight::Regular, font) {
            let gy = y0 + baseline + rect.baseline_to_top;
            push_glyph_quad(verts, &rect, edge, x0, gy, rect.w as f32, rect.h as f32, default_fg);
        }
        // 预编辑下划线（与光标同色，粗一点便于区分）。
        push_quad(verts, white, edge, x0, y0 + baseline + 2.0, cw * span as f64, 2.0, default_fg);
        col += span;
    }
}

fn rgb_to_f32s(c: Rgb) -> [f32; 4] {
    [
        f32::from(c.0) / 255.0,
        f32::from(c.1) / 255.0,
        f32::from(c.2) / 255.0,
        1.0,
    ]
}

/// cell 循环本体（背景/字形/下划线/删除线），从 `Renderer::build_verts`
/// 提出的自由函数：不碰 Metal 状态机，宽字形（East Asian Width：vt 核
/// 给的 `CellWideKind::Wide`）的背景/装饰 span 两格，占位格
///（SpacerTail/SpacerHead）整体跳过。回归测试直接看顶点缓冲。
#[allow(clippy::too_many_arguments)]
fn build_cell_pass(
    verts: &mut Vec<Vertex>,
    frame: &Frame,
    cell_px: (f64, f64, f64),
    selection: [f32; 4],
    cursor_color: Rgb,
    atlas: &mut GlyphAtlas,
    font: &mut Font,
) {
    let white = atlas.white();
    let edge = atlas.edge() as f32;
    let (cw, ch, baseline) = cell_px;
    let default_bg = rgb_to_f32s(frame.bg);
    let default_fg = rgb_to_f32s(frame.fg);

    for (i, cell) in frame.cells.iter().enumerate() {
        if matches!(cell.wide, CellWideKind::SpacerTail | CellWideKind::SpacerHead) {
            continue;
        }
        let col = f64::from((i % usize::from(frame.cols)) as u32);
        let row = f64::from((i / usize::from(frame.cols)) as u32);
        let x0 = col * cw;
        let y0 = row * ch;

        // 有效前景/背景：inverse 交换；选区覆盖；块光标再覆盖。
        let mut fg = cell.fg.map(rgb_to_f32s).unwrap_or(default_fg);
        let mut bg = cell.bg.map(rgb_to_f32s).unwrap_or(default_bg);
        if cell.inverse {
            std::mem::swap(&mut fg, &mut bg);
        }
        let selected = cell.selected;
        if selected {
            bg = selection;
        }

        let is_cursor_cell = frame.cursor.is_some_and(|c| {
            usize::from(c.x) == i % usize::from(frame.cols)
                && usize::from(c.y) == i / usize::from(frame.cols)
        });
        let cursor_block =
            is_cursor_cell && matches!(frame.cursor_style, CursorVisualStyle::Block);

        // 宽字形 span：East Asian Width 双宽占两格（vt 核 CellWide::Wide）。
        let span = if cell.wide == CellWideKind::Wide { 2.0 } else { 1.0 };

        if bg != default_bg || selected || cursor_block {
            let bg_color = if cursor_block {
                rgb_to_f32s(cursor_color)
            } else {
                bg
            };
            push_quad(verts, white, edge, x0, y0, cw * span, ch, bg_color);
            if cursor_block {
                fg = default_bg; // 块光标上字形反色
            }
        }

        // 字形（G 回退：atlas 槽位按字体+字形双维度，回退字体字形不冒名）。
        if !cell.text.is_empty() {
            let weight = match (cell.bold, cell.italic) {
                (true, true) => Weight::BoldItalic,
                (true, false) => Weight::Bold,
                (false, true) => Weight::Italic,
                (false, false) => Weight::Regular,
            };
            if let Some(rect) = atlas.get_or_rasterize(&cell.text, weight, font) {
                // ink 左沿贴 cell（dx = ink.x - 1px 边距）。
                let gx = x0 + rect.dx;
                let gy = y0 + baseline + rect.baseline_to_top;
                push_glyph_quad(
                    verts,
                    &rect,
                    edge,
                    gx,
                    gy,
                    rect.w as f32,
                    rect.h as f32,
                    fg,
                );
            }
        }

        // 下划线 / 删除线：跟着宽字形 span（G：宽字形装饰旧实现只画
        // 一格宽，CJK 下划线在双宽字后半截缺失）。
        if cell.underline {
            push_quad(verts, white, edge, x0, y0 + baseline + 1.0, cw * span, 1.0, fg);
        }
        if cell.strikethrough {
            push_quad(
                verts,
                white,
                edge,
                x0,
                y0 + baseline - ch * 0.28,
                cw * span,
                1.0,
                fg,
            );
        }
    }
}

/// NINJA_DUMP_DRAWABLE 取证目录（只读一次 env；None = 关）。
fn drawable_probe_dir() -> Option<std::path::PathBuf> {
    static DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| std::env::var_os("NINJA_DUMP_DRAWABLE").map(std::path::PathBuf::from))
        .clone()
}

/// NINJA_DUMP_DRAWABLE 的已呈现帧序号（cyclic 3 槽位文件名用）。
static DRAWABLE_PROBE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn push_quad(
    verts: &mut Vec<Vertex>,
    white: GlyphRect,
    edge: f32,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    color: [f32; 4],
) {
    let (x0, y0) = (x as f32, y as f32);
    let (x1, y1) = ((x + w) as f32, (y + h) as f32);
    let (u0, v0) = (white.x as f32 / edge, white.y as f32 / edge);
    let (u1, v1) = (
        (white.x + white.w) as f32 / edge,
        (white.y + white.h) as f32 / edge,
    );
    let (r, g, b, a) = (color[0], color[1], color[2], color[3]);
    verts.extend_from_slice(&[
        Vertex { x: x0, y: y0, u: u0, v: v0, r, g, b, a },
        Vertex { x: x1, y: y0, u: u1, v: v0, r, g, b, a },
        Vertex { x: x1, y: y1, u: u1, v: v1, r, g, b, a },
        Vertex { x: x0, y: y0, u: u0, v: v0, r, g, b, a },
        Vertex { x: x1, y: y1, u: u1, v: v1, r, g, b, a },
        Vertex { x: x0, y: y1, u: u0, v: v1, r, g, b, a },
    ]);
}

fn push_glyph_quad(
    verts: &mut Vec<Vertex>,
    rect: &GlyphRect,
    edge: f32,
    x: f64,
    y: f64,
    w: f32,
    h: f32,
    color: [f32; 4],
) {
    let (x0, y0) = (x as f32, y as f32);
    let (x1, y1) = (x0 + w, y0 + h);
    let (u0, v0) = (rect.x as f32 / edge, rect.y as f32 / edge);
    let (u1, v1) = (
        (rect.x + rect.w) as f32 / edge,
        (rect.y + rect.h) as f32 / edge,
    );
    let (r, g, b, a) = (color[0], color[1], color[2], color[3]);
    verts.extend_from_slice(&[
        Vertex { x: x0, y: y0, u: u0, v: v0, r, g, b, a },
        Vertex { x: x1, y: y0, u: u1, v: v0, r, g, b, a },
        Vertex { x: x1, y: y1, u: u1, v: v1, r, g, b, a },
        Vertex { x: x0, y: y0, u: u0, v: v0, r, g, b, a },
        Vertex { x: x1, y: y1, u: u1, v: v1, r, g, b, a },
        Vertex { x: x0, y: y1, u: u0, v: v1, r, g, b, a },
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::{CursorView, FrameCell};

    /// 回归（p1 黑屏缺陷）：首帧光栅化的字形必须同帧进 atlas 纹理。
    /// 老实现在帧首 drain pending，本帧新字形只画 quad 不上传（下一帧
    /// 才进纹理）；空闲终端无 PTY 字节、光标不闪烁 → 没有下一帧，
    /// 首屏全黑。本测试离屏跑一次 draw，读回纹理取证。
    /// 拿不到 Metal 设备/drawable（纯 headless 无窗口服务）时跳过。
    #[test]
    fn first_frame_glyphs_uploaded_same_frame() {
        let scale = 2.0;
        let mut font = Font::new(13.0, scale);
        let cw = (font.metrics.cell_w * scale).ceil() as u32;
        let ch = (font.metrics.cell_h * scale).ceil() as u32;
        let layer = CAMetalLayer::new();
        layer.setContentsScale(scale);
        let Some(mut r) = Renderer::new(
            layer,
            GlyphAtlas::EDGE,
            (f64::from(cw), f64::from(ch), font.baseline_offset() * scale),
        ) else {
            eprintln!("skip: no Metal device");
            return;
        };
        r.drawable_size = (f64::from(cw) * 10.0, f64::from(ch) * 4.0);

        // 一帧 "bash$ " + 空白格（bash 启动提示符的最小化身）+ G：首帧就
        // 带回退字体字形（中文 / emoji——分别落 PingFang / AppleColorEmoji
        // 槽），验证回退路径同帧上传不被跳帧吃掉。
        let mut frame = Frame {
            cols: 10,
            rows: 4,
            fg: Rgb(255, 255, 255),
            bg: Rgb(0, 0, 0),
            cursor: Some(CursorView { x: 5, y: 0, at_wide_tail: false }),
            cursor_style: CursorVisualStyle::Block,
            cursor_blinking: false,
            dirty: libghostty_vt::render::Dirty::Clean,
            cells: vec![FrameCell::default(); 40],
            marked: None,
        };
        for (i, c) in "bash$ "
            .chars()
            .chain("中".chars())
            .chain("\u{1F600}".chars())
            .enumerate()
        {
            frame.cells[i].text = c.to_string();
        }

        let mut atlas = GlyphAtlas::new(ch);
        r.draw(&frame, &mut atlas, &mut font, &[]);

        // 离屏 layer 拿不到 drawable → 帧没画成，headless 环境跳过。
        if r.frames_drawn == 0 {
            eprintln!("skip: no drawable (headless)");
            return;
        }
        assert_eq!(r.frames_drawn, 1);

        // pending 已被本帧消费干净，不留给「可能永远不会来的下一帧」。
        assert!(atlas.take_pending().is_empty());

        // 纹理读回：白块 (0,0) 之外必须有字形 ink。
        let edge = atlas.edge() as usize;
        let mut buf = vec![0u8; edge * edge];
        // SAFETY: 布局与 R8Unorm 匹配，读回整版。
        unsafe {
            r.atlas_texture.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                std::ptr::NonNull::new(buf.as_mut_ptr().cast()).unwrap(),
                edge,
                objc2_metal::MTLRegion {
                    origin: objc2_metal::MTLOrigin { x: 0, y: 0, z: 0 },
                    size: objc2_metal::MTLSize { width: edge, height: edge, depth: 1 },
                },
                0,
            );
        }
        let ink = buf[1..].iter().filter(|&&v| v > 0).count();
        assert!(ink > 50, "atlas ink beyond white block: {ink} (want >50)");
        // 回退字形确实进了 atlas：基础槽 + 中/emoji 回退槽部有位图。
        assert!(atlas.cached_count() >= 8, "ASCII + 中文 + emoji 全部入槽");
    }

    /// D-C 回归（需 drawable，headless 自动跳过）：Clean 且视觉未变的
    /// 第二帧不提交 drawable（frames_drawn 不涨、frames_skipped 涨）；
    /// 光标动了的 Clean 帧（`\r` 语义）仍然提交。
    #[test]
    fn clean_unchanged_frame_skips_drawable() {
        let scale = 2.0;
        let mut font = Font::new(13.0, scale);
        let cw = (font.metrics.cell_w * scale).ceil() as u32;
        let ch = (font.metrics.cell_h * scale).ceil() as u32;
        let layer = CAMetalLayer::new();
        layer.setContentsScale(scale);
        let Some(mut r) = Renderer::new(
            layer,
            GlyphAtlas::EDGE,
            (f64::from(cw), f64::from(ch), font.baseline_offset() * scale),
        ) else {
            eprintln!("skip: no Metal device");
            return;
        };
        r.drawable_size = (f64::from(cw) * 10.0, f64::from(ch) * 4.0);

        let mk_frame = |cur_x: u16| Frame {
            cols: 10,
            rows: 4,
            fg: Rgb(255, 255, 255),
            bg: Rgb(0, 0, 0),
            cursor: Some(CursorView { x: cur_x, y: 0, at_wide_tail: false }),
            cursor_style: CursorVisualStyle::Block,
            cursor_blinking: false,
            dirty: libghostty_vt::render::Dirty::Clean,
            cells: vec![FrameCell::default(); 40],
            marked: None,
        };

        let mut atlas = GlyphAtlas::new(ch);
        let f1 = mk_frame(5);
        r.draw(&f1, &mut atlas, &mut font, &[]);
        if r.frames_drawn == 0 {
            eprintln!("skip: no drawable (headless)");
            return;
        }
        assert_eq!(r.frames_drawn, 1);

        // 同帧再来：Clean 且签名一致 → 跳。
        r.draw(&f1, &mut atlas, &mut font, &[]);
        assert_eq!(r.frames_drawn, 1, "Clean 未变帧不得提交 drawable");
        assert_eq!(r.frames_skipped, 1);

        // 光标移动的 Clean 帧（vt 对 `\r` 不标脏）：必须提交。
        let f2 = mk_frame(0);
        r.draw(&f2, &mut atlas, &mut font, &[]);
        assert_eq!(r.frames_drawn, 2, "Clean 但光标变了必须重画");
        assert_eq!(r.frames_skipped, 1);
    }

    /// D-C 回归（纯函数，headless）：跳帧判据。
    /// Clean 且视觉签名一致 → 跳；任一「vt 不标脏但屏幕要变」路径
    ///（光标移动/OSC 颜色/预编辑/尺寸/层摘除）→ 画；首帧/帧脏/有层/
    /// atlas 有待传 → 画。
    #[test]
    fn should_present_covers_clean_but_visible_changes() {
        use crate::term::Marked;

        let frame = |dirty: Dirty, cursor: Option<(u16, u16)>, fg: Rgb, marked: Option<Marked>| Frame {
            cols: 10,
            rows: 4,
            fg,
            bg: Rgb(0, 0, 0),
            cursor: cursor.map(|(x, y)| CursorView { x, y, at_wide_tail: false }),
            cursor_style: CursorVisualStyle::Block,
            cursor_blinking: false,
            dirty,
            cells: Vec::new(),
            marked,
        };

        // 首帧必画。
        assert!(should_present_with(
            None,
            (800.0, 600.0),
            &frame(Dirty::Clean, Some((0, 0)), Rgb(255, 255, 255), None),
            false,
            false
        ));

        let last = LastPresent {
            drawable_size: (800.0, 600.0),
            cols: 10,
            rows: 4,
            fg: Rgb(255, 255, 255),
            bg: Rgb(0, 0, 0),
            cursor: Some((3, 1)),
            cursor_style: CursorVisualStyle::Block,
            marked: None,
            had_layers: false,
        };
        let same = frame(Dirty::Clean, Some((3, 1)), Rgb(255, 255, 255), None);

        // Clean + 全一致 → 跳（不提交 drawable / 不重建顶点）。
        assert!(!should_present_with(Some(&last), (800.0, 600.0), &same, false, false));

        // vt 不标脏但屏幕要变：`\r` 类光标移动。
        assert!(should_present_with(
            Some(&last),
            (800.0, 600.0),
            &frame(Dirty::Clean, Some((0, 1)), Rgb(255, 255, 255), None),
            false,
            false
        ));
        // 光标消失（闪烁抑制相位翻转）。
        assert!(should_present_with(
            Some(&last),
            (800.0, 600.0),
            &frame(Dirty::Clean, None, Rgb(255, 255, 255), None),
            false,
            false
        ));
        // OSC 10/11 改默认前景。
        assert!(should_present_with(
            Some(&last),
            (800.0, 600.0),
            &frame(Dirty::Clean, Some((3, 1)), Rgb(1, 2, 3), None),
            false,
            false
        ));
        // IME 预编辑出现。
        let with_marked = frame(
            Dirty::Clean,
            Some((3, 1)),
            Rgb(255, 255, 255),
            Some(Marked { text: "你".into(), selected: (0, 0), x: 3, y: 1 }),
        );
        assert!(should_present_with(
            Some(&last),
            (800.0, 600.0),
            &with_marked,
            false,
            false
        ));
        // resize（drawable 尺寸变化）。
        assert!(should_present_with(Some(&last), (801.0, 600.0), &same, false, false));
        // 帧脏（Partial/Full）。
        assert!(should_present_with(
            Some(&last),
            (800.0, 600.0),
            &frame(Dirty::Partial, Some((3, 1)), Rgb(255, 255, 255), None),
            false,
            false
        ));
        // 有层 → 每帧重画（层内容宿主无法脏跟踪）。
        assert!(should_present_with(Some(&last), (800.0, 600.0), &same, true, false));
        let last_with_layer = LastPresent { had_layers: true, ..last.clone() };
        assert!(should_present_with(
            Some(&last_with_layer),
            (800.0, 600.0),
            &same,
            true,
            false
        ));
        // 上次带层、这次无层 → 必画（把层从屏幕上抹掉）。
        assert!(should_present_with(
            Some(&last_with_layer),
            (800.0, 600.0),
            &same,
            false,
            false
        ));
        // atlas 还有待上传（首帧字形同帧上传兜底）。
        assert!(should_present_with(Some(&last), (800.0, 600.0), &same, false, true));
    }

    /// G 回归（纯函数，无 Metal/无 drawable）：cell pass 顶点几何。
    /// ① 宽字形（EAW 双宽）：背景/下划线/删除线 span 两格；
    /// ② 占位格（SpacerTail）整体跳过（带下划线也不画）；
    /// ③ 窄字形装饰一格宽。
    #[test]
    fn wide_cell_decorations_span_two_cells_and_spacers_skipped() {
        struct Quad {
            x0: f64,
            x1: f64,
            y0: f64,
            y1: f64,
            rgba: [f32; 4],
        }
        let as_quads = |verts: &[Vertex]| -> Vec<Quad> {
            verts
                .chunks_exact(6)
                .map(|c| Quad {
                    x0: c[0].x as f64,
                    x1: c[1].x as f64,
                    y0: c[0].y as f64,
                    y1: c[2].y as f64,
                    rgba: [c[0].r, c[0].g, c[0].b, c[0].a],
                })
                .collect()
        };

        let scale = 2.0;
        let mut font = Font::new(13.0, scale);
        let cw = font.metrics.cell_w * scale;
        let ch = (font.metrics.cell_h * scale).ceil();
        let baseline = font.baseline_offset() * scale;
        let mut atlas = GlyphAtlas::new(ch as u32);

        let mut frame = Frame {
            cols: 4,
            rows: 1,
            fg: Rgb(255, 255, 255),
            bg: Rgb(0, 0, 0),
            cursor: None,
            cursor_style: CursorVisualStyle::Block,
            cursor_blinking: false,
            dirty: Dirty::Clean,
            cells: vec![FrameCell::default(); 4],
            marked: None,
        };
        // col0: 宽字形（中文，选中 + 下划线 + 删除线）；col1: 占位格
        //（也开了下划线——必须被跳过）；col2: 窄字形下划线；col3: 空白。
        frame.cells[0] = FrameCell {
            text: "中".into(),
            wide: CellWideKind::Wide,
            selected: true,
            underline: true,
            strikethrough: true,
            ..FrameCell::default()
        };
        frame.cells[1] = FrameCell {
            wide: CellWideKind::SpacerTail,
            underline: true,
            ..FrameCell::default()
        };
        frame.cells[2] = FrameCell {
            text: "b".into(),
            underline: true,
            ..FrameCell::default()
        };

        let sel = [0.3f32, 0.4, 0.5, 0.6];
        let mut verts = Vec::new();
        build_cell_pass(
            &mut verts,
            &frame,
            (cw, ch, baseline),
            sel,
            Rgb(255, 255, 255),
            &mut atlas,
            &mut font,
        );
        let quads = as_quads(&verts);

        // ① 选区背景：一块 cell 高、两格宽、起点 col0，颜色 = 选区色。
        let bg: Vec<&Quad> = quads.iter().filter(|q| (q.y1 - q.y0 - ch).abs() < 0.01).collect();
        assert_eq!(bg.len(), 1, "选中背景 quad 应只有一块（宽字形跨两格）");
        assert!((bg[0].x1 - bg[0].x0 - 2.0 * cw).abs() < 0.5, "宽字形背景必须两格宽");
        assert_eq!(bg[0].x0, 0.0);
        assert_eq!(bg[0].rgba, sel);

        // ②/③ 装饰条：高度 1 的 quad（下划线在 baseline+1，删除线在其上）。
        // 宽字形下划线两格宽；窄字形下划线一格宽；占位格的下划线不存在。
        let bars: Vec<&Quad> = quads
            .iter()
            .filter(|q| (q.y1 - q.y0 - 1.0).abs() < 0.01)
            .collect();
        assert_eq!(bars.len(), 3, "下划线×2 + 删除线×1（占位格被跳过）");
        let mut underline: Vec<f64> = bars
            .iter()
            .filter(|q| (q.y0 - baseline - 1.0).abs() < 0.01)
            .map(|q| q.x1 - q.x0)
            .collect();
        underline.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(underline.len(), 2, "两条下划线（宽 + 窄）");
        assert!((underline[0] - cw).abs() < 0.5, "窄下划线一格宽");
        assert!((underline[1] - 2.0 * cw).abs() < 0.5, "宽字形下划线必须两格宽");
        // 删除线（宽字形）也两格宽。
        let strike: Vec<&Quad> = bars
            .iter()
            .filter(|q| (q.y0 - (baseline - ch * 0.28)).abs() < 0.01)
            .copied()
            .collect();
        assert_eq!(strike.len(), 1);
        assert!((strike[0].x1 - strike[0].x0 - 2.0 * cw).abs() < 0.5, "宽字形删除线必须两格宽");

        // ②占位格无任何自有 quad：背景1 + 下划线2 + 删除线1 + 字形2 = 6。
        assert_eq!(quads.len(), 6, "占位格不得贡献 quad");
        // 字形 quad：中（回退字体槽）与 b（基础字体槽）都拿得到槽位。
        assert_eq!(atlas.cached_count(), 2);
    }
}
