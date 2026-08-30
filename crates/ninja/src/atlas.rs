//! 字形 atlas：按 (文本, 字重, 字体槽) 缓存光栅化结果，摊到一张 R8Unorm
//! 纹理上。
//!
//! 分配是简单的 shelf/行分配器：行高自适应（X1：按字形高 8px 桶取整，
//! 33/37/39 → 40——回退字体的位图高用它们自己字体的排高，比基础
//! cell 行高高；固定行高会被下一行的上传覆写先放字形的底部，行尾
//! 高字形还会把 replaceRegion 推出纹理底），一行放满换下一行；
//! 整版放满就清空重来（缓存条目同时作废，按需重新光栅化）。
//! p1 不做 LRU 淘汰——2000+ 槽位对单终端足够，满了整版重排成本也可接受。
//!
//! D-C：槽位表按字重分四张 map，命中查询走 `Borrow<str>`
//!（零分配）——旧实现每 cell 每帧 `text.to_string()` 建
//! 查询 key，重画热路径上 1920 次/帧的堆分配是 debug 构建大量输出
//! 吃力的直接成分之一。
//!
//! G-字形回退：同一文本可能渲染自不同字体（基础等宽字体 / 回退字体，
//! 见 font 模块），槽位表按「字体槽 × 文本」双维度分命名空间
//!（`HashMap<槽位, HashMap<文本, GlyphRect>>`）——字体槽从
//! `Font::resolve_slot` 拿，命中路径两次哈希零分配；换字体槽绝不复用
//! 旧字体的位图。

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

/// 待上传到 GPU 的区域（渲染器每帧消费）。
pub struct PendingUpload {
    pub region: GlyphRect,
    pub bytes: Vec<u8>,
}

/// 字重 → 槽位表下标（四变体固定）。
fn weight_idx(w: Weight) -> usize {
    match w {
        Weight::Regular => 0,
        Weight::Bold => 1,
        Weight::Italic => 2,
        Weight::BoldItalic => 3,
    }
}

pub struct GlyphAtlas {
    edge: u32,
    /// 行高下限（cell 行高，设备像素）；实际行高按字形高自适应。
    row_h: u32,
    /// 当前 shelf 行的实际高度（X1：高字形撑高所在行，只增不减）。
    cur_row_h: u32,
    /// 当前行已用宽。
    cursor_x: u32,
    cursor_y: u32,
    /// 四张槽位表（Regular/Bold/Italic/BoldItalic）；表内再按字体槽
    /// 分命名空间（G：字形+字体双维度）。查询走 Borrow<str>。
    maps: [HashMap<u32, HashMap<String, GlyphRect>>; 4],
    pending: Vec<PendingUpload>,
    /// 1x1 白块：实心 quad（背景/光标/下划线）走同一管线。
    white: GlyphRect,
    total_slots: usize,
    /// 满版 reset 次数。渲染器用它检测「本帧已建 quad 的槽位被 reset
    /// 作废」——reset 只由光栅化新字形触发，空闲帧恒为 0。
    resets: u32,
}

/// 行高桶化：按 8px 向上取整（33/37/39 → 40）。同桶字形共享一行，
/// 行内密度不因个别高字形塌缩。
fn snap_row_h(h: u32) -> u32 {
    h.div_ceil(8) * 8
}

impl GlyphAtlas {
    pub const EDGE: u32 = 2048;

