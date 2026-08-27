//! CoreText 字体度量 + 光栅化（STACK.md：字体走 CoreText，不自带字体引擎）。
//!
//! 一个 [`Font`] 持 regular/bold/italic/bold-italic 四个 CTFont 变体，
//! 给出 cell 度量（宽 = 'M' advance，高 = ascent+descent+leading），
//! 把任意字符串（grapheme cluster，含中文/emoji 序列）按 CTLine 光栅化成
//! 8-bit 灰度覆盖率位图：黑底白字，像素取反即覆盖率，直接喂 atlas。

use std::collections::HashMap;

use objc2_core_foundation::{
    CFAttributedString, CFBoolean, CFDictionary, CFRetained, CFString, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{CGColorSpace, CGContext};
use objc2_core_text::{
    kCTFontAttributeName, kCTForegroundColorFromContextAttributeName, CTFont,
    CTFontOrientation, CTFontSymbolicTraits, CTLine,
};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Weight {
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

/// 一组等宽字体的度量（points，layout 用）。
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    /// 单格宽（'M' advance，points）。
    pub cell_w: f64,
    /// 单行高（ascent+descent+leading，points）。
    pub cell_h: f64,
    /// 基线到行顶（ascent，points）。
    pub ascent: f64,
    /// 下伸（points）。
    pub descent: f64,
}

/// 四变体字体 + 光栅化器。仅主线程使用（CG/CT 上下文非线程安全）。
/// 度量用基础字号（points）；光栅化用 `size_pt * scale`（设备像素），
/// 视网膜屏不清糊。
pub struct Font {
    fonts: HashMap<Weight, CFRetained<CTFont>>,
    pub metrics: Metrics,
    pub scale: f64,
    /// 位图上下文的行缓冲复用（光栅化期间独占使用）。
    scratch: Vec<u8>,
}

/// 光栅化结果：覆盖率位图（0=透明 255=实心），尺寸/偏移均为设备像素。
pub struct RasterGlyph {
    /// 位图宽（ink 宽 + 2px 边距）。
    pub w: u32,
    /// 位图高（排高 + 2px 边距）。
    pub h: u32,
    /// 位图顶相对基线的 y 偏移（负 = 在基线上方；屏幕坐标 y 向下）。
    pub baseline_to_top: f64,
    /// 位图左相对 cell 左的 x 偏移（ink 左沿 - 1px 边距，可为负）。
    pub dx: f64,
    pub coverage: Vec<u8>,
}

// CGBitmapContextCreate 在 objc2-core-graphics 0.3.2 没生成绑定
// （只有新的 CreateAdaptive），直接声明经典符号；CoreGraphics 已链接。
unsafe extern "C-unwind" {
    fn CGBitmapContextCreate(
        data: *mut std::ffi::c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: *const CGColorSpace,
        bitmap_info: u32,
    ) -> *mut CGContext;
    fn CGContextRelease(c: *mut CGContext);
    fn CGContextFillRect(c: *mut CGContext, rect: CGRect);
}

impl Font {
    /// 默认等宽字体：Menlo（系统必装）。`scale`：设备缩放（retina = 2.0）。
    pub fn new(size_pt: f64, scale: f64) -> Self {
        let base = default_monospace(size_pt);
        let metrics = measure(&base, size_pt);


        let raster_pt = size_pt * scale;
        let raster_base = default_monospace(raster_pt);
        let mut fonts = HashMap::new();
        // SAFETY: raster_base 是合法 retain 过的 CTFont，再 retain 一个所有权。
        fonts.insert(
            Weight::Regular,
            unsafe { CFRetained::retain(std::ptr::NonNull::from(&*raster_base)) },
        );
        fonts.insert(Weight::Bold, variant(&raster_base, true, false));
        fonts.insert(Weight::Italic, variant(&raster_base, false, true));
        fonts.insert(Weight::BoldItalic, variant(&raster_base, true, true));

        Self {
            fonts,
            metrics,
            scale,
            scratch: Vec::new(),
        }
    }

    pub fn font(&self, weight: Weight) -> &CTFont {
        self.fonts
            .get(&weight)
            .unwrap_or_else(|| self.fonts.get(&Weight::Regular).unwrap())
    }

    /// 光栅化一个 grapheme cluster。`max_w_px` 是位图宽上限（设备像素，
    /// cell 宽的倍数），防超宽 emoji 序列把 atlas 行撑爆。
    pub fn rasterize(&mut self, text: &str, weight: Weight, max_w_px: f64) -> Option<RasterGlyph> {
        if text.is_empty() {
            return None;
        }
        let font: &CTFont = self.font(weight);

        // CFAttributedString { font, foreground-from-context }。
        // macOS ≥10.13 的 CTLineDraw 默认忽略 context 填充色，必须显式打开
        // kCTForegroundColorFromContextAttributeName 才会用当前填充色画字形。
        let cf_str = CFString::from_str(text);
        let attrs = unsafe {
            let keys: [*const std::ffi::c_void; 2] = [
                std::ptr::from_ref(kCTFontAttributeName).cast(),
                std::ptr::from_ref(kCTForegroundColorFromContextAttributeName).cast(),
            ];
            let values: [*const std::ffi::c_void; 2] = [
                std::ptr::from_ref(&*font).cast(),
                std::ptr::from_ref(CFBoolean::new(true)).cast(),
            ];
            let mut keys_mut = keys;
            let mut values_mut = values;
            CFDictionary::new(
                None,
                keys_mut.as_mut_ptr(),
                values_mut.as_mut_ptr(),
                2,
                &objc2_core_foundation::kCFTypeDictionaryKeyCallBacks,
                &objc2_core_foundation::kCFTypeDictionaryValueCallBacks,
            )
        }?;

        let (line, _width, ascent, descent) = unsafe {
            let attr_str = CFAttributedString::new(None, Some(&cf_str), Some(&attrs))?;
            let line = CTLine::with_attributed_string(&attr_str);
            let mut ascent: f64 = 0.0;
            let mut descent: f64 = 0.0;
            let mut leading: f64 = 0.0;
            let width = line.typographic_bounds(
                &raw mut ascent,
                &raw mut descent,
                &raw mut leading,
            ) as f64;
            (line, width, ascent, descent)
        };

        // ink bounds 要在 CG 上下文上量（CoreText 用它选渲染器）：
        // 先用 1x1 哑上下文探尺寸，再开真位图。
        let space =
            CGColorSpace::new_device_gray().expect("device gray color space");
        // SAFETY: 1x1 灰度哑上下文，只用来取 bounds。
        let probe = unsafe {
            CGBitmapContextCreate(
                std::ptr::null_mut(),
                1,
                1,
                8,
                1,
                std::ptr::from_ref(&*space).cast(),
                0,
            )
        };
        let ink = if probe.is_null() {
            CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize { width: 0.0, height: 0.0 },
            }
        } else {
            // SAFETY: probe 有效且与 line 同一渲染栈。
            let r = unsafe { line.image_bounds(Some(&*probe)) };
            unsafe { CGContextRelease(probe) };
            r
        };
        if !ink.size.width.is_finite() || ink.size.width < 0.0 {
            return None; // CGRectNull / 无 ink（空字形）：不给槽位
        }

        // 位图尺寸：宽用 ink bounds（紧致，避免字形 quad 越过 cell 互相压盖），
        // 高用排高 + 2px，封顶。
        let bmp_w = ((ink.size.width + 2.0).ceil() as usize).clamp(1, (max_w_px.ceil() as usize).max(2));
        let bmp_h = ((ascent + descent + 2.0).ceil() as usize).clamp(1, 256);
        let px_len = bmp_w * bmp_h;
        if self.scratch.len() < px_len {
            self.scratch.resize(px_len, 0);
        }
        self.scratch[..px_len].fill(0);

        // SAFETY: scratch 缓冲足够大且对齐；灰度 8bpp 无 alpha（bitmapInfo=0）。
        let ctx = unsafe {
            CGBitmapContextCreate(
                self.scratch.as_mut_ptr().cast(),
                bmp_w,
                bmp_h,
                8,
                bmp_w,
                std::ptr::from_ref(&*space).cast(),
                0,
            )
        };
        if ctx.is_null() {
            return None;
        }

        // SAFETY: ctx 有效；位图坐标原点在左下（y 向上）。
        // 基线放距位图底 descent + 1：descender 在基线下最多 descent，
        // 底部留 1px 边距；基线上方剩余高度 = 位图高 - (descent+1) = ascent+1，
        // 正好容纳 ascender + 1px 顶边距。（此前误放 ascent+1，26pt 字形
        // 基线上方只剩 ~8px，上半被裁——验证阶段的「字形只占 cell 40%」。）
        // ink 左沿贴位图 x=1：文本起点 x = 1 - ink.x。
        // 背景填白（coverage=0），字形用黑填充（coverage=255）。
        unsafe {
            CGContext::set_gray_fill_color(Some(&*ctx), 1.0, 1.0);
            CGContextFillRect(
                ctx,
                CGRect {
                    origin: CGPoint { x: 0.0, y: 0.0 },
                    size: CGSize {
                        width: bmp_w as f64,
                        height: bmp_h as f64,
                    },
                },
            );
            CGContext::set_gray_fill_color(Some(&*ctx), 0.0, 1.0);
            CGContext::set_text_position(Some(&*ctx), 1.0 - ink.origin.x, descent + 1.0);
            line.draw(&*ctx);
            CGContextRelease(ctx);
        }

        // 白底黑字 → 覆盖率 = 255 - 灰度。
        let mut coverage = self.scratch[..px_len].to_vec();
        for px in coverage.iter_mut() {
            *px = 255u8.saturating_sub(*px);
        }

        Some(RasterGlyph {
            w: bmp_w as u32,
            h: bmp_h as u32,
            // 位图顶 = 基线 - (ascent + 1px 边距)；屏幕坐标 y 向下 → 负值。
            baseline_to_top: -(ascent + 1.0),
            // cell 内左偏移：ink 左沿 - 1px 边距。
            dx: ink.origin.x - 1.0,
            coverage,
        })
    }

    /// 基线在行内的 y 位置（相对行顶，points）。行高比排高多出的部分上下对半分。
    pub fn baseline_offset(&self) -> f64 {
        let m = &self.metrics;
        let slack = (m.cell_h - (m.ascent + m.descent)).max(0.0);
        m.ascent + slack / 2.0
    }
}

