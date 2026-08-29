//! 层像素绘制：宿主给的 IOSurface（global id 跨进程共享）上盖一个
//! CGContext（BGRA 预乘、字节序 32Little——与宿主 Metal 侧
//! MTLPixelFormatBGRA8Unorm 的内存布局一致），CoreText 画文本，
//! 画完解锁。宿主在同一帧的 cell pass 之上合成这块纹理。
//!
//! 坐标系：IOSurface 行 0 在上；CGContext 原点在左下 → CTM 翻转
//! （translate(0,h) + scale(1,-1)）后按左上原点画，所见即层上所见。

use ninja_protocol::LayerReady;
use objc2_core_foundation::{
    CFAttributedString, CFBoolean, CFDictionary, CFString, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::CGBitmapContextCreate;
use objc2_core_graphics::{CGColorSpace, CGImageAlphaInfo, CGImageByteOrderInfo};
use objc2_core_text::{
    kCTFontAttributeName, kCTForegroundColorFromContextAttributeName, CTFont, CTLine,
};
use objc2_io_surface::{IOSurfaceLockOptions, IOSurfaceRef};

use crate::Target;

/// 层像素配色（T-主题：与宿主同源 One Dark Pro——插件层不读宿主主题，
/// 那不是 v0 协议的一部分，但视觉必须一套；色值取自官方主题源
/// OneDark-Pro.json，键名注在行尾）。
const BG: (f64, f64, f64) = (40.0 / 255.0, 44.0 / 255.0, 52.0 / 255.0);   // editor.background #282c34
const FG: (f64, f64, f64) = (171.0 / 255.0, 178.0 / 255.0, 191.0 / 255.0); // terminal.foreground #abb2bf
const HEADER_FG: (f64, f64, f64) = (92.0 / 255.0, 99.0 / 255.0, 112.0 / 255.0); // comment token #5c6370
const LINE_HL_BG: (f64, f64, f64) = (44.0 / 255.0, 49.0 / 255.0, 60.0 / 255.0); // editor.lineHighlightBackground #2c313c
const LINE_HL_FG: (f64, f64, f64) = (215.0 / 255.0, 218.0 / 255.0, 224.0 / 255.0); // terminal.ansiWhite #d7dae0
/// 基础字号（points）；按 layer.ready 的 dpi 换算成像素。
const BASE_PT: f64 = 13.0;
/// 预览起始行的前后文（把命中行放进首屏中间偏上）。
const CONTEXT_LINES: usize = 3;

pub fn draw(target: &Target, content: &str, ready: &LayerReady) -> Result<(), String> {
    let id = u32::try_from(ready.io_surface_id)
        .map_err(|_| format!("io_surface_id {} 超 u32（IOSurfaceID 上限）", ready.io_surface_id))?;
    // SAFETY: 返回的引用由 IOSurfaceLookup retain，本进程合法持有。
    let surface = IOSurfaceRef::lookup(id).ok_or_else(|| format!("IOSurface {id} 不存在"))?;

    let w = ready.width_px as usize;
    let h = ready.height_px as usize;
    if w == 0 || h == 0 {
        return Err("层尺寸为 0".into());
    }
    // 写锁（CPU 访问 base address 前后成对；seed 不用）。
    // SAFETY: surface 合法；options 空 = read/write 锁。
    let kr = unsafe { surface.lock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()) };
    if kr != 0 {
        return Err(format!("IOSurfaceLock 失败 krc={kr}"));
    }
    let result = (|| {
        let space = CGColorSpace::new_device_rgb().ok_or("device RGB 色彩空间")?;
        let bpr = surface.bytes_per_row();
        // SAFETY: base_address/bpr/w/h 与 surface 匹配；BGRA 预乘布局
        //（byteOrder32Little + alphaFirst）与宿主 Metal 纹理一致。
        let ctx = unsafe {
            CGBitmapContextCreate(
                surface.base_address().as_ptr(),
                w,
                h,
                8,
                bpr,
                Some(&space),
                CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0,
            )
        }
        .ok_or("CGBitmapContextCreate 失败")?;
        // 翻转后按左上原点画（CGContext 原生原点在左下）。
        CGContext::translate_ctm(Some(&ctx), 0.0, h as f64);
        CGContext::scale_ctm(Some(&ctx), 1.0, -1.0);
        let r = unsafe { paint(&ctx, w, h, target, content, ready.dpi) };
        drop(ctx); // 释放上下文（不动像素）
        r
    })();
    // SAFETY: 与 lock 成对。
    let _ = unsafe { surface.unlock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()) };
    result
}

use objc2_core_graphics::CGContext;