    /// `row_h_px`：cell 行高（设备像素），行高下限（实际行高按字形高
    /// 自适应，见模块头）。
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
            cur_row_h: row_h,
            cursor_x: white.w,
            cursor_y: 0,
            maps: [
                HashMap::new(),
                HashMap::new(),
                HashMap::new(),
                HashMap::new(),
            ],
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
    /// 命中路径零分配（字体槽一次查 + Borrow<str> 一次查，见模块头）。
    pub fn get_or_rasterize(
        &mut self,
        text: &str,
        weight: Weight,
        font: &mut Font,
    ) -> Option<GlyphRect> {
        let slot = font.resolve_slot(text);
        if let Some(&r) = self.maps[weight_idx(weight)]
            .get(&slot)
            .and_then(|m| m.get(text))
        {
            return Some(r);
        }

        let max_w = 4.0 * font.metrics.cell_w * font.scale;
        let glyph: RasterGlyph = font.rasterize(text, weight, max_w)?;
        if glyph.w == 0 || glyph.h == 0 {
            return None;
        }

        // X1-位图错取修复：shelf 行高不再固定为 cell 行高。回退字体
        // 的字形位图高用它自己字体的 ascent+descent+2（emoji 37px /
        // 中文 39px / ASCII 33px vs cell 行高 31px）——固定行高会让高
        // 字形越进下一行，行装满换行后新字形的上传把先放字形的底部
        // 覆写（实测：中文槽位第 31 行像素被后续上传清零）；行尾高
        // 字形还会把 replaceRegion 推出纹理底部（Metal 层未定义
        // 行为）。现在行高按 8px 桶自适应（33/37/39 → 40）且只增不减
        //（行内已放字形不受影响，只是把后续行的 y 起点推低），
        // 容量检查用实际行高，放不下就整版 reset。
        let need = snap_row_h(glyph.h.max(self.row_h));
        // 本行放不下 → 换行；纵向也满 → 整版清空，从头再放。
        if self.cursor_x + glyph.w > self.edge {
            self.new_row();
        }
        self.cur_row_h = self.cur_row_h.max(need);
        if self.cursor_y + self.cur_row_h > self.edge {
            self.reset();
            self.cur_row_h = need;
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
        self.maps[weight_idx(weight)]
            .entry(slot)
            .or_default()
            .insert(text.to_string(), rect);
        self.total_slots += 1;
        self.pending.push(PendingUpload {
            region: rect,
            bytes: glyph.coverage,
        });
        Some(rect)
    }

    fn new_row(&mut self) {
        self.cursor_x = 0;
        self.cursor_y += self.cur_row_h;
        self.cur_row_h = self.row_h;
    }

    fn reset(&mut self) {
        for m in &mut self.maps {
            m.clear();
        }
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
        self.cur_row_h = self.row_h;
    }

    /// 渲染器每帧取走上传列表。
    pub fn take_pending(&mut self) -> Vec<PendingUpload> {
        std::mem::take(&mut self.pending)
    }

    /// 是否还有待上传槽位（渲染器跳帧判据之一：有 pending 必须画，
    /// 否则字形滞留 CPU 侧——空闲首开同帧上传语义依赖此兜底）。
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn cached_count(&self) -> usize {
        self.total_slots
    }

    /// 当前 shelf 行的 y 偏移（测试观察换行用）。
    pub fn cursor_y(&self) -> u32 {
        self.cursor_y
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

    /// G 回归：槽位按 字形+字体 双维度——ASCII（基础字体，槽 0）与
    /// CJK（回退字体，槽 >0）各自缓存互不冒名；同一字体槽内命中缓存。
    /// 回退字体槽位号由 Font::resolve_slot 给出（字体维度的事实源）。
    #[test]
    fn slots_keyed_by_font_dimension() {
        let mut font = Font::new(13.0, 2.0);
        let mut atlas = GlyphAtlas::new(32);

        let base_slot = font.resolve_slot("A");
        let cjk_slot = font.resolve_slot("中");
        let emoji_slot = font.resolve_slot("\u{1F600}");
        assert_eq!(base_slot, 0, "ASCII 必须落基础字体槽");
        assert_ne!(cjk_slot, base_slot, "中文必须落回退字体槽");
        assert_ne!(emoji_slot, base_slot, "emoji 必须落回退字体槽");
        assert_ne!(emoji_slot, cjk_slot, "emoji 与中文不应共享回退字体槽");

        let a = atlas.get_or_rasterize("A", Weight::Regular, &mut font).unwrap();
        let cjk = atlas.get_or_rasterize("中", Weight::Regular, &mut font).unwrap();
        assert_eq!(atlas.cached_count(), 2);
        // 命中各自缓存：同 key 同槽位。
        assert_eq!(atlas.get_or_rasterize("A", Weight::Regular, &mut font).unwrap(), a);
        assert_eq!(atlas.get_or_rasterize("中", Weight::Regular, &mut font).unwrap(), cjk);
        assert_eq!(atlas.cached_count(), 2);
        // 不同字体槽不同位图（不会拿 Menlo 的位图给 PingFang 的字形）。
        assert_ne!((a.x, a.y), (cjk.x, cjk.y));
    }

    /// X1 回归：高字形不得被下一 shelf 行的字形覆写。
    ///
    /// 缺陷（第三轮用户反馈「图形字符显示成 ~」的位图错取成分）：shelf
    /// 行高固定 = 基础 cell 行高（13pt×2 ≈ 31px），但回退字体的字形位图
    /// 高度用它自己字体的 ascent+descent+2（中文 39px / emoji 37px）——
    /// 高字形越界伸进下一 shelf 行。行装满换行后，新行的上传
    ///（`upload_pending` 按矩形 blit、后写覆盖先写）把先放的高字形
    /// 底部覆写成后来字形的顶部：图形字符（符号/图标/中文）内容被
    /// 破坏。本测试按 pending 上传顺序重放整张纹理（与 GPU 所见
    /// 一致），断言先放的高字形区域内容 == 它自己的位图。
    #[test]
    fn tall_fallback_glyphs_not_clobbered_by_row_wrap() {
        let scale = 2.0;
        let mut font = Font::new(13.0, scale);
        let row_h = (font.metrics.cell_h * scale).ceil() as u32;
        let mut atlas = GlyphAtlas::new(row_h);
        let max_w = 4.0 * font.metrics.cell_w * font.scale;

        // 前置事实：回退字形位图高 > shelf 行高（X1 的度量根源）。
        let cjk = atlas
            .get_or_rasterize("中", Weight::Regular, &mut font)
            .unwrap();
        assert!(
            cjk.h > row_h,
            "前置失效：中文位图高 {} 应 > 行高 {}（否则测不到越界）",
            cjk.h,
            row_h
        );
        let emoji = atlas
            .get_or_rasterize("\u{1F512}", Weight::Regular, &mut font)
            .unwrap();
        assert!(emoji.h > row_h);

        // 填满第一行（每个 key 唯一 → 必然换行），再放新字形。
        let mut i = 0u32;
        let y0 = cjk.y;
        while atlas.cursor_y() == y0 && i < 600 {
            atlas
                .get_or_rasterize(&format!("g{i}q"), Weight::Regular, &mut font)
                .unwrap();
            i += 1;
        }
        assert!(atlas.cursor_y() > y0, "600 个字形应已换行");
        atlas
            .get_or_rasterize("tail", Weight::Regular, &mut font)
            .unwrap();

        // 纹理重放：pending 顺序 blit（后写覆盖先写，同 GPU 语义）。
        let edge = atlas.edge() as usize;
        let mut tex = vec![0u8; edge * edge];
        for u in atlas.take_pending() {
            let r = &u.region;
            assert!(
                (r.x as usize + r.w as usize) <= edge && (r.y as usize + r.h as usize) <= edge,
                "槽位越界: {r:?}"
            );
            for y in 0..r.h as usize {
                for x in 0..r.w as usize {
                    tex[(r.y as usize + y) * edge + r.x as usize + x] =
                        u.bytes[y * r.w as usize + x];
                }
            }
        }

        // 高字形区域内容必须原样保留：与重新光栅化的位图逐像素一致。
        for (text, rect) in [("中", cjk), ("\u{1F512}", emoji)] {
            let fresh = font.rasterize(text, Weight::Regular, max_w).unwrap();
            assert_eq!((fresh.w, fresh.h), (rect.w, rect.h), "{text:?} 尺寸漂移");
            for y in 0..rect.h as usize {
                for x in 0..rect.w as usize {
                    let got = tex[(rect.y as usize + y) * edge + rect.x as usize + x];
                    let want = fresh.coverage[y * rect.w as usize + x];
                    assert_eq!(
                        got, want,
                        "{text:?} 槽位 ({},{}) 像素 ({},{}) 被后续上传覆写",
                        rect.x, rect.y, x, y
                    );
                }
            }
        }
    }

    /// X1 回归：行尾高字形不得把 replaceRegion 推出纹理底部。
    ///
    /// 缺陷成分二：旧分配器容量检查用固定行高（31px），最后一行
    /// cursor_y=2016 时 2016+31=2047 ≤ 2048 放行，但随后放入的高字形
    ///（37-39px）区域达到 y=2053——replaceRegion 越界，Metal 校验
    /// 层下 abort，无校验的 release 构建里是未定义行为（atlas 内容
    /// 损坏 → 字形错乱，用户看到的「图形字符变成别的字符」）。修复后
    /// 容量检查用实际行高：放不下就整版 reset，槽位永远在界内。
    #[test]
    fn tall_glyph_at_last_row_never_exceeds_texture() {
        let scale = 2.0;
        let mut font = Font::new(13.0, scale);
        let row_h = (font.metrics.cell_h * scale).ceil() as u32;
        let mut atlas = GlyphAtlas::new(row_h);
        let edge = atlas.edge();

        // 真实填充到接近底部：每个 key 唯一，宽度填满一行就换行。
        let mut i = 0u32;
        while atlas.cursor_y() + row_h <= edge && i < 20000 {
            atlas
                .get_or_rasterize(&format!("f{i}z"), Weight::Regular, &mut font)
                .unwrap();
            i += 1;
        }
        // 已经在最后几行：再放高字形（中文/emoji/Nerd 图标，37-39px）。
        for text in ["中", "\u{1F512}", "\u{F108}"] {
            let r = atlas
                .get_or_rasterize(text, Weight::Regular, &mut font)
                .unwrap();
            assert!(
                r.y + r.h <= edge,
                "{text:?} 槽位 ({},{}) {}x{} 越出纹理底部 {}",
                r.x,
                r.y,
                r.w,
                r.h,
                edge
            );
            assert!(r.x + r.w <= edge);
        }
        // 换行/reset 后继续可用。
        let after = atlas
            .get_or_rasterize("post", Weight::Regular, &mut font)
            .unwrap();
        assert!(after.y + after.h <= edge && after.x + after.w <= edge);
    }
}
