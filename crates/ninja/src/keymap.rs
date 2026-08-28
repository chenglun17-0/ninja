//! NSEvent → libghostty-vt 键事件的桥。
//!
//! 两条路：
//! 1. 文本键走 `interpretKeyEvents`（IME/组合输入的正确路径），
//!    `insertText` / `doCommandBySelector` 回调里落字节。
//! 2. 功能键（F1-F12、无字符键）和宿主直通键在 keyDown 里直接编码。
//!
//! 例外（D-B）：Ctrl 组合（非 Cmd）必须整类走第 2 条路——AppKit 键绑定
//! 把 Ctrl+字母翻译成编辑命令（^a→moveToBeginningOfParagraph:），
//! 未绑定的 ^c 在 IME 输入源下被 interpretKeyEvents 整体吞掉（零回调），
//! 而控制字符又过不了 sanitize 文本路径（C0 被剥）——只有按 vt 键 +
//! CTRL 修饰编码才能得到终端语义的 C0 字节（^C→0x03）。
//!
//! macOS 虚拟键码是固定值（HIToolbox Events.h），与键盘布局无关。

use libghostty_vt::key::{Key, Mods};

/// 虚拟键码 → 逻辑键。未列出的键码返回 None（走文本路径）。
pub fn key_from_code(code: u16) -> Option<Key> {
    Some(match code {
        0x00 => Key::A,
        0x01 => Key::S,
        0x02 => Key::D,
        0x03 => Key::F,
        0x04 => Key::H,
        0x05 => Key::G,
        0x06 => Key::Z,
        0x07 => Key::X,
        0x08 => Key::C,
        0x09 => Key::V,
        0x0B => Key::B,
        0x0C => Key::Q,
        0x0D => Key::W,
        0x0E => Key::E,
        0x0F => Key::R,
        0x10 => Key::Y,
        0x11 => Key::T,
        0x12 => Key::Digit1,
        0x13 => Key::Digit2,
        0x14 => Key::Digit3,
        0x15 => Key::Digit4,
        0x16 => Key::Digit6,
        0x17 => Key::Digit5,
        0x18 => Key::Equal,
        0x19 => Key::Digit9,
        0x1A => Key::Digit7,
        0x1B => Key::Minus,
        0x1C => Key::Digit8,
        0x1D => Key::Digit0,
        0x1E => Key::BracketRight,
        0x1F => Key::O,
        0x20 => Key::U,
        0x21 => Key::BracketLeft,
        0x22 => Key::I,
        0x23 => Key::P,
        0x25 => Key::L,
        0x26 => Key::J,
        0x27 => Key::Quote,
        0x28 => Key::K,
        0x29 => Key::Semicolon,
        0x2A => Key::Backslash,
        0x2B => Key::Comma,
        0x2C => Key::Slash,
        0x2D => Key::N,
        0x2E => Key::M,
        0x2F => Key::Period,
        0x32 => Key::IntlBackslash,
        0x41 => Key::NumpadDecimal,
        0x43 => Key::NumpadMultiply,
        0x45 => Key::NumpadAdd,
        0x47 => Key::NumLock,
        0x4B => Key::NumpadDivide,
        0x4C => Key::NumpadEnter,
        0x4E => Key::NumpadSubtract,
        0x51 => Key::NumpadEqual,
        0x52 => Key::Numpad0,
        0x53 => Key::Numpad1,
        0x54 => Key::Numpad2,
        0x55 => Key::Numpad3,
        0x56 => Key::Numpad4,
        0x57 => Key::Numpad5,
        0x58 => Key::Numpad6,
        0x59 => Key::Numpad7,
        0x5B => Key::Numpad8,
        0x5C => Key::Numpad9,
        0x24 => Key::Enter,
        0x30 => Key::Tab,
        0x31 => Key::Space,
        0x33 => Key::Backspace,
        0x35 => Key::Escape,
        0x37 => Key::MetaLeft, // Cmd
        0x38 => Key::ShiftLeft,
        0x39 => Key::CapsLock,
        0x3A => Key::AltLeft, // Option
        0x3B => Key::ControlLeft,
        0x3C => Key::ShiftRight,
        0x3D => Key::AltRight,
        0x3E => Key::ControlRight,
        0x3F => Key::MetaRight,
        0x40 => Key::Fn,
        0x48 => Key::AudioVolumeUp,
        0x49 => Key::AudioVolumeDown,
        0x4A => Key::AudioVolumeMute,
        0x50 => Key::F5,
        0x5F => Key::F6,
        0x60 => Key::F7,
        0x61 => Key::F3,
        0x62 => Key::F8,
        0x63 => Key::F9,
        0x64 => Key::F10,
        0x65 => Key::F11,
        0x6D => Key::F12,
        0x67 => Key::F13,
        0x69 => Key::F14,
        0x6B => Key::F15,
        0x71 => Key::F16,
        0x6A => Key::F17,
        0x72 => Key::Help,
        0x73 => Key::Home,
        0x74 => Key::PageUp,
        0x75 => Key::Delete,
        0x76 => Key::F4,
        0x77 => Key::End,
        0x78 => Key::F2,
        0x79 => Key::PageDown,
        0x7A => Key::F1,
        0x7B => Key::ArrowLeft,
        0x7C => Key::ArrowRight,
        0x7D => Key::ArrowDown,
        0x7E => Key::ArrowUp,
        _ => return None,
    })
}

