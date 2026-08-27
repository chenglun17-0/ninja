//! 字形 atlas：按 (文本, 粗, 斜) 缓存光栅化结果，摊到一张 R8Unorm 纹理上。
//!
//! 分配是简单的 shelf/行分配器：行高等于 cell 行高（设备像素），一行放满
//! 换下一行；整版放满就清空重来（缓存条目同时作废，按需重新光栅化）。
//! p1 不做 LRU 淘汰——2000+ 槽位对单终端足够，满了整版重排成本也可接受。

use std::collections::HashMap;

use crate::font::{Font, RasterGlyph, Weight};

/// atlas 里的槽位（像素坐标，设备像素）。`baseline_to_top`：位图顶
/// 相对基线的 y 偏移（负 = 基线上方），摆放字形用。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub baseline_to_top: f64,
    /// cell 内左偏移（ink 左沿 - 1px，可为负）。
    pub dx: f64,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct GlyphKey {
    text: String,
    weight: Weight,
}

/// 待上传到 GPU 的区域（渲染器每帧消费）。
pub struct PendingUpload {
    pub region: GlyphRect,
    pub bytes: Vec<u8>,
}

pub struct GlyphAtlas {
    edge: u32,
    row_h: u32,
    /// 当前行已用宽。
    cursor_x: u32,
    cursor_y: u32,
    map: HashMap<GlyphKey, GlyphRect>,
    pending: Vec<PendingUpload>,
    /// 1x1 白块：实心 quad（背景/光标/下划线）走同一管线。
    white: GlyphRect,
    total_slots: usize,
    /// 满版 reset 次数。渲染器用它检测「本帧已建 quad 的槽位被 reset
    /// 作废」——reset 只由光栅化新字形触发，空闲帧恒为 0。
    resets: u32,
}

impl GlyphAtlas {
    pub const EDGE: u32 = 2048;

    /// `row_h_px`：cell 行高（设备像素），决定 shelf 行高。
    pub fn new(row_h_px: u32) -> Self {
        let row_h = row_h_px.clamp(4, 256);
        // 第一个槽留给白色 1x1。
        let white = GlyphRect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
            baseline_to_top: 0.0,
            dx: 0.0,
        };
        Self {
            edge: Self::EDGE,
            row_h,
            cursor_x: white.w,
            cursor_y: 0,
            map: HashMap::new(),
            pending: vec![PendingUpload {
                region: white,
                bytes: vec![255],
            }],
            white,
            total_slots: 0,
            resets: 0,
        }
    }

    pub fn white(&self) -> GlyphRect {
        self.white
    }

    pub fn edge(&self) -> u32 {
        self.edge
    }

    /// 取槽位；未命中则光栅化并占位。返回 None = 没法画（光栅化失败）。
    pub fn get_or_rasterize(
        &mut self,
        text: &str,
        weight: Weight,
        font: &mut Font,
    ) -> Option<GlyphRect> {
        let key = GlyphKey {
            text: text.to_string(),
            weight,
        };
        if let Some(&r) = self.map.get(&key) {
            return Some(r);
        }

        let max_w = 4.0 * font.metrics.cell_w * font.scale;
        let glyph: RasterGlyph = font.rasterize(text, weight, max_w)?;
        if glyph.w == 0 || glyph.h == 0 {
            return None;
        }

        // 本行放不下 → 换行；纵向也满 → 整版清空，从头再放。
        if self.cursor_x + glyph.w > self.edge {
            self.new_row();
        }
        if self.cursor_y + self.row_h > self.edge {
            self.reset();
        }

        let rect = GlyphRect {
            x: self.cursor_x,
            y: self.cursor_y,
            w: glyph.w,
            h: glyph.h,
            baseline_to_top: glyph.baseline_to_top,
            dx: glyph.dx,
        };
        self.cursor_x += glyph.w;
        self.map.insert(key, rect);
        self.total_slots += 1;
        self.pending.push(PendingUpload {
            region: rect,
            bytes: glyph.coverage,
        });
        Some(rect)
    }

    fn new_row(&mut self) {
        self.cursor_x = 0;
        self.cursor_y += self.row_h;
    }

    fn reset(&mut self) {
        self.map.clear();
        self.total_slots = 0;
        self.pending.clear();
        self.resets += 1;
        // 白块重传。
        self.pending.push(PendingUpload {
            region: self.white,
            bytes: vec![255],
        });
        self.cursor_x = self.white.w;
        self.cursor_y = 0;
    }

    /// 渲染器每帧取走上传列表。
    pub fn take_pending(&mut self) -> Vec<PendingUpload> {
        std::mem::take(&mut self.pending)
    }

    pub fn cached_count(&self) -> usize {
        self.total_slots
    }

    /// 满版 reset 累计次数（帧首尾各读一次，变了 = 本帧发生过 reset）。
    pub fn resets(&self) -> u32 {
        self.resets
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_and_evicts() {
        let mut font = Font::new(13.0, 2.0);
        let mut atlas = GlyphAtlas::new(32);
        let a = atlas.get_or_rasterize("A", Weight::Regular, &mut font).unwrap();
        assert!(a.w > 0 && a.h > 0);
        // 命中缓存：同 key 返回同槽。
        let a2 = atlas.get_or_rasterize("A", Weight::Regular, &mut font).unwrap();
        assert_eq!(a, a2);
        assert_eq!(atlas.cached_count(), 1);

        // 槽位不重叠：不同 key 落不同 x。
        let b = atlas.get_or_rasterize("B", Weight::Regular, &mut font).unwrap();
        assert_ne!(a.x, b.x);

        // 上传队列包含白块 + 两个字形。
        let pending = atlas.take_pending();
        assert!(pending.len() >= 3);
        assert!(pending.iter().any(|u| u.region == atlas.white()));
        // 取走后清空。
        assert!(atlas.take_pending().is_empty());
    }
}
