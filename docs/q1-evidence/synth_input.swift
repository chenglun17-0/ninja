// synth_input.swift —— q1 E2E 真实输入驱动（证据脚本）
//
// 用途：向指定 PID 投递真实 CGEvent（CGEventPostToPid，不抢开发者全局
// 焦点；键盘/鼠标事件直接进目标 app 的事件队列），以及查询目标 app 的
// 窗口表（CGWindowID/标题/bounds，CG 全局坐标：原点=主屏左上、y 向下）。
//
// 子命令：
//   trust                              打印 AXIsProcessTrusted（合成键盘事件需要）
//   key <pid> <keycode> [mods]         单键按下+抬起；mods 逗号组合：
//                                      cmd,shift,alt,ctrl（kVK 键码，十进制）
//   type <pid> <text>                  逐字符键入（a-z0-9 空格 . - / _ = 回车\n）
//   click <pid> <x> <y>                左键点击（CG 全局坐标）
//   drag <pid> <x0> <y0> <x1> <y1>     左键按下→插值移动→抬起（拖分隔条）
//   wins <pid>                         目标 app 窗口表 JSON（id/title/bounds/layer）
//
// 常用键码（kVK）：w=13 t=17 n=45 q=12 d=2 enter=36 '['=33 ']'=30 '='=39
//   left=123 right=124 down=125 up=126 ','=43 ('='=24：0x27 是引号键！)
//
// 编译（产物不入库；随证据脚本分发以便复跑）：
//   clang -fobjc-arc -framework Foundation -framework CoreGraphics \
//         -Wl,-undefined,dynamic_lookup docs/q1-evidence/synth_input.swift \
//         -o /tmp/nq1-synth 2>/dev/null || swiftc -O docs/q1-evidence/synth_input.swift -o /tmp/nq1-synth

import Foundation
import CoreGraphics
import ApplicationServices

let cmdFlag = CGEventFlags.maskCommand
let shiftFlag = CGEventFlags.maskShift
let altFlag = CGEventFlags.maskAlternate
let ctrlFlag = CGEventFlags.maskControl

let keycodes: [Character: (Int, Bool)] = [ // (kVK, needsShift)
    "a": (0x00, false), "b": (0x0B, false), "c": (0x08, false), "d": (0x02, false),
    "e": (0x0E, false), "f": (0x03, false), "g": (0x05, false), "h": (0x04, false),
    "i": (0x22, false), "j": (0x26, false), "k": (0x28, false), "l": (0x25, false),
    "m": (0x2E, false), "n": (0x2D, false), "o": (0x1F, false), "p": (0x23, false),
    "q": (0x0C, false), "r": (0x0F, false), "s": (0x01, false), "t": (0x11, false),
    "u": (0x20, false), "v": (0x09, false), "w": (0x0D, false), "x": (0x07, false),
    "y": (0x10, false), "z": (0x06, false),
    "1": (0x12, false), "2": (0x13, false), "3": (0x14, false), "4": (0x15, false),
    "5": (0x17, false), "6": (0x16, false), "7": (0x1A, false), "8": (0x1C, false),
    "9": (0x19, false), "0": (0x1D, false),
    " ": (0x31, false), ".": (0x2F, false), "-": (0x1B, false), "/": (0x2C, false),
    "_": (0x1B, true), "=": (0x18, false),
]

func postKey(pid: pid_t, keycode: Int, flags: CGEventFlags) {
    guard let down = CGEvent(keyboardEventSource: nil, virtualKey: CGKeyCode(keycode), keyDown: true),
          let up = CGEvent(keyboardEventSource: nil, virtualKey: CGKeyCode(keycode), keyDown: false)
    else { FileHandle.standardError.write("event create failed\n".data(using: .utf8)!); exit(1) }
    // flags 直接替换目标修饰 + 恒定 0x100（kCGEventFlagMaskNonCoalesced，
    // 真实键盘事件永远带此位；实测 flags=0x0 的退化事件 keyDown 能到但
    // IME insertText 路径不产生 → Enter 不执行。也不能 union 残留位——
    // shift 位泄漏会把 Enter 变 shift+Enter）。
    down.flags = flags.union(CGEventFlags(rawValue: 0x100))
    up.flags = flags.union(CGEventFlags(rawValue: 0x100))
    down.postToPid(pid)
    usleep(30_000)
    up.postToPid(pid)
    usleep(60_000)
}

// 鼠标走全局 HID tap（CGEventPostToPid 的鼠标事件不带窗口上下文，目标
// app 不会命中；HID 注入经窗服按坐标路由——坐标在虚拟屏上，只影响那
// 里的窗口）。先 mouseMoved 挪指针再点击/拖拽。
func postMouse(_ type: CGEventType, x: Double, y: Double, button: CGMouseButton = .left) {
    let src = CGEventSource(stateID: .hidSystemState)
    guard let ev = CGEvent(mouseEventSource: src, mouseType: type,
                           mouseCursorPosition: CGPoint(x: x, y: y), mouseButton: button)
    else { FileHandle.standardError.write("mouse event create failed\n".data(using: .utf8)!); exit(1) }
    ev.flags = []   // 同上：清残留修饰位
    ev.post(tap: .cghidEventTap)
}

