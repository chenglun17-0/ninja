//! CoreText 字体度量 + 光栅化（STACK.md：字体走 CoreText，不自带字体引擎）。
//!
//! 一个 [`Font`] 持 regular/bold/italic/bold-italic 四个 CTFont 变体，
//! 给出 cell 度量（宽 = 'M' advance，高 = ascent+descent+leading），
//! 把任意字符串（grapheme cluster，含中文/emoji 序列）按 CTLine 光栅化成
//! 8-bit 灰度覆盖率位图：黑底白字，像素取反即覆盖率，直接喂 atlas。
//!
//! G-字形回退：单字体（Menlo）覆盖不了的字形原来靠 CTLine 隐式 cascade，
//! PUA（Powerline U+E0B0 系）会落到 LastResort 豆腐。现在的解析链
//! （全部 CoreText 系统级，禁止打包字体/第三方字体引擎——STACK 红线）：
//!
//! 1. 基础字体覆盖（`CTFontGetGlyphsForCharacters` 全非 0）→ 用基础字体；
//! 2. `CTFontCreateForString`（沿系统 cascade list）选出的回退字体
//!    真覆盖 → 用它（中文/假名 → PingFang，emoji → AppleColorEmoji）；
//!    返回 LastResort = cascade 无源（PUA 常见）；
//! 3. 惰性扫一次全字体集合（CTFontCollection）找覆盖源——用户装的
//!    Powerline/Nerd 字体不在系统 cascade list 里，但字体在就渲染；
//! 4. 三步全空 → 残留如实记录（[`Font::residuals`]）+ 渲染回基础字体
//!    （CTLine 自动回退画 LastResort 豆腐，不假装有字形，不打包字体）。
//!
//! 解析按「簇首字符」缓存（[`Font::resolve_slot`]），命中路径一次 HashMap
//! 查询；回退字体注册进 [`Font::fallbacks`]，atlas 槽位按 字形+字体槽
//! 双维度分开（见 atlas 模块），同一文本不会拿错字体的槽位。

use std::collections::HashMap;

