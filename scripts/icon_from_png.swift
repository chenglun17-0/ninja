// 从 assets/icon-source.png 生成 macOS iconset 全尺寸：
//   1024 母版 = 源图 aspect-fill + macOS 圆角方形 alpha 蒙版（半径比 0.2246，
//   与本仓历代图标一致），再高质量降采样出 10 个标准尺寸。
// 用法：
//   icon_from_png.swift <source.png> <iconset_dir>
//   icon_from_png.swift --sample <png> <x> <y>   # 打印 "#RRGGBB a=NNN"
import AppKit
import CoreGraphics

let SIZES: [(String, Int)] = [
    ("icon_16x16", 16), ("icon_16x16@2x", 32), ("icon_32x32", 32), ("icon_32x32@2x", 64),
    ("icon_128x128", 128), ("icon_128x128@2x", 256), ("icon_256x256", 256),
    ("icon_256x256@2x", 512), ("icon_512x512", 512), ("icon_512x512@2x", 1024),
]

func fail(_ message: String) -> Never {
    FileHandle.standardError.write("\(message)\n".data(using: .utf8)!)
    exit(1)
}

func makeContext(pxWidth: Int, pxHeight: Int) -> CGContext {
    guard let context = CGContext(
        data: nil, width: pxWidth, height: pxHeight, bitsPerComponent: 8, bytesPerRow: 0,
        space: CGColorSpace(name: CGColorSpace.sRGB)!,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else { fail("无法创建 CGContext \(pxWidth)x\(pxHeight)") }
    return context
}

@discardableResult
func sample(_ path: String, _ pixX: Int, _ pixY: Int) -> String {
    guard let image = NSImage(contentsOfFile: path),
          let cgImage = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
        fail("读不到图: \(path)")
    }
    let imgW = cgImage.width, imgH = cgImage.height
    let context = makeContext(pxWidth: 1, pxHeight: 1)
    context.interpolationQuality = .none
    // 采 (pixX,pixY)（左上原点坐标习惯）：CG 原点在左下，翻 y。
    context.draw(
        cgImage,
        in: CGRect(
            x: CGFloat(-pixX), y: CGFloat(pixY - imgH + 1),
            width: CGFloat(imgW), height: CGFloat(imgH)
        )
    )
    guard let base = context.data else { fail("无像素数据") }
    let redByte = base.load(fromByteOffset: 0, as: UInt8.self)
    let greenByte = base.load(fromByteOffset: 1, as: UInt8.self)
    let blueByte = base.load(fromByteOffset: 2, as: UInt8.self)
    let alphaByte = base.load(fromByteOffset: 3, as: UInt8.self)
    let result = String(format: "#%02X%02X%02X a=%d", redByte, greenByte, blueByte, alphaByte)
    print(result)
    exit(0)
}

let argv = CommandLine.arguments
if argv.count == 5 && argv[1] == "--sample" {
    sample(argv[2], Int(argv[3]) ?? 0, Int(argv[4]) ?? 0)
}
guard argv.count == 3 else {
    fail("用法: icon_from_png.swift <source.png> <iconset_dir>")
}
let srcPath = argv[1]
let outDir = argv[2]

guard let nsImage = NSImage(contentsOfFile: srcPath),
      let srcImage = nsImage.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
    fail("读不到源图: \(srcPath)")
}

let canvasSize = 1024
let radius = CGFloat(230.0) // 0.2246 × canvasSize

// 母版：圆角蒙版 + aspect-fill
let canvasCtx = makeContext(pxWidth: canvasSize, pxHeight: canvasSize)
canvasCtx.interpolationQuality = .high
let clipPath = CGPath(
    roundedRect: CGRect(x: 0, y: 0, width: canvasSize, height: canvasSize),
    cornerWidth: radius, cornerHeight: radius, transform: nil
)
canvasCtx.saveGState()
canvasCtx.addPath(clipPath)
canvasCtx.clip()
let longSide = CGFloat(max(srcImage.width, srcImage.height))
let drawW = CGFloat(srcImage.width) / longSide * CGFloat(canvasSize)
let drawH = CGFloat(srcImage.height) / longSide * CGFloat(canvasSize)
canvasCtx.draw(
    srcImage,
    in: CGRect(
        x: (CGFloat(canvasSize) - drawW) / 2, y: (CGFloat(canvasSize) - drawH) / 2,
        width: drawW, height: drawH
    )
)
canvasCtx.restoreGState()
guard let composedImage = canvasCtx.makeImage() else { fail("母版合成失败") }

do {
    try FileManager.default.createDirectory(atPath: outDir, withIntermediateDirectories: true)
} catch {
    fail("建目录失败: \(outDir)")
}

for (name, px) in SIZES {
    let context = makeContext(pxWidth: px, pxHeight: px)
    context.interpolationQuality = .high
    context.draw(composedImage, in: CGRect(x: 0, y: 0, width: CGFloat(px), height: CGFloat(px)))
    guard let sizedImage = context.makeImage() else {
        fail("尺寸 \(name) 失败")
    }
    let rep = NSBitmapImageRep(cgImage: sizedImage)
    guard let png = rep.representation(using: .png, properties: [:]) else {
        fail("尺寸 \(name) PNG 编码失败")
    }
    do {
        try png.write(to: URL(fileURLWithPath: "\(outDir)/\(name).png"))
    } catch {
        fail("写 \(name).png 失败: \(error)")
    }
}
print("iconset-done \(outDir)")