/// NSEvent 修饰位（objc2-app-kit NSEventModifierFlags，HIToolbit 独立位）→ vt Mods。
/// NSEventModifierFlagControl = 1<<18 = 0x4_0000（老 Carbon 时代的 0x10001
/// 已废弃，现代 NSEvent 永不携带）。
pub fn mods_from_flags(flags: u64) -> Mods {
    let mut mods = Mods::empty();
    if flags & 0x0002_0000 != 0 {
        mods |= Mods::SHIFT;
    }
    if flags & 0x0004_0000 != 0 {
        mods |= Mods::CTRL;
    }
    if flags & 0x0008_0000 != 0 {
        mods |= Mods::ALT;
    }
    if flags & 0x0010_0000 != 0 {
        mods |= Mods::SUPER;
    }
    mods
}

/// `doCommandBySelector:` 的选择子 → 逻辑键（终端常见的方向/编辑键映射）。
pub fn key_from_command_selector(sel: &str) -> Option<Key> {
    Some(match sel {
        "insertNewline:" | "insertLineBreak:" => Key::Enter,
        "insertTab:" => Key::Tab,
        "insertBacktab:" => Key::Tab,
        "deleteBackward:" => Key::Backspace,
        "deleteForward:" => Key::Delete,
        "deleteWordBackward:" => Key::Backspace,
        "deleteWordForward:" => Key::Delete,
        "deleteToBeginningOfLine:" => Key::Backspace,
        "deleteToEndOfLine:" => Key::Delete,
        "moveUp:" => Key::ArrowUp,
        "moveDown:" => Key::ArrowDown,
        "moveLeft:" => Key::ArrowLeft,
        "moveRight:" => Key::ArrowRight,
        "moveWordLeft:" => Key::ArrowLeft,
        "moveWordRight:" => Key::ArrowRight,
        "moveBackward:" => Key::ArrowLeft,
        "moveForward:" => Key::ArrowRight,
        "moveToBeginningOfLine:" | "moveToLeftEndOfLine:" => Key::Home,
        "moveToEndOfLine:" | "moveToRightEndOfLine:" => Key::End,
        "moveToBeginningOfParagraph:" => Key::ArrowUp,
        "moveToEndOfParagraph:" => Key::ArrowDown,
        "moveToBeginningOfDocument:" => Key::Home,
        "moveToEndOfDocument:" => Key::End,
        "moveParagraphForward:" => Key::ArrowDown,
        "moveParagraphBackward:" => Key::ArrowUp,
        "pageUp:" | "scrollPageUp:" => Key::PageUp,
        "pageDown:" | "scrollPageDown:" => Key::PageDown,
        "pageForward:" => Key::PageDown,
        "pageBackward:" => Key::PageUp,
        "cancelOperation:" => Key::Escape,
        "transpose:" => Key::T,
        "insertContainerBreak:" => Key::Enter,
        // 剩余 insertXxx: 文本类；noop:/scrollToBeginningOfDocument: 等
        // Home/End 的滚动变体已在上面接住。
        _ => return None,
    })
}