use objc2_core_foundation::{
    CFArray, CFAttributedString, CFBoolean, CFDictionary, CFRetained, CFString, CGPoint, CGRect,
    CGSize,
};
use objc2_core_graphics::{CGColorSpace, CGContext};
use objc2_core_text::{
    kCTFontAttributeName, kCTForegroundColorFromContextAttributeName, CTFont,
    CTFontCollection, CTFontDescriptor, CTFontOrientation, CTFontSymbolicTraits, CTLine,
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

/// 一个已解析的回退字体槽：描述符级基础字体 + 按字重惰性派生的变体。
/// （slot 0 恒为基础等宽字体，不进这张表。）
struct FallbackFont {
    /// PostScript 名（取证/残留区分用）。
    postscript: String,
    /// 光栅化尺寸的基础引用（cascade 或集合扫描选出）。
    base: CFRetained<CTFont>,
    /// 按字重派生（copy_with_symbolic_traits，失败回落 base）。
    variants: HashMap<Weight, CFRetained<CTFont>>,
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
    /// 光栅化字号（设备像素；集合扫描造字体用）。
    raster_pt: f64,
    /// G 回退：簇首字符 → 字体槽（0=基础）。命中路径一次查询。
    resolve_cache: HashMap<char, u32>,
    /// 已注册回退字体（槽位 i ↔ fallbacks[i-1]）。
    fallbacks: Vec<FallbackFont>,
    /// 惰性加载的全字体描述符集合（CTFontCollection，扫 PUA 覆盖源用）。
    scan: Option<CFRetained<CFArray>>,
    /// 三步解析全空、确实无覆盖源的码点（如实记录，不假装有字形）。
    residuals: Vec<char>,
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
    /// 默认等宽字体（Menlo）。`scale`：设备缩放（retina = 2.0）。
    pub fn new(size_pt: f64, scale: f64) -> Self {
        Self::with_family(size_pt, scale, None)
    }

    /// 指定字体族（p2 配置）；None 或字体不可用（CTFontCreateWithName
    /// 会静默换成替补字体）→ 回退 Menlo。非等宽字体不拒绝（用户选择），
    /// 但仍要求能取到度量。
    pub fn with_family(size_pt: f64, scale: f64, family: Option<&str>) -> Self {
        let named = family.and_then(|f| named_monospace(f, size_pt));
        let base = named
            .as_ref()
            .map(|f: &CFRetained<CTFont>| {
                // SAFETY: 合法 retain 过的 CTFont，再 retain 一个所有权。
                unsafe { CFRetained::retain(std::ptr::NonNull::from(&**f)) }
            })
            .unwrap_or_else(|| default_monospace(size_pt));
        let metrics = measure(&base, size_pt);


        let raster_pt = size_pt * scale;
        let raster_base = named.map_or_else(
            || default_monospace(raster_pt),
            |base| {
                // SAFETY: 同一 CTFont，再 retain 一个光栅化尺寸的引用。
                unsafe { CFRetained::retain(std::ptr::NonNull::from(&*base)) }
            },
        );
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
            raster_pt,
            resolve_cache: HashMap::new(),
            fallbacks: Vec::new(),
            scan: None,
            residuals: Vec::new(),
        }
    }

    pub fn font(&self, weight: Weight) -> &CTFont {
        self.fonts
            .get(&weight)
            .unwrap_or_else(|| self.fonts.get(&Weight::Regular).unwrap())
    }

    /// 解析 `text`（grapheme cluster）应渲染自哪个字体槽：0 = 基础等宽
    /// 字体，>0 = [`Font::fallbacks`] 里的回退槽。按簇首字符缓存，命中
    /// 路径零光栅化；atlas 用槽位做第二维 key（见 atlas 模块头）。
    /// 解析链见模块头（覆盖 → cascade → 集合扫描 → 残留）。
    pub fn resolve_slot(&mut self, text: &str) -> u32 {
        let Some(first) = text.chars().next() else {
            return 0;
        };
        if let Some(&slot) = self.resolve_cache.get(&first) {
            return slot;
        }
        let slot = self.resolve_slow(first);
        self.resolve_cache.insert(first, slot);
        slot
    }

    /// 慢路径：按单码点（簇首）三步解析。全空时记残留并回 0。
    fn resolve_slow(&mut self, first: char) -> u32 {
        let mut buf = [0u8; 4];
        let probe: &str = first.encode_utf8(&mut buf);
        // ① 基础字体覆盖。
        if covers(self.fonts.get(&Weight::Regular).unwrap(), probe) {
            return 0;
        }
        // ② CTFontCreateForString：系统级 cascade（range 必须给真实长度）。
        // SAFETY: probe 是活的 CFString；range 覆盖全串。
        let cascade = unsafe {
            let cf = CFString::from_str(probe);
            self.fonts
                .get(&Weight::Regular)
                .unwrap()
                .for_string(
                    &cf,
                    objc2_core_foundation::CFRange {
                        location: 0,
                        length: probe.encode_utf16().count() as isize,
                    },
                )
        };
        // SAFETY: 参数平凡。
        let cascade_ps = unsafe { cascade.post_script_name().to_string() };
        if cascade_ps != "LastResort" && covers(&cascade, probe) {
            return self.push_fallback(cascade, cascade_ps);
        }
        // ③ cascade 无源（LastResort 或返回的基础字体本身不覆盖）：扫
        //    全字体集合——用户装的 Powerline/Nerd 字体不在系统 cascade
        //    list 里，但字体在就渲染（「有回退源就渲染」）。
        if let Some(found) = self.scan_covering(probe) {
            // SAFETY: 参数平凡。
            let ps = unsafe { found.post_script_name().to_string() };
            return self.push_fallback(found, ps);
        }
        // ④ 残留如实记录：渲染回基础字体（CTLine 自动回退画 LastResort
        //    豆腐）。不打包 Nerd Font（STACK 红线）。
        eprintln!(
            "ninja: U+{:04X} 无系统回退源（豆腐如实呈现；不打包字体）",
            first as u32
        );
        self.residuals.push(first);
        0
    }

    /// 注册一个回退字体槽，返回槽位号。同一 PostScript 名去重：同一
    /// 回退字体（如全部 emoji → AppleColorEmoji）只占一个槽。X1 前的
    /// 实现每个码点各推一个新槽（即使解析出的是同一字体），槽表随
    /// 会话码点数无界膨胀，atlas 也跟着多建一堆等价命名空间。
    fn push_fallback(&mut self, font: CFRetained<CTFont>, postscript: String) -> u32 {
        if let Some(i) = self
            .fallbacks
            .iter()
            .position(|f| f.postscript == postscript)
        {
            return (i + 1) as u32;
        }
        self.fallbacks.push(FallbackFont {
            postscript,
            base: font,
            variants: HashMap::new(),
        });
        self.fallbacks.len() as u32
    }

    /// 惰性加载全字体描述符集合，扫一个覆盖 `text` 的字体（先到先得）。
    fn scan_covering(&mut self, text: &str) -> Option<CFRetained<CTFont>> {
        if self.scan.is_none() {
            // SAFETY: 参数平凡（nil options = 系统默认匹配规则）。
            self.scan = unsafe {
                CTFontCollection::from_available_fonts(None)
                    .matching_font_descriptors()
            };
        }
        let arr = self.scan.as_ref()?;
        // SAFETY: CoreText 声明元素类型为 CTFontDescriptor。
        let arr: &CFArray<CTFontDescriptor> = unsafe { arr.cast_unchecked() };
        // SAFETY: 索引在界内；descriptor 参数平凡。
        unsafe {
            for i in 0..arr.len() {
                let Some(d) = arr.get(i) else { continue };
                let f = CTFont::with_font_descriptor(&d, self.raster_pt, std::ptr::null());
                if covers(&f, text) {
                    return Some(f);
                }
            }
        }
        None
    }

    /// 取槽位在指定字重下实际使用的字体（retain 一份所有权给光栅化）。
    /// 回退槽的字重变体惰性派生（copy_with_symbolic_traits，失败回 base）。
    fn take_font_for(&mut self, slot: u32, weight: Weight) -> CFRetained<CTFont> {
        let base_ref: &CFRetained<CTFont> = if slot == 0 {
            self.fonts.get(&weight).unwrap_or_else(|| {
                self.fonts.get(&Weight::Regular).unwrap()
            })
        } else {
            let fb = self
                .fallbacks
                .get_mut(slot as usize - 1)
                .expect("slot registered");
            if !fb.variants.contains_key(&weight) {
                let v = match weight {
                    // SAFETY: 合法 retain 过的 CTFont，再 retain 一个所有权。
                    Weight::Regular => unsafe {
                        CFRetained::retain(std::ptr::NonNull::from(&*fb.base))
                    },
                    Weight::Bold => variant(&fb.base, true, false),
                    Weight::Italic => variant(&fb.base, false, true),
                    Weight::BoldItalic => variant(&fb.base, true, true),
                };
                fb.variants.insert(weight, v);
            }
            fb.variants.get(&weight).unwrap()
        };
        // SAFETY: 合法 retain 过的 CTFont，再 retain 一个所有权。
        unsafe { CFRetained::retain(std::ptr::NonNull::from(&**base_ref)) }
    }

    /// 取证探针：`text` 解析到的字体 PostScript 名（"Menlo-Regular" =
    /// 基础字体；回退槽给回退字体名）。测试/取证用，热路径不走。
    pub fn font_postscript_of(&mut self, text: &str) -> String {
        let slot = self.resolve_slot(text);
        if slot == 0 {
            // SAFETY: 参数平凡。
            unsafe {
                self.fonts
                    .get(&Weight::Regular)
                    .unwrap()
                    .post_script_name()
                    .to_string()
            }
        } else {
            self.fallbacks[slot as usize - 1].postscript.clone()
        }
    }

    /// 残留码点（三步解析全空：系统确实无覆盖源）。取证/测试用。
    pub fn residuals(&self) -> &[char] {
        &self.residuals
    }

    /// 光栅化一个 grapheme cluster。`max_w_px` 是位图宽上限（设备像素，
    /// cell 宽的倍数），防超宽 emoji 序列把 atlas 行撑爆。
    pub fn rasterize(&mut self, text: &str, weight: Weight, max_w_px: f64) -> Option<RasterGlyph> {
        if text.is_empty() {
            return None;
        }
        // G 回退：光栅化用解析出的字体（0=基础，>0=回退槽），不再单盯
        // Menlo——簇内个别字符回退字体不覆盖时由 CTLine 自动 cascade 兜底。
        let slot = self.resolve_slot(text);
        let font = self.take_font_for(slot, weight);

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

/// 字体是否真覆盖字符串：逐 UTF-16 单元取字形，glyph id 全非 0 才算。
/// 代理对（astral：emoji/CJK 扩展）字形只落在高代理槽，低代理槽的 0
/// 不算缺失；变体选择子（U+FE00..FE0F）与 ZWJ 容忍 0（布局器自会处理）。
fn covers(font: &CTFont, s: &str) -> bool {
    let utf16: Vec<u16> = s.encode_utf16().collect();
    let mut glyphs = vec![0u16; utf16.len()];
    // SAFETY: 输入/输出缓冲按 count 配对，单次同步调用。
    let ok = unsafe {
        font.glyphs_for_characters(
            std::ptr::NonNull::new(utf16.as_ptr() as *mut u16).unwrap(),
            std::ptr::NonNull::new(glyphs.as_mut_ptr()).unwrap(),
            utf16.len() as isize,
        )
    };
    if !ok {
        return false;
    }
    let mut i = 0;
    while i < glyphs.len() {
        let u = utf16[i];
        if (0xD800..0xDC00).contains(&u) {
            // 高代理：字形在首槽，下一槽（低代理）必为 0。
            if glyphs[i] == 0 {
                return false;
            }
            i += 2;
        } else if (0xFE00..=0xFE0F).contains(&u) || u == 0x200D {
            i += 1;
        } else if glyphs[i] == 0 {
            return false;
        } else {
            i += 1;
        }
    }
    true
}

/// 按名字取字体；CTFontCreateWithName 对不存在的名字会静默回退系统字体，
/// 这里比对 family 名，对不上就当不可用（让上层回退 Menlo）。
fn named_monospace(name: &str, size_pt: f64) -> Option<CFRetained<CTFont>> {
    let cf = CFString::from_str(name);
    // SAFETY: 参数平凡。
    let font = unsafe { CTFont::with_name(&cf, size_pt, std::ptr::null()) };
    let family = unsafe { font.family_name() };
    let got = family.to_string();
    if got.eq_ignore_ascii_case(name.trim()) {
        Some(font)
    } else {
        None
    }
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
    fn family_config_falls_back_to_menlo() {
        // 不存在的字体族：CTFontCreateWithName 静默回退系统字体，
        // with_family 必须识别并退回 Menlo（度量与默认一致）。
        let fallback = Font::with_family(13.0, 2.0, Some("No Such Font 42"));
        let default = Font::new(13.0, 2.0);
        assert_eq!(fallback.metrics.cell_w, default.metrics.cell_w);
        assert_eq!(fallback.metrics.cell_h, default.metrics.cell_h);
        // 存在的族正常加载。
        let mono = Font::with_family(13.0, 2.0, Some("Menlo"));
        assert_eq!(mono.metrics.cell_w, default.metrics.cell_w);
        assert_eq!(Font::with_family(13.0, 2.0, None).metrics.cell_w, default.metrics.cell_w);
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

    /// G 回归：CoreText 系统级回退验收——样本逐类像素探针（非空白）+ 字体解析
    /// （非豆腐：解析不到 LastResort）。六类：制表画框/符号/Powerline/
    /// 中日/emoji/变音符-希腊-西里尔。
    #[test]
    fn fallback_renders_acceptance_categories() {
        let mut f = Font::new(13.0, 2.0);
        let max_w = 4.0 * f.metrics.cell_w * f.scale;

        let cats: [(&str, &[&str]); 7] = [
            ("box", &["│","┌","┐","└","┘","├","┤","┬","┴","┼","═","║"]),
            ("sym", &["→","←","⇄","✓","✗","●","▲","△","◆","★","☆"]),
            ("cjk", &["中","文","日","本","か","な","漢","あ"]),
            ("emoji", &["\u{1F600}","\u{1F389}","\u{1F44D}"]),
            ("latin", &["é","à","ü","ß","ñ","ő"]),
            ("greek", &["Ω","α","β","λ"]),
            ("cyr", &["П","р","и","в","е","т"]),
        ];
        for (name, samples) in &cats {
            for s in *samples {
                let ps = f.font_postscript_of(s);
                assert_ne!(ps, "LastResort", "{name} {s:?} 不得落到 LastResort 豆腐");
                let g = f
                    .rasterize(s, Weight::Regular, max_w)
                    .unwrap_or_else(|| panic!("rasterize {name} {s:?}"));
                let ink = g.coverage.iter().filter(|&&c| c > 40).count();
                assert!(ink > 0, "{name} {s:?} ({ps}) 空白位图");
            }
        }

        // 字体槽双维度的事实：ASCII 基础槽、中文/emoji 各自回退槽互不相同。
        assert_eq!(f.resolve_slot("A"), 0);
        let cjk = f.resolve_slot("中");
        let emoji = f.resolve_slot("\u{1F600}");
        assert_ne!(cjk, 0);
        assert_ne!(emoji, 0);
        assert_ne!(cjk, emoji);
        // 解析按簇首字符缓存：重复解析稳定。
        assert_eq!(f.resolve_slot("中"), cjk);
        assert!(f.residuals().is_empty(), "六类样本不得有残留");
    }

    /// G 回归（Powerline U+E0B0 系）：有回退源就渲染（非 LastResort、
    /// E0B0/E0B2 位图不得是同一张豆腐）；系统确实没有覆盖源时如实记
    /// 残留（不打包 Nerd Font——STACK 红线）。本机取证：ProFontForPowerline
    /// 覆盖 E0B0-E0B3。
    #[test]
    fn powerline_renders_or_records_residual() {
        let mut f = Font::new(13.0, 2.0);
        let max_w = 4.0 * f.metrics.cell_w * f.scale;
        let pl = '\u{E0B0}';
        let ps = f.font_postscript_of("\u{E0B0}");
        if !f.residuals().contains(&pl) {
            // 有回退源：真字形（非豆腐），且左右三角不是同一张位图。
            assert_ne!(ps, "LastResort", "E0B0 不得渲染自 LastResort");
            assert_ne!(ps, f.font_postscript_of("A"), "E0B0 不得静默回基础字体");
            let right = f.rasterize("\u{E0B0}", Weight::Regular, max_w).unwrap();
            let left = f.rasterize("\u{E0B2}", Weight::Regular, max_w).unwrap();
            assert!(right.coverage.iter().filter(|&&c| c > 40).count() > 0, "E0B0 空白");
            assert!(left.coverage.iter().filter(|&&c| c > 40).count() > 0, "E0B2 空白");
            assert_ne!(right.coverage, left.coverage, "E0B0/E0B2 位图一致 = 豆腐");
            assert!(!f.residuals().contains(&pl), "有覆盖源时不得记残留");
        } else {
            // 无覆盖源：残留如实记录 + 渲染回基础字体（CTLine 自动回退）。
            assert!(f.residuals().contains(&pl), "无覆盖源必须如实记残留");
        }
    }

    /// G 回归：确实无覆盖源的码点（未分配码位，系统字体不会有）记残留、
    /// 渲染不 panic（CTLine 自动回退画豆腐）。
    #[test]
    fn no_source_codepoint_records_residual() {
        let mut f = Font::new(13.0, 2.0);
        let unassigned = '\u{0378}'; // Latin 扩展区未分配码位
        let ps = f.font_postscript_of(unassigned.encode_utf8(&mut [0u8; 4]));
        assert!(
            f.residuals().contains(&unassigned),
            "U+0378 应记残留（解析到 {ps}）"
        );
        let max_w = 4.0 * f.metrics.cell_w * f.scale;
        // 光栅化不 panic（豆腐/空白均可接受——系统确实没有字形）。
        let _ = f.rasterize("\u{0378}", Weight::Regular, max_w);
        // 残留码点缓存稳定：重复解析不再扫集合。
        assert_eq!(f.resolve_slot("\u{0378}"), 0);
    }

    /// X1 回归：图形字符（🔒 一类 emoji/符号、robbyrussell 提示符符号、
    /// Nerd Font PUA 图标）不得渲染成 '~'。每个样本：解析字体非
    /// LastResort（三步解析构造上已验有该码点字形）、位图非空白、且
    /// 位图 ≠ 解析字体自己的 '~' 位图；同一回退字体的码点共享槽位。
    #[test]
    fn graphical_chars_not_rendered_as_tilde() {
        let mut f = Font::new(13.0, 2.0);
        let max_w = 4.0 * f.metrics.cell_w * f.scale;
        let samples = [
            '\u{1F510}', '\u{1F511}', '\u{1F512}', '\u{1F513}', '\u{1F514}', // 🔐🔑🔒🔓🔔
            '\u{1F600}', '\u{1F389}', '\u{231A}',                              // 😀🎉⌚
            '\u{279C}',                                                          // ➜ 提示符箭头
            '\u{2717}',                                                          // ✗ 提示符叉
            '\u{E0B0}', '\u{E0B2}', '\u{F108}',                                  // Powerline/Nerd PUA
        ];
        let mut tilde_by_ps: HashMap<String, Vec<u8>> = HashMap::new();
        for &c in &samples {
            let s = c.to_string();
            let ps = f.font_postscript_of(&s);
            assert_ne!(ps, "LastResort", "U+{:04X} 不得落到 LastResort", c as u32);
            assert!(
                !f.residuals().contains(&c),
                "U+{:04X} 本机应有覆盖源",
                c as u32
            );
            let g = f
                .rasterize(&s, Weight::Regular, max_w)
                .unwrap_or_else(|| panic!("rasterize U+{:04X}", c as u32));
            let ink = g.coverage.iter().filter(|&&v| v > 40).count();
            assert!(ink > 0, "U+{:04X} ({ps}) 空白位图", c as u32);
            if !tilde_by_ps.contains_key(&ps) {
                let t = f.rasterize("~", Weight::Regular, max_w).unwrap();
                tilde_by_ps.insert(ps.clone(), t.coverage);
            }
            let tilde = &tilde_by_ps[&ps];
            assert_ne!(
                g.coverage, *tilde,
                "U+{:04X} ({ps}) 位图 == 该字体的 '~' 位图",
                c as u32
            );
        }
        // 槽位去重（X1）：同一回退字体（AppleColorEmoji）的码点共享槽。
        let a = f.resolve_slot("\u{1F512}");
        let b = f.resolve_slot("\u{1F513}");
        assert_ne!(a, 0);
        assert_eq!(a, b, "同一回退字体应共享槽位（AppleColorEmoji）");
        assert_eq!(f.resolve_slot("\u{1F512}"), a, "解析缓存稳定");
        assert!(f.residuals().is_empty());
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