/// 布局 + 逐行画。返回 Err = 字体/度量异常（层退回 close）。
unsafe fn paint(
    ctx: &CGContext,
    w: usize,
    h: usize,
    target: &Target,
    content: &str,
    dpi: u32,
) -> Result<(), String> {
    let scale = (dpi as f64 / 72.0).max(0.5);
    let font_px = BASE_PT * scale;
    let name = CFString::from_str("Menlo");
    // SAFETY: 参数平凡。
    let font = unsafe { CTFont::with_name(&name, font_px, std::ptr::null()) };
    let (ascent, descent, leading) = unsafe {
        (
            font.ascent() as f64,
            font.descent() as f64,
            font.leading() as f64,
        )
    };
    let line_h = (ascent + descent + leading).max(font_px * 1.2);
    let pad = (8.0 * scale).max(4.0);
    let header_h = line_h + pad * 1.5;

    // 背景（CGContext 静态方法是安全函数，参数为可空接收者）。
    CGContext::set_rgb_fill_color(Some(ctx), BG.0, BG.1, BG.2, 1.0);
    CGContext::fill_rect(
        Some(ctx),
        CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: w as f64,
                height: h as f64,
            },
        },
    );

    // 头部：路径 + 行号 + 提示。
    let header = format!(
        "{} — ninja-preview · Esc 关闭",
        target
            .path
            .display()
    );
    // SAFETY: ctx 合法；文本画在头带内。
    unsafe {
        draw_line(ctx, &header, pad, header_h - line_h - pad * 0.25, &font, HEADER_FG);
    }
    // 头带与正文之间的分隔线。
    CGContext::set_rgb_fill_color(Some(ctx), HEADER_FG.0, HEADER_FG.1, HEADER_FG.2, 0.35);
    CGContext::fill_rect(
        Some(ctx),
        CGRect {
            origin: CGPoint {
                x: 0.0,
                y: header_h,
            },
            size: CGSize {
                width: w as f64,
                height: 1.0 * scale,
            },
        },
    );

    // 正文：命中行（1 基）为中心起 CONTEXT_LINES 行上文。
    let all: Vec<&str> = content.lines().collect();
    let hit_idx = target.line.map(|l| (l as usize).saturating_sub(1));
    let start = hit_idx
        .map(|i| i.saturating_sub(CONTEXT_LINES))
        .unwrap_or(0)
        .min(all.len().saturating_sub(1));
    let visible = (((h as f64 - header_h - pad) / line_h).floor() as usize).saturating_sub(1);

    for (i, line) in all.iter().skip(start).take(visible.max(1)).enumerate() {
        let y = header_h + pad * 0.5 + (i as f64) * line_h;
        if y + line_h > h as f64 {
            break;
        }
        let is_hit = hit_idx == Some(start + i);
        // SAFETY: ctx 合法；逐行画在预算行位内。
        unsafe {
            if is_hit {
                CGContext::set_rgb_fill_color(
                    Some(ctx),
                    LINE_HL_BG.0,
                    LINE_HL_BG.1,
                    LINE_HL_BG.2,
                    1.0,
                );
                CGContext::fill_rect(
                    Some(ctx),
                    CGRect {
                        origin: CGPoint { x: 0.0, y },
                        size: CGSize {
                            width: w as f64,
                            height: line_h,
                        },
                    },
                );
            }
            let numbered = format!("{:>5} {}", start + i + 1, line);
            draw_line(
                ctx,
                &numbered,
                pad,
                y + ascent,
                &font,
                if is_hit { LINE_HL_FG } else { FG },
            );
        }
    }
    Ok(())
}

/// 单行文本：CFAttributedString{font, fg-from-context} → CTLine →
/// set_text_position + draw。`x`/`baseline_y` 是翻转后坐标系（左上原点）。
unsafe fn draw_line(
    ctx: &CGContext,
    text: &str,
    x: f64,
    baseline_y: f64,
    font: &CTFont,
    fg: (f64, f64, f64),
) {
    let cf_str = CFString::from_str(text);
    // SAFETY: 键值类型匹配（CTFontRef / CFBooleanRef）。
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
    };
    let Some(attrs) = attrs else { return };
    // SAFETY: attr_str 合法构造。
    let attr_str = match unsafe { CFAttributedString::new(None, Some(&cf_str), Some(&attrs)) } {
        Some(s) => s,
        None => return,
    };
    // SAFETY: 同上。
    let line = unsafe { CTLine::with_attributed_string(&attr_str) };
    // SAFETY: ctx 合法；fill color 经 fg-from-context 进入字形渲染。
    unsafe {
        CGContext::set_rgb_fill_color(Some(ctx), fg.0, fg.1, fg.2, 1.0);
        CGContext::set_text_position(Some(ctx), x, baseline_y);
        line.draw(ctx);
    }
}