fn measure(font: &CTFont, size_pt: f64) -> Metrics {
    // SAFETY: font/缓冲参数布局正确，单字形测量。
    let (ascent, descent, leading, advance) = unsafe {
        let ascent = font.ascent() as f64;
        let descent = font.descent() as f64;
        let leading = font.leading() as f64;
        let mut chars: [u16; 1] = [b'M' as u16];
        let mut glyphs: [u16; 1] = [0];
        let ok = font.glyphs_for_characters(
            std::ptr::NonNull::new(chars.as_mut_ptr()).unwrap(),
            std::ptr::NonNull::new(glyphs.as_mut_ptr()).unwrap(),
            1,
        );
        let advance = if ok {
            font.advances_for_glyphs(
                CTFontOrientation::Horizontal,
                std::ptr::NonNull::new(glyphs.as_ptr() as *mut u16).unwrap(),
                std::ptr::null_mut(),
                1,
            ) as f64
        } else {
            size_pt * 0.6
        };
        (ascent, descent, leading, advance)
    };

    let line_h = ascent + descent + leading;
    Metrics {
        cell_w: advance.max(1.0),
        cell_h: line_h.max(size_pt),
        ascent,
        descent,
    }
}

fn default_monospace(size_pt: f64) -> CFRetained<CTFont> {
    let name = CFString::from_str("Menlo");
    // SAFETY: 参数平凡。
    unsafe { CTFont::with_name(&name, size_pt, std::ptr::null()) }
}

