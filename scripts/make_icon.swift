// Ninja 应用图标：程序化绘制（Swift/CoreGraphics，扁平风）。
//
// 用法：swift scripts/make_icon.swift <iconset-dir>
//     生成 Apple iconset 标准 10 个 PNG：icon_16x16 … icon_512x512@2x(=1024)，
//     供 iconutil -c icns 合成 .icns（编排与自检见 scripts/make_icon.sh）。
//   swift scripts/make_icon.swift --sample <png> <x> <y>
//     打印采样像素 "#RRGGBB a=NNN"（y 从顶部数）——给 make_icon.sh 的
//     回归自检用：验证特征色（底/兜帽/头带/眼缝/眼/飘带）落位与透明角。
//
// 设计（用户确认 2026-08-29）：
//   - macOS 圆角方形底，深底 #282C34 呼应 One Dark Pro（T1 唯一内置配色）；
//   - 简洁忍者头：兜帽 + 头带（ODP 红 #E06C75，右侧两片飘带）+ 眼缝开口，
//     缝内双眼用 ODP 蓝 #61AFEF 点亮（终端气质）；
//   - 全扁平色块，无渐变无阴影。
//
// 实现要点：几何在 1024 逻辑空间（y 轴向上）定义，每个目标尺寸独立
// 重绘（缩放 CTM 后同一套矢量路径），不是大图降采样——小尺寸由
// CoreGraphics 抗锯齿自然收敛，不糊。
import Foundation
import CoreGraphics
import ImageIO

let SPACE: CGFloat = 1024

// One Dark Pro 呼应色板（值见 crates/ninja 内置配色，DISTRIBUTION.md）
enum Pal {
    static let bg:    UInt32 = 0x282C34 // ODP 默认底色
    static let rim:   UInt32 = 0x3E4451 // 底缘细描边：暗色 Finder 壁纸下给轮廓
    static let hood:  UInt32 = 0x333842 // 兜帽（比底色亮一档，剪影可辨）
    static let band:  UInt32 = 0xE06C75 // 头带 ODP 红
    static let band2: UInt32 = 0xBE5046 // 飘带暗红（扁平双色调）
    static let slit:  UInt32 = 0x14161B // 眼缝开口（最暗）
    static let eye:   UInt32 = 0x61AFEF // 双眼 ODP 蓝
}

// 注意两个 CoreGraphics 经典坑（本文件必须保持，勿“简化”回去）：
//   1. CGColor(red:green:blue:) 创建的是 generic RGB(γ1.8) 颜色，灌进
//      sRGB 位图上下文会被色彩管理改值（实测 #282C34→#353A44）。必须
//      用 CGColor(space:components:) 以 sRGB 分量直接构造，值才字节保真。
//   2. CGBitmapContext 用户空间原点在左下、而 CGImage 首行是图像顶部，
//      直接导出会上下镜像。缩放 CTM 后翻 y 轴一次，y-up 几何即正向落图。
let srgb = CGColorSpace(name: CGColorSpace.sRGB)!

func cg(_ hex: UInt32) -> CGColor {
    let r = CGFloat((hex >> 16) & 0xFF) / 255.0
    let g = CGFloat((hex >> 8) & 0xFF) / 255.0
    let b = CGFloat(hex & 0xFF) / 255.0
    return CGColor(colorSpace: srgb, components: [r, g, b, 1.0])!
}

func rrect(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat, _ r: CGFloat) -> CGPath {
    CGPath(roundedRect: CGRect(x: x, y: y, width: w, height: h),
           cornerWidth: r, cornerHeight: r, transform: nil)
}

func quad(_ p0: (CGFloat, CGFloat), _ p1: (CGFloat, CGFloat),
          _ p2: (CGFloat, CGFloat), _ p3: (CGFloat, CGFloat)) -> CGPath {
    let path = CGMutablePath()
    path.move(to: CGPoint(x: p0.0, y: p0.1))
    path.addLine(to: CGPoint(x: p1.0, y: p1.1))
    path.addLine(to: CGPoint(x: p2.0, y: p2.1))
    path.addLine(to: CGPoint(x: p3.0, y: p3.1))
    path.closeSubpath()
    return path
}

func fill(_ ctx: CGContext, _ path: CGPath, _ hex: UInt32) {
    ctx.setFillColor(cg(hex))
    ctx.addPath(path)
    ctx.fillPath()
}

