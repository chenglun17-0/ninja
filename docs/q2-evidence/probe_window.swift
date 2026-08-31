// probe_window.swift —— q2 E2E 窗口像素探针（证据脚本）
//
// 用途：对 `screencapture -x -l <windowID>` 抓出的窗口 PNG 采样相对区域
//（0..1 分数坐标）的平均 RGB，输出 JSON（E2E 用 json.load 断言）。
// 绝对色断言容差 ±10（显示色空间相对 sRGB 有 ~10% 系统性压暗，同一显示
// 恒定）；颜色传播断言用强对比色（#ff00ff）。q0 pixel-sample.swift 同款
// 位图采样路径（本机 TCC 已授屏幕录制，screencapture 可用）。
//
// 子命令：
//   avg <png> <fx> <fy> <fw> <fh>   相对区域平均 → {"avg":[r,g,b],"px":[w,h]}
//
// 编译（产物不入库；随证据脚本分发以便复跑）：
//   swiftc -O docs/q2-evidence/probe_window.swift -o /tmp/nq2-probe

import Foundation
import CoreGraphics
import ImageIO

func avgJSON(_ path: String, _ fx: Double, _ fy: Double, _ fw: Double, _ fh: Double) -> String {
    guard let src = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
          let img = CGImageSourceCreateImageAtIndex(src, 0, nil) else {
        return "{\"error\":\"cannot open \(path)\"}"
    }
    let w = img.width, h = img.height
    var rect = CGRect(
        x: Double(w) * fx, y: Double(h) * fy,
        width: Double(w) * fw, height: Double(h) * fh
    )
    rect = rect.intersection(CGRect(x: 0, y: 0, width: w, height: h))
    guard !rect.isEmpty, let crop = img.cropping(to: rect) else {
        return "{\"error\":\"empty region\"}"
    }
    let space = CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpace(name: CGColorSpace.genericRGBLinear)!
    guard let ctx = CGContext(
        data: nil, width: crop.width, height: crop.height,
        bitsPerComponent: 8, bytesPerRow: crop.width * 4, space: space,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else { return "{\"error\":\"ctx\"}" }
    ctx.draw(crop, in: CGRect(x: 0, y: 0, width: crop.width, height: crop.height))
    guard let data = ctx.data else { return "{\"error\":\"no data\"}" }
    let px = data.assumingMemoryBound(to: UInt8.self)
    var sr = 0.0, sg = 0.0, sb = 0.0
    let n = Double(crop.width * crop.height)
    for i in 0..<Int(n) {
        sr += Double(px[i * 4]); sg += Double(px[i * 4 + 1]); sb += Double(px[i * 4 + 2])
    }
    let avg = [Int((sr / n).rounded()), Int((sg / n).rounded()), Int((sb / n).rounded())]
    return "{\"avg\":[\(avg[0]),\(avg[1]),\(avg[2])],\"px\":[\(crop.width),\(crop.height)]}"
}

let args = CommandLine.arguments
guard args.count == 7,
      let fx = Double(args[3]), let fy = Double(args[4]),
      let fw = Double(args[5]), let fh = Double(args[6]) else {
    FileHandle.standardError.write(
        "usage: probe_window avg <png> <fx> <fy> <fw> <fh>  (relative 0..1)\n".data(using: .utf8)!
    )
    exit(2)
}
print(avgJSON(args[2], fx, fy, fw, fh))
