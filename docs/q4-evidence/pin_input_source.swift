// 把当前输入源钉到 US 键盘布局（E2E 防输入法吞合成键）。
// 用法：pin_input_source [restore-source-id]
//   无参：打印并切到 US 前的当前输入源 ID（供恢复）。
//   带参：切回指定输入源。
import Carbon
import Foundation

func source(id: String) -> TISInputSource? {
    let filter: [CFString: Any] = [kTISPropertyInputSourceID: id as CFString]
    guard let list = TISCreateInputSourceList(filter as CFDictionary, false)?
        .takeRetainedValue() as? [TISInputSource], let first = list.first
    else { return nil }
    return first
}

let us = source(id: "com.apple.keylayout.US")!
if let restoreID = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : nil,
    let target = source(id: restoreID) {
    TISSelectInputSource(target)
    print(restoreID)
    exit(0)
}
// 读当前（非 US 时打印其 ID，恢复用）
if let cur = TISCopyCurrentKeyboardInputSource()?.takeRetainedValue(),
    let rawID = TISGetInputSourceProperty(cur, kTISPropertyInputSourceID) {
    let curID = Unmanaged<CFString>.fromOpaque(rawID).takeUnretainedValue() as String
    TISSelectInputSource(us)
    print(curID)
} else {
    TISSelectInputSource(us)
    print("com.apple.keylayout.US")
}