fn variant(base: &CTFont, bold: bool, italic: bool) -> CFRetained<CTFont> {
    let mut traits = CTFontSymbolicTraits::empty();
    if bold {
        traits |= CTFontSymbolicTraits::TraitBold;
    }
    if italic {
        traits |= CTFontSymbolicTraits::TraitItalic;
    }
    let mask = CTFontSymbolicTraits::TraitBold | CTFontSymbolicTraits::TraitItalic;
    unsafe {
        base.copy_with_symbolic_traits(0.0, std::ptr::null(), traits, mask)
            .unwrap_or_else(|| CFRetained::retain(std::ptr::NonNull::from(base)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_are_sane() {
        // 无窗口服务依赖：CoreText 纯用户态即可用。
        let f = Font::new(13.0, 2.0);
        assert!(f.metrics.cell_w >= 4.0 && f.metrics.cell_w <= 30.0);
        assert!(f.metrics.cell_h >= 8.0 && f.metrics.cell_h <= 40.0);
        assert!(f.metrics.ascent > 0.0 && f.metrics.descent > 0.0);
        assert!(f.baseline_offset() > 0.0 && f.baseline_offset() < f.metrics.cell_h);
    }

    #[test]
    fn rasterize_ascii_and_cjk() {
        let mut f = Font::new(13.0, 2.0);
        let max_w = 4.0 * f.metrics.cell_w * f.scale;
        for text in ["A", "中", "ni\u{1F600}"] {
            let g = f
                .rasterize(text, Weight::Regular, max_w)
                .unwrap_or_else(|| panic!("rasterize {text:?}"));
            assert!(g.w > 0 && g.h > 0, "{text:?} empty bitmap");
            assert!(
                g.coverage.iter().any(|&p| p > 40),
                "{text:?} all-blank coverage"
            );
            assert_eq!(g.coverage.len(), g.w as usize * g.h as usize);
            // 位图顶在基线上方。
            assert!(g.baseline_to_top < 0.0);
        }
        // 中文应比 ASCII 宽（宽字符占两格）。
        let a = f.rasterize("A", Weight::Regular, 320.0).unwrap();
        let cjk = f.rasterize("中", Weight::Regular, 320.0).unwrap();
        assert!(cjk.w > a.w, "CJK {} not wider than A {}", cjk.w, a.w);
    }

    /// 防回归：验证阶段发现的基线错位（基线误放距底 ascent+1）会把
    /// 26pt 字形裁到只剩基线上方 ~8px、贴位图顶。修复后：
    /// - 'M' 的 ink 高度应占位图高的大头（cap ≈ 0.73×字号，26px 字号 ≈ 19px）；
    /// - 'g' 必须跨基线：descender 在基线下（ink 底行距位图底 < descent+2），
    ///   x-height 在基线上（ink 顶行远离位图底）。
    #[test]
    fn rasterize_baseline_not_clipped() {
        let mut f = Font::new(13.0, 2.0);
        let max_w = 4.0 * f.metrics.cell_w * f.scale;
        let ascent = f.metrics.ascent * f.scale;
        let descent = f.metrics.descent * f.scale;

        let m = f.rasterize("M", Weight::Regular, max_w).unwrap();
        let ink_rows = |g: &RasterGlyph| -> (usize, usize) {
            let mut top = usize::MAX;
            let mut bottom = 0usize;
            for y in 0..g.h as usize {
                let row = &g.coverage[y * g.w as usize..(y + 1) * g.w as usize];
                if row.iter().any(|&c| c > 60) {
                    top = top.min(y);
                    bottom = bottom.max(y);
                }
            }
            (top, bottom)
        };
        let (mt, mb) = ink_rows(&m);
        let m_h = (mb - mt + 1) as f64;
        assert!(
            m_h >= (m.h as f64) * 0.5,
            "M ink only {m_h}px of {}px bitmap — clipped?",
            m.h
        );
        // M 无 descender：ink 底行应就在基线附近（距位图底 ≈ descent+1）。
        assert!(
            (m.h as usize - 1 - mb) <= (descent + 3.0) as usize,
            "M bottom row {mb} too far above bitmap bottom {}",
            m.h
        );
        // M 顶行不应贴位图顶（顶边距 ≈ ascent+1-cap ≈ 5-7px）。
        assert!(mt >= 2, "M ink touches bitmap top (row {mt}) — baseline misplaced");

        let g_glyph = f.rasterize("g", Weight::Regular, max_w).unwrap();
        let (gt, gb) = ink_rows(&g_glyph);
        // 跨基线：ink 底行在基线下方（距位图底 < descent），顶行在基线上方。
        assert!(
            gb >= ((descent + 1.0 + 3.0) as usize).min(g_glyph.h as usize - 1),
            "g ink bottom row {gb} does not cross baseline (descent+1={})",
            descent + 1.0
        );
        assert!(
            (gt as f64) < (g_glyph.h as f64) - descent - 1.0,
            "g ink top row {gt} not above baseline"
        );
        let _ = ascent;
    }
}
