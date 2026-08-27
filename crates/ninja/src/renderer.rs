//! 自研 Metal cell 绘制（STACK.md：不吃 Skia/WebRender，直接吃 vt 的帧）。
//!
//! 一个管线：atlas 采样 × 顶点色。实心 quad（背景/选区/光标/下划线）用
//! atlas 里的 1x1 白块采样，字形 quad 用字形槽位采样，同一条 blend 路径。
//! 顶点走 setVertexBytes + vertex_id 解引用，无需 vertex descriptor。
//! 坐标系：设备像素，左上原点；viewport uniform 换算 NDC。

use libghostty_vt::render::CursorVisualStyle;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::{NSString};
use objc2_core_foundation::CGSize;
use objc2_metal::{
    MTLCommandEncoder, MTLBlendFactor, MTLClearColor, MTLCommandBuffer, MTLCommandQueue,
    MTLCompileOptions, MTLDevice, MTLLibrary, MTLPrimitiveType, MTLRenderCommandEncoder,
    MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLRenderPassDescriptor,
    MTLResourceOptions, MTLSamplerDescriptor, MTLSamplerMinMagFilter, MTLSamplerState,
    MTLStorageMode, MTLTexture, MTLTextureDescriptor, MTLTextureType, MTLTextureUsage,
    MTLCreateSystemDefaultDevice, MTLLoadAction, MTLStoreAction, MTLPixelFormat,
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
"#;

/// 渲染主题色（p1 硬编码；p2 配置阶段进 TOML）。
pub struct Theme {
    pub selection_bg: Rgb,
    pub cursor: Rgb,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            selection_bg: Rgb(0x35, 0x4B, 0x8C),
            cursor: Rgb(0xE6, 0xE6, 0xE6),
        }
    }
}

