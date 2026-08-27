//! NSEvent → libghostty-vt 键事件的桥。
//!
//! 两条路：
//! 1. 文本键走 `interpretKeyEvents`（IME/组合输入的正确路径），
//!    `insertText` / `doCommandBySelector` 回调里落字节。
//! 2. 功能键（F1-F12、无字符键）和宿主直通键在 keyDown 里直接编码。
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
}