func postClick(pid: pid_t, x: Double, y: Double) {
    _ = pid // 键盘仍走 PostToPid；鼠标全局注入（坐标即目标）
    postMouse(.mouseMoved, x: x, y: y)
    usleep(80_000)
    postMouse(.leftMouseDown, x: x, y: y)
    usleep(60_000)
    postMouse(.leftMouseUp, x: x, y: y)
    usleep(80_000)
}

let args = CommandLine.arguments
guard args.count >= 2 else {
    print("usage: synth_input trust|key|type|click|drag|wins …"); exit(2)
}

switch args[1] {
case "trust":
    print(AXIsProcessTrusted() ? "trusted" : "not-trusted")
case "key":
    guard args.count >= 4, let pid = pid_t(args[2]), let code = Int(args[3]) else { exit(2) }
    var flags: CGEventFlags = []
    if args.count >= 5 {
        for m in args[4].split(separator: ",") {
            switch m {
            case "cmd": flags = flags.union(cmdFlag)
            case "shift": flags = flags.union(shiftFlag)
            case "alt": flags = flags.union(altFlag)
            case "ctrl": flags = flags.union(ctrlFlag)
            default: break
            }
        }
    }
    postKey(pid: pid, keycode: code, flags: flags)
case "type":
    guard args.count >= 4, let pid = pid_t(args[2]) else { exit(2) }
    // 字面 "\n"（shell 双引号）也按回车处理；结尾 "\b" = 补一发退格
    // （就绪探针清理 shell 输入行，不污染后续取证）。
    var text = args[3].replacingOccurrences(of: "\\n", with: "\n")
    var probe = false
    if text.hasSuffix("\\b") {
        text = String(text.dropLast(2))
        probe = true
    }
    for ch in text {
        if ch == "\n" {
            postKey(pid: pid, keycode: 0x24, flags: [])
            continue
        }
        let lower = Character(ch.lowercased())
        let needShift = ch.isUppercase || (String(ch) != String(lower) && !ch.isNumber)
        guard let (code, baseShift) = keycodes[lower] else {
            FileHandle.standardError.write("no keycode for \(ch)\n".data(using: .utf8)!); exit(1)
        }
        let shift = needShift || baseShift
        postKey(pid: pid, keycode: code, flags: shift ? shiftFlag : [])
    }
    if probe {
        postKey(pid: pid, keycode: 0x33, flags: []) // backspace 清行
    }
case "click":
    guard args.count >= 5, let pid = pid_t(args[2]), let x = Double(args[3]), let y = Double(args[4]) else { exit(2) }
    postClick(pid: pid, x: x, y: y)
case "drag":
    guard args.count >= 7, let pid = pid_t(args[2]),
          let x0 = Double(args[3]), let y0 = Double(args[4]),
          let x1 = Double(args[5]), let y1 = Double(args[6]) else { exit(2) }
    _ = pid // 鼠标全局注入（见 postMouse 注）
    postMouse(.mouseMoved, x: x0, y: y0)
    usleep(80_000)
    postMouse(.leftMouseDown, x: x0, y: y0)
    usleep(80_000)
    for i in 1...12 {
        let t = Double(i) / 12.0
        postMouse(.leftMouseDragged, x: x0 + (x1 - x0) * t, y: y0 + (y1 - y0) * t)
        usleep(40_000)
    }
    usleep(80_000)
    postMouse(.leftMouseUp, x: x1, y: y1)
    usleep(120_000)
case "wins":
    guard args.count >= 3, let pid = pid_t(args[2]) else { exit(2) }
    guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID)
          as? [[String: Any]] else { print("[]"); exit(0) }
    let out = list.filter { ($0[kCGWindowOwnerPID as String] as? Int32) == pid }
        .map { w -> [String: Any] in
            var o: [String: Any] = [
                "id": w[kCGWindowNumber as String] ?? 0,
                "layer": w[kCGWindowLayer as String] ?? -1,
                "title": w[kCGWindowName as String] ?? "",
            ]
            if let b = w[kCGWindowBounds as String] as? [String: Any],
               let x = (b["X"] as? NSNumber)?.doubleValue,
               let y = (b["Y"] as? NSNumber)?.doubleValue,
               let w2 = (b["Width"] as? NSNumber)?.doubleValue,
               let h = (b["Height"] as? NSNumber)?.doubleValue {
                o["bounds"] = [x, y, w2, h]
            }
            return o
        }
    if let data = try? JSONSerialization.data(withJSONObject: out),
       let s = String(data: data, encoding: .utf8) { print(s) }
default:
    print("unknown subcommand \(args[1])"); exit(2)
}
