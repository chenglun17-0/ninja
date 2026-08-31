// probe_window.swift —— q3 像素探针（q2 同款 + 方差：层文本可判）。
//
// 用途：对窗口截图（screencapture -l 的 PNG）取相对区域的平均 RGB 与
// 通道标准差（文本行有前景色像素 → 方差 > 0；纯色背景方差 ≈ 0）。
//
// 用法:
//   probe_window avg   <png> <x0> <y0> <w> <h>   # 区域平均色 JSON（相对比例 0-1）
//   probe_window var   <png> <x0> <y0> <w> <h>   # 区域平均色 + 通道标准差
//
// 编译: swiftc -O docs/q3-evidence/probe_window.swift -o /tmp/nq3-probe

import Foundation
import CoreGraphics
import AppKit

func loadPixels(_ path: String) -> (CGImage, [UInt8])? {
    guard let img = NSImage(contentsOfFile: path),
          let cg = img.cgImage(forProposedRect: nil, context: nil, hints: nil) else { return nil }
    let w = cg.width, h = cg.height
    guard w > 0 && h > 0 else { return nil }
    var data = [UInt8](repeating: 0, count: w * h * 4)
    let ctx = CGContext(data: &data, width: w, height: h, bitsPerComponent: 8,
                        bytesPerRow: w * 4, space: CGColorSpaceCreateDeviceRGB(),
                        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.draw(cg, in: CGRect(x: 0, y: 0, width: w, height: h))
    return (cg, data)
}

let args = CommandLine.arguments
guard args.count >= 7, let (img, data) = loadPixels(args[2]),
      let x0 = Double(args[3]), let y0 = Double(args[4]),
      let rw = Double(args[5]), let rh = Double(args[6]) else {
    print("{\"error\":\"usage: probe avg|var <png> x0 y0 w h (相对比例)\"}")
    exit(2)
}
let mode = args[1]
let w = img.width, h = img.height
let px0 = max(0, min(w - 1, Int(x0 * Double(w))))
let py0 = max(0, min(h - 1, Int(y0 * Double(h))))
let px1 = max(px0 + 1, min(w, px0 + Int(rw * Double(w))))
let py1 = max(py0 + 1, min(h, py0 + Int(rh * Double(h))))

var sums = [0.0, 0.0, 0.0]
var squares = [0.0, 0.0, 0.0]
var n = 0
for y in py0..<py1 {
    for x in px0..<px1 {
        let o = (y * w + x) * 4
        for c in 0..<3 {
            let v = Double(data[o + c])
            sums[c] += v
            squares[c] += v * v
        }
        n += 1
    }
}
guard n > 0 else { print("{\"error\":\"empty region\"}"); exit(1) }
let avg = sums.map { Int($0 / Double(n)) }
if mode == "var" {
    let vars: [Double] = (0..<3).map { c in
        let mean = sums[c] / Double(n)
        return (squares[c] / Double(n) - mean * mean).squareRoot()
    }
    let obj: [String: Any] = ["avg": avg, "std": vars.map { Int($0) }, "px": [px1 - px0, py1 - py0]]
    let d = try! JSONSerialization.data(withJSONObject: obj)
    print(String(data: d, encoding: .utf8)!)
} else {
    let obj: [String: Any] = ["avg": avg, "px": [px1 - px0, py1 - py0]]
    let d = try! JSONSerialization.data(withJSONObject: obj)
    print(String(data: d, encoding: .utf8)!)
}