// —— 几何（1024 逻辑空间，y 向上）——
// 底：圆角方形，圆角率 ~0.224（macOS 大样）；兜帽居中 600×620。
let bgPath   = rrect(12, 12, 1000, 1000, 224)
let hoodPath = rrect(212, 202, 600, 620, 170)
// 飘带：从头带右端向右上甩出两片（根藏在头带后面）
let tailA = quad((814, 566), (956, 620), (956, 688), (814, 646)) // 主带同色
let tailB = quad((814, 646), (946, 744), (918, 806), (800, 724)) // 暗红衬带
let bandRect = CGRect(x: 212, y: 554, width: 600, height: 130)   // 裁进兜帽
let slitPath = rrect(292, 334, 440, 150, 75)                      // 眼缝（胶囊）
let eyeL     = rrect(352, 377, 120, 64, 30)                       // 双眼，眯长条
let eyeR     = rrect(552, 377, 120, 64, 30)

func paint(_ ctx: CGContext) {
    // 1. 圆角方形底 + 细描边
    fill(ctx, bgPath, Pal.bg)
    ctx.setStrokeColor(cg(Pal.rim))
    ctx.setLineWidth(10)
    ctx.addPath(bgPath)
    ctx.strokePath()
    // 2. 兜帽
    fill(ctx, hoodPath, Pal.hood)
    // 3. 飘带（先画，根部被下一步头带压住）
    fill(ctx, tailA, Pal.band)
    fill(ctx, tailB, Pal.band2)
    // 4. 头带（裁到兜帽轮廓内，边角不外溢）
    ctx.saveGState()
    ctx.addPath(hoodPath)
    ctx.clip()
    ctx.setFillColor(cg(Pal.band))
    ctx.fill(bandRect)
    ctx.restoreGState()
    // 5. 眼缝 + 6. 双眼
    fill(ctx, slitPath, Pal.slit)
    fill(ctx, eyeL, Pal.eye)
    fill(ctx, eyeR, Pal.eye)
}

// iconutil iconset 标准名 ↔ 像素尺寸（@2x 覆盖 64/256/512/1024）
let entries: [(String, Int)] = [
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024),
]

let args = CommandLine.arguments

// —— --sample：回归自检采样（y 从顶部数），打印 "#RRGGBB a=NNN" ——
if args.count == 5 && args[1] == "--sample" {
    guard let src = CGImageSourceCreateWithURL(URL(fileURLWithPath: args[2]) as CFURL, nil),
          let img = CGImageSourceCreateImageAtIndex(src, 0, nil) else { exit(1) }
    let w = img.width, h = img.height
    guard let bctx = CGContext(
        data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
        space: srgb, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else { exit(1) }
    bctx.draw(img, in: CGRect(x: 0, y: 0, width: w, height: h))
    guard let data = bctx.data else { exit(1) }
    let px = data.bindMemory(to: UInt8.self, capacity: w * h * 4)
    let x = Int(args[3])!, y = Int(args[4])!
    guard x >= 0, x < w, y >= 0, y < h else { exit(1) }
    let i = ((h - 1 - y) * w + x) * 4
    print(String(format: "#%02X%02X%02X a=%d", px[i], px[i + 1], px[i + 2], px[i + 3]))
    exit(0)
}

guard args.count == 2 else {
    FileHandle.standardError.write("用法: swift make_icon.swift <iconset-dir>\n".data(using: .utf8)!)
    exit(2)
}
let outDir = URL(fileURLWithPath: args[1], isDirectory: true)

for (name, px) in entries {
    guard let ctx = CGContext(
        data: nil, width: px, height: px,
        bitsPerComponent: 8, bytesPerRow: 0,
        space: srgb,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else {
        FileHandle.standardError.write("错误: 建 \(px)px 位图上下文失败\n".data(using: .utf8)!)
        exit(1)
    }
    let k = CGFloat(px) / SPACE
    ctx.scaleBy(x: k, y: k)
    ctx.translateBy(x: 0, y: SPACE)   // 翻 y：位图首行=图像顶部
    ctx.scaleBy(x: 1, y: -1)
    paint(ctx)
    guard let image = ctx.makeImage() else { exit(1) }
    let url = outDir.appendingPathComponent(name)
    guard let dest = CGImageDestinationCreateWithURL(
        url as CFURL, "public.png" as CFString, 1, nil
    ) else { exit(1) }
    CGImageDestinationAddImage(dest, image, nil)
    guard CGImageDestinationFinalize(dest) else { exit(1) }
}
print("ok: \(entries.count) png -> \(outDir.path)")
