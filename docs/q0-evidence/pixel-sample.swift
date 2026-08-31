// ninja q0 取证：对 demo 截图采样相对矩形的平均色（RGB）。
// 用法: swift pixel-sample.swift <image.png> <x0> <y0> <x1> <y1>   (坐标为 0..1 相对比例)
// demo 窗口内容视图 940x620pt（截图含标题栏，比例采样不受影响）。
import CoreGraphics
import ImageIO
import Foundation

func avgColor(_ path: String, _ rx0: Double, _ ry0: Double, _ rx1: Double, _ ry1: Double) -> (Int, Int, Int) {
    guard let src = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
          let img = CGImageSourceCreateImageAtIndex(src, 0, nil) else {
        fatalError("cannot open \(path)")
    }
    let w = img.width, h = img.height
    let rect = CGRect(x: Int(Double(w) * rx0), y: Int(Double(h) * ry0),
                      width: Int(Double(w) * (rx1 - rx0)), height: Int(Double(h) * (ry1 - ry0)))
    guard let crop = img.cropping(to: rect) else { fatalError("crop failed") }
    let space = CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpace(name: CGColorSpace.genericRGBLinear)!
    guard let ctx = CGContext(data: nil, width: crop.width, height: crop.height,
                              bitsPerComponent: 8, bytesPerRow: crop.width * 4, space: space,
                              bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else {
        fatalError("ctx failed")
    }
    ctx.draw(crop, in: CGRect(x: 0, y: 0, width: crop.width, height: crop.height))
    guard let data = ctx.data else { fatalError("no data") }
    let px = data.bindMemory(to: UInt8.self, capacity: crop.width * crop.height * 4)
    var r = 0, g = 0, b = 0, n = 0
    for i in stride(from: 0, to: crop.width * crop.height * 4, by: 4) {
        r += Int(px[i]); g += Int(px[i + 1]); b += Int(px[i + 2]); n += 1
    }
    return (r / n, g / n, b / n)
}

let args = CommandLine.arguments
guard args.count == 6 else {
    print("usage: pixel-sample.swift <png> <x0> <y0> <x1> <y1>  (relative 0..1)")
    exit(2)
}
let (r, g, b) = avgColor(args[1], Double(args[2])!, Double(args[3])!, Double(args[4])!, Double(args[5])!)
print("(\(r),\(g),\(b))")
