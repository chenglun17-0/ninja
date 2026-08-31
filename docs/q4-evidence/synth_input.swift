// synth_input.swift —— q3 E2E 真实输入驱动（q1 同款 + 鼠标修饰键）
//
// 用途：向指定 PID 投递真实 CGEvent（键盘 CGEventPostToPid；鼠标走全局
// HID tap——CGEventPostToPid 的鼠标事件不带窗口上下文，目标 app 不会
// 命中；HID 注入经窗服按坐标路由——坐标在虚拟屏上，只影响那里的窗口），
// 以及查询目标 app 的窗口表（CGWindowID/标题/bounds，CG 全局坐标：
// 原点=主屏左上、y 向下）。
//
// 子命令：
//   trust                              打印 AXIsProcessTrusted（合成事件需要）
//   key <pid> <keycode> [mods]         单键按下+抬起；mods 逗号组合：
//                                      cmd,shift,alt,ctrl（kVK 键码，十进制）
//   type <pid> <text>                  逐字符键入（a-z0-9 空格 . - / _ = 回车\n）
//   click <pid> <x> <y> [mods]         左键点击（CG 全局坐标；mods 同上，
//                                      ⌘+click 的 hit 门禁用）
//   wins <pid>                         目标 app 窗口表 JSON（id/title/bounds/layer）
//
// 常用键码（kVK）：w=13 t=17 n=45 q=12 d=2 enter=36 esc=53 '['=33 ']'=30
//
// 编译（产物不入库）：
//   swiftc -O docs/q3-evidence/synth_input.swift -o /tmp/nq3-synth

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
    "_": (0x1B, true), "=": (0x18, false), ":": (0x27, true),
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

// 鼠标走全局 HID tap（坐标即目标）；flags 携带修饰（⌘+click 的 hit
// 门禁：ghostty 的链接 hover/点击判定读 mouse mods）。
func postMouse(_ type: CGEventType, x: Double, y: Double, flags: CGEventFlags) {
    let src = CGEventSource(stateID: .hidSystemState)
    guard let ev = CGEvent(mouseEventSource: src, mouseType: type,
                           mouseCursorPosition: CGPoint(x: x, y: y), mouseButton: .left)
    else { FileHandle.standardError.write("mouse event create failed\n".data(using: .utf8)!); exit(1) }
    ev.flags = flags.union(CGEventFlags(rawValue: 0x100))
    ev.post(tap: .cghidEventTap)
}

func postClick(pid: pid_t, x: Double, y: Double, flags: CGEventFlags) {
    _ = pid
    postMouse(.mouseMoved, x: x, y: y, flags: flags)
    usleep(150_000)
    postMouse(.leftMouseDown, x: x, y: y, flags: flags)
    usleep(80_000)
    postMouse(.leftMouseUp, x: x, y: y, flags: flags)
    usleep(120_000)
}

let args = CommandLine.arguments
guard args.count >= 2 else {
    print("usage: synth_input trust|key|type|click|wins …"); exit(2)
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
    for ch in args[3] {
        if ch == "\n" {
            postKey(pid: pid, keycode: 36, flags: [])
            continue
        }
        guard let (code, needsShift) = keycodes[ch] else { continue }
        postKey(pid: pid, keycode: code, flags: needsShift ? shiftFlag : [])
    }
case "click":
    guard args.count >= 5, let pid = pid_t(args[2]), let x = Double(args[3]), let y = Double(args[4]) else { exit(2) }
    var flags: CGEventFlags = []
    if args.count >= 6 {
        for m in args[5].split(separator: ",") {
            switch m {
            case "cmd": flags = flags.union(cmdFlag)
            case "shift": flags = flags.union(shiftFlag)
            case "alt": flags = flags.union(altFlag)
            case "ctrl": flags = flags.union(ctrlFlag)
            default: break
            }
        }
    }
    postClick(pid: pid, x: x, y: y, flags: flags)
case "wins":
    guard args.count >= 3, let pid = pid_t(args[2]) else { exit(2) }
    let opts = CGWindowListOption([.optionOnScreenOnly, .excludeDesktopElements])
    guard let list = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] else {
        print("[]"); exit(0)
    }
    var out: [[String: Any]] = []
    for w in list where (w[kCGWindowOwnerPID as String] as? Int) == Int(pid) {
        let bounds = w[kCGWindowBounds as String] as? [String: Int] ?? [:]
        out.append([
            "id": w[kCGWindowNumber as String] as? Int ?? 0,
            "layer": w[kCGWindowLayer as String] as? Int ?? 0,
            "title": w[kCGWindowName as String] as? String ?? "",
            "bounds": [
                bounds["X"] ?? 0, bounds["Y"] ?? 0,
                bounds["Width"] ?? 0, bounds["Height"] ?? 0,
            ],
        ])
    }
    let data = try! JSONSerialization.data(withJSONObject: out)
    print(String(data: data, encoding: .utf8)!)
default:
    print("unknown subcommand \(args[1])"); exit(2)
}