/// vt 逻辑键 → ADE 协议键名（命名集：left/right/up/down/home/end/
/// pageup/pagedown/delete/backspace/tab/enter/esc/f1..f12 + 单字符）。
/// 协议键名集冻结（新键名升 v）；不在集内 → None（调用方退回单字符
/// 文本或丢弃）。
pub fn protocol_key_name(key: Key) -> Option<String> {
    let name = match key {
        Key::ArrowLeft => "left",
        Key::ArrowRight => "right",
        Key::ArrowUp => "up",
        Key::ArrowDown => "down",
        Key::Home => "home",
        Key::End => "end",
        Key::PageUp => "pageup",
        Key::PageDown => "pagedown",
        Key::Delete => "delete",
        Key::Backspace => "backspace",
        Key::Tab => "tab",
        Key::Enter | Key::NumpadEnter => "enter",
        Key::Escape => "esc",
        Key::F1 => "f1",
        Key::F2 => "f2",
        Key::F3 => "f3",
        Key::F4 => "f4",
        Key::F5 => "f5",
        Key::F6 => "f6",
        Key::F7 => "f7",
        Key::F8 => "f8",
        Key::F9 => "f9",
        Key::F10 => "f10",
        Key::F11 => "f11",
        Key::F12 => "f12",
        Key::Space => " ",
        _ => return None,
    };
    Some(name.to_string())
}