pub struct Renderer {
    pub device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
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
            sampler,
            atlas_texture,
            layer,
            theme: Theme::default(),
            drawable_size: (0.0, 0.0),
            cell_px,
            frames_drawn: 0,
        })
    }

    /// 画一帧：先组顶点（cell 循环里按需光栅化新字形），随后把
    /// atlas pending（含本帧新字形）上传纹理，最后才编码 draw。
    /// 顺序是 p1 黑屏缺陷的修复本体：`replaceRegion` 是 CPU 侧立即写入
    /// （Shared 存储），GPU 在同一命令缓冲 commit 后才采样 —— 本帧
    /// 光栅化的字形当帧可见，不需要「下一帧」来补上传（空闲终端没有
    /// 下一帧：无 PTY 字节、光标不闪烁时不重画）。
    /// 不额外调度重画：无新增槽位时 pending 为空、上传 no-op，无自旋。
    /// 光标可见性/闪烁相位由 view 折进 frame.cursor（None = 不画）。
    pub fn draw(&mut self, frame: &Frame, atlas: &mut GlyphAtlas, font: &mut Font) {
        let (dw, dh) = self.drawable_size;
        if dw < 1.0 || dh < 1.0 || frame.cols == 0 || frame.rows == 0 {
            return;
        }
        self.layer.setDrawableSize(CGSize {
            width: dw,
            height: dh,
        });
        let Some(drawable) = self.layer.nextDrawable() else {
            return;
        };

        // ---- 组顶点 ----
        // atlas 满版 reset 会作废本 pass 已建 quad 的槽位（map 清空、
        // 槽位重分）→ 有 reset 就重建一遍顶点。上限 3 pass：reset 只
        // 由光栅化新字形触发，空闲帧不退循环，无自旋。
        let mut verts: Vec<Vertex> = Vec::with_capacity(frame.cells.len() * 6);
        for _ in 0..3 {
            let resets_before = atlas.resets();
            verts.clear();
            self.build_verts(&mut verts, frame, atlas, font);
            if atlas.resets() == resets_before {
                break;
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
            return;
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
            return;
        };
        // SAFETY: verts 生存期覆盖 commit；顶点字节数 < 4KB 限制之外时
        // setVertexBytes 仍允许更大上限（Metal 文档上限 4KB，超限走 buffer）。
        // p1 终端尺寸上限下先按 buffer 走，避免踩 4KB 语义争议。
        let byte_len = verts.len() * std::mem::size_of::<Vertex>();
        // SAFETY: verts 指针与长度匹配，生存期覆盖 commit。
        let vertex_buffer = unsafe {
            self.device.newBufferWithBytes_length_options(
                std::ptr::NonNull::new(verts.as_ptr() as *mut std::ffi::c_void).unwrap(),
                byte_len,
                MTLResourceOptions::StorageModeShared,
            )
        };
        let Some(vertex_buffer) = vertex_buffer else {
            return;
        };
        encoder.setRenderPipelineState(&self.pipeline);
        unsafe {
            encoder.setVertexBuffer_offset_atIndex(Some(&vertex_buffer), 0, 0);
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
            encoder.endEncoding();
        }
        let drawable: &ProtocolObject<dyn objc2_metal::MTLDrawable> = drawable.as_ref();
        cmdbuf.presentDrawable(drawable);
        cmdbuf.commit();
        self.frames_drawn += 1;
    }

    /// 组一帧的全部顶点：cell 循环（背景/字形/装饰）→ IME 预编辑 →
    /// 非块光标。会经 `atlas.get_or_rasterize` 光栅化新字形并压进
    /// atlas pending（上传由 `upload_pending` 在 draw 内随后消费）。
    fn build_verts(
        &mut self,
        verts: &mut Vec<Vertex>,
        frame: &Frame,
        atlas: &mut GlyphAtlas,
        font: &mut Font,
    ) {
        let white = atlas.white();
        let edge = atlas.edge() as f32;
        let (cw, ch, baseline) = self.cell_px;
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
                bg = rgb_to_f32s(self.theme.selection_bg);
            }

            let is_cursor_cell = frame.cursor.is_some_and(|c| {
                usize::from(c.x) == i % usize::from(frame.cols)
                    && usize::from(c.y) == i / usize::from(frame.cols)
            });
            let cursor_block =
                is_cursor_cell && matches!(frame.cursor_style, CursorVisualStyle::Block);

            if bg != default_bg || selected || cursor_block {
                let bg_color = if cursor_block {
                    rgb_to_f32s(self.theme.cursor)
                } else {
                    bg
                };
                let span = if cell.wide == CellWideKind::Wide { 2.0 } else { 1.0 };
                push_quad(verts, white, edge, x0, y0, cw * span, ch, bg_color);
                if cursor_block {
                    fg = default_bg; // 块光标上字形反色
                }
            }

            // 字形。
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

            // 下划线 / 删除线。
            if cell.underline {
                push_quad(verts, white, edge, x0, y0 + baseline + 1.0, cw, 1.0, fg);
            }
            if cell.strikethrough {
                push_quad(
                    verts,
                    white,
                    edge,
                    x0,
                    y0 + baseline - ch * 0.28,
                    cw,
                    1.0,
                    fg,
                );
            }
        }

        // IME 预编辑串：从光标 cell 起按字符宽度逐字落格，下划线标记。
        if let Some(marked) = &frame.marked {
            self.draw_marked(verts, white, edge, marked, frame, default_fg, atlas, font);
        }

        // 非块光标样式：条 / 下划线 / 空心块。
        if let Some(c) = frame.cursor {
            let x0 = f64::from(c.x) * cw;
            let y0 = f64::from(c.y) * ch;
            let color = rgb_to_f32s(self.theme.cursor);
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
            rgb_to_f32s(self.theme.selection_bg),
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

        // 一帧 "bash$ " + 空白格（bash 启动提示符的最小化身）。
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
        for (i, c) in "bash$ ".chars().enumerate() {
            frame.cells[i].text = c.to_string();
        }

        let mut atlas = GlyphAtlas::new(ch);
        r.draw(&frame, &mut atlas, &mut font);

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
    }
}