/// 剪掉 vt 编码器不接受的文本：C0 控制字符与 macOS PUA 功能键码
///（U+F700–U+F8FF），见 libghostty-vt key::Event::set_utf8 文档。
pub fn sanitize_utf8(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        let c = ch as u32;
        if c < 0x20 || c == 0x7f {
            continue;
        }
        if (0xf700..=0xf8ff).contains(&c) {
            continue;
        }
        out.push(ch);
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// D-B：Ctrl 组合是否必须绕过 interpretKeyEvents、在 keyDown 里按
/// vt 键直接编码。条件：CTRL 修饰 + 键码有逻辑键映射（SUPER 组合在
/// 调用方上游已被单独截走）。无映射键码的 Ctrl 组合（罕见）回落
/// interpret 兜底，行为不劣于修复前。
pub fn ctrl_bypasses_interpret(code: u16, mods: Mods) -> bool {
    mods.contains(Mods::CTRL) && key_from_code(code).is_some()
}

/// D-B：Ctrl 直通路径喂给 vt 编码器的 utf8。取
/// charactersIgnoringModifiers 小写化后过 sanitize——编码器要求
/// 「未修饰、无控制字符」的文本（key::Event::set_utf8 文档），据此
/// 派生 C0 字节；小写化让 ⇧^C 也归到 ^C=0x03（终端惯例：shift 不
/// 参与 C0 派生，编码器对大写 "C" 会改产 CSI u 序列）。
/// PUA 功能键码（Ctrl+方向键的 U+F702 等）剥成 None，编码器按
/// 逻辑键 + CTRL 产出 CSI 修饰序列。
pub fn ctrl_key_utf8(chars_ignoring_mods: &str) -> Option<String> {
    sanitize_utf8(&chars_ignoring_mods.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_flags_map() {
        // 与 NSEventModifierFlag* 位值一一对应（NSEvent.rs bitflags）。
        let shift = 1 << 17;
        let ctrl = 1 << 18;
        let opt = 1 << 19;
        let cmd = 1 << 20;
        assert_eq!(mods_from_flags(shift), Mods::SHIFT);
        assert_eq!(mods_from_flags(cmd), Mods::SUPER);
        assert_eq!(mods_from_flags(opt), Mods::ALT);
        assert_eq!(mods_from_flags(ctrl), Mods::CTRL);
        // 组合 + 无修饰。
        let all = shift | ctrl | opt | cmd;
        assert_eq!(
            mods_from_flags(all),
            Mods::SHIFT | Mods::CTRL | Mods::ALT | Mods::SUPER
        );
        assert_eq!(mods_from_flags(0), Mods::empty());
        // 老的 Carbon 掩码 0x10001 不得误判为 CTRL。
        assert_eq!(mods_from_flags(0x1_0001), Mods::empty());
    }

    #[test]
    fn sanitize_strips_control_and_pua() {
        assert_eq!(sanitize_utf8("a").as_deref(), Some("a"));
        assert_eq!(sanitize_utf8("\u{f700}b").as_deref(), Some("b"));
        assert_eq!(sanitize_utf8("\u{1}").as_deref(), None);
        assert_eq!(sanitize_utf8("").as_deref(), None);
        // 中文正常保留（IME 提交路径不走这里，但别误伤）。
        assert_eq!(sanitize_utf8("你").as_deref(), Some("你"));
    }

    #[test]
    fn ctrl_bypass_rules() {
        // D-B：Ctrl + 有映射键码 → 绕过 interpretKeyEvents 直通编码。
        assert!(ctrl_bypasses_interpret(0x08, Mods::CTRL)); // C
        assert!(ctrl_bypasses_interpret(0x08, Mods::CTRL | Mods::SHIFT)); // ⇧^C
        assert!(ctrl_bypasses_interpret(0x7B, Mods::CTRL)); // ←
        assert!(ctrl_bypasses_interpret(0x31, Mods::CTRL)); // Space
        // 无 Ctrl、或键码无映射 → 仍走 interpret 文本路径。
        assert!(!ctrl_bypasses_interpret(0x08, Mods::empty()));
        assert!(!ctrl_bypasses_interpret(0x0A, Mods::CTRL)); // 表外键码
    }

    #[test]
    fn ctrl_key_utf8_lowers_and_sanitizes() {
        // 编码器要「未修饰」文本：^C 与 ⇧^C 的 charactersIgnoringModifiers
        // （"c"/"C"）统一小写化 → 0x03，而非大写路径的 CSI u 序列。
        assert_eq!(ctrl_key_utf8("c").as_deref(), Some("c"));
        assert_eq!(ctrl_key_utf8("C").as_deref(), Some("c"));
        // PUA 功能键码与 C0 控制字符剥除（同 sanitize 语义）。
        assert_eq!(ctrl_key_utf8("\u{f702}"), None);
        assert_eq!(ctrl_key_utf8("\u{3}"), None);
        assert_eq!(ctrl_key_utf8(""), None);
        // 空格保留（^Space → 0x00 由编码器派生）。
        assert_eq!(ctrl_key_utf8(" ").as_deref(), Some(" "));
    }

    #[test]
    fn known_keycodes() {
        assert_eq!(key_from_code(0x7E), Some(Key::ArrowUp));
        assert_eq!(key_from_code(0x33), Some(Key::Backspace));
        assert_eq!(key_from_code(0x24), Some(Key::Enter));
        assert_eq!(key_from_code(0x31), Some(Key::Space));
        assert_eq!(key_from_code(0x7A), Some(Key::F1));
        assert_eq!(key_from_code(0x7B), Some(Key::ArrowLeft));
    }

    #[test]
    fn selectors_map() {
        assert_eq!(key_from_command_selector("moveLeft:"), Some(Key::ArrowLeft));
        assert_eq!(
            key_from_command_selector("deleteBackward:"),
            Some(Key::Backspace)
        );
        assert_eq!(
            key_from_command_selector("cancelOperation:"),
            Some(Key::Escape)
        );
        assert_eq!(key_from_command_selector("insertNewline:"), Some(Key::Enter));
        assert_eq!(key_from_command_selector("insertTab:"), Some(Key::Tab));
        assert_eq!(key_from_command_selector("noop:"), None);
    }

    #[test]
    fn protocol_key_names() {
        // 命名集与协议文档一致；字母/数字不在命名集（退回单字符文本）。
        assert_eq!(protocol_key_name(Key::Escape).as_deref(), Some("esc"));
        assert_eq!(protocol_key_name(Key::ArrowLeft).as_deref(), Some("left"));
        assert_eq!(protocol_key_name(Key::PageDown).as_deref(), Some("pagedown"));
        assert_eq!(protocol_key_name(Key::NumpadEnter).as_deref(), Some("enter"));
        assert_eq!(protocol_key_name(Key::F12).as_deref(), Some("f12"));
        assert_eq!(protocol_key_name(Key::Space).as_deref(), Some(" "));
        assert_eq!(protocol_key_name(Key::A), None);
    }
}
