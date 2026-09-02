//! NSEvent → ghostty 嵌入 API 键事件的桥（q1）。
//!
//! 嵌入 API 的 `ghostty_input_key_s.keycode` 传 **macOS 原生虚拟键码**
//! （embedded.zig `KeyEvent.core` 按 `input.keycodes.entries` 的 mac 列
//! （native index 4）反查物理键），所以宿主不做键码映射，直传
//! `event.keyCode()`。文本、修饰、unshifted codepoint 按 macOS Ghostty
//! 本尊（SurfaceView_AppKit.swift + NSEvent+Extension.swift）同款启发式：
//! - mods：NSEventModifierFlags → GHOSTTY_MODS_*（含 caps 与右侧设备位）；
//! - consumed_mods：ctrl/⌘ 不参与文本翻译，其余（shift/option/caps）记为
//!   已消耗；
//! - text：`characters`，单个 C0 控制字符与 PUA 功能键（F700-F8FF）不下发
//!   （ghostty 自己编码控制键）；
//! - unshifted_codepoint：`charactersByApplyingModifiers([])` 的首码点。

#![allow(non_snake_case)]

use objc2_app_kit::{NSEvent, NSEventModifierFlags};

use ghostty_sys::*;

// NSEventModifierFlags（HIToolbox Events.h；与 v1 keymap.rs 同源）。
pub const MASK_ALPHA_SHIFT: u64 = 0x0001_0000; // capsLock 同位
pub const MASK_SHIFT: u64 = 0x0002_0000;
pub const MASK_CTRL: u64 = 0x0004_0000;
pub const MASK_ALT: u64 = 0x0008_0000;
pub const MASK_CMD: u64 = 0x0010_0000;

// 右侧修饰键的设备位（IOKit hidsystem/IOLLEvent.h；嵌入 mods 的 *_RIGHT
// 由 macOS Ghostty 同款换算使用——flags 只报左右合一，设备位补右侧）。
const NX_DEVICERSHIFTKEYMASK: u64 = 0x0000_0004;
const NX_DEVICERCMDKEYMASK: u64 = 0x0000_0010;
const NX_DEVICERALTKEYMASK: u64 = 0x0000_0040;
const NX_DEVICERCTLKEYMASK: u64 = 0x0000_2000;

/// NSEvent 修饰位 → ghostty mods（纯函数，单测覆盖）。
pub fn mods_from_flags(flags: u64) -> ghostty_input_mods_e {
    let mut m: u32 = GHOSTTY_MODS_NONE;
    if flags & MASK_SHIFT != 0 {
        m |= GHOSTTY_MODS_SHIFT;
    }
    if flags & MASK_CTRL != 0 {
        m |= GHOSTTY_MODS_CTRL;
    }
    if flags & MASK_ALT != 0 {
        m |= GHOSTTY_MODS_ALT;
    }
    if flags & MASK_CMD != 0 {
        m |= GHOSTTY_MODS_SUPER;
    }
    if flags & MASK_ALPHA_SHIFT != 0 {
        m |= GHOSTTY_MODS_CAPS;
    }
    if flags & NX_DEVICERSHIFTKEYMASK != 0 {
        m |= GHOSTTY_MODS_SHIFT_RIGHT;
    }
    if flags & NX_DEVICERCTLKEYMASK != 0 {
        m |= GHOSTTY_MODS_CTRL_RIGHT;
    }
    if flags & NX_DEVICERALTKEYMASK != 0 {
        m |= GHOSTTY_MODS_ALT_RIGHT;
    }
    if flags & NX_DEVICERCMDKEYMASK != 0 {
        m |= GHOSTTY_MODS_SUPER_RIGHT;
    }
    m
}

/// 「裸 ⌘ 键」判定（shell.rs ⌘W 决策用）：只有 SUPER，无 shift/option/ctrl。
pub fn is_bare_super(flags: u64) -> bool {
    flags & MASK_CMD != 0 && flags & (MASK_SHIFT | MASK_ALT | MASK_CTRL) == 0
}

/// consumed_mods 启发式（NSEvent+Extension.swift 同款）：ctrl/⌘ 从不参与
/// 文本翻译，其余修饰（shift/option/caps）视为已消耗。
pub fn consumed_mods_from_flags(flags: u64) -> ghostty_input_mods_e {
    let mut m: u32 = GHOSTTY_MODS_NONE;
    if flags & MASK_SHIFT != 0 {
        m |= GHOSTTY_MODS_SHIFT;
    }
    if flags & MASK_ALT != 0 {
        m |= GHOSTTY_MODS_ALT;
    }
    if flags & MASK_ALPHA_SHIFT != 0 {
        m |= GHOSTTY_MODS_CAPS;
    }
    m
}

/// keyDown 的 text 过滤（ghosttyCharacters 同款）：
/// - 单个 C0 控制字符 → None（ghostty 的 KeyEncoder 自己编码控制键；
///   ctrl+h 之类若把 "\u{8}" 也下发会双写）；
/// - 单个 PUA 功能键字符（F700-F8FF：方向键/功能键的 characters）→ None；
/// - 其余原样下发。
pub fn sanitize_text(s: &str) -> Option<String> {
    let mut it = s.chars();
    let (first, rest) = (it.next(), it.next().is_none());
    if let (Some(c), true) = (first, rest) {
        let v = c as u32;
        if v < 0x20 {
            return None;
        }
        if (0xF700..=0xF8FF).contains(&v) {
            return None;
        }
    }
    Some(s.to_string())
}

/// 组 `ghostty_input_key_s`（不含 text——text 由调用方按生命周期挂 C 串）。
/// `keycode` = 原生虚拟键码直传（见模块文档）。
pub fn key_event(
    event: &NSEvent,
    action: ghostty_input_action_e,
    mods: ghostty_input_mods_e,
) -> ghostty_input_key_s {
    let flags = event.modifierFlags().0 as u64;
    // unshifted codepoint：无修饰下的首码点（charactersIgnoringModifiers 在
    // ctrl 按下时行为会变，Ghostty 本尊用 byApplyingModifiers([])）。
    let unshifted = event
        .charactersByApplyingModifiers(NSEventModifierFlags(0))
        .and_then(|s| s.to_string().chars().next())
        .map(|c| c as u32)
        .unwrap_or(0);
    ghostty_input_key_s {
        action,
        mods,
        consumed_mods: consumed_mods_from_flags(flags),
        keycode: u32::from(event.keyCode()),
        text: std::ptr::null(),
        unshifted_codepoint: unshifted,
        composing: false,
    }
}

/// 滚轮 mods 打包（Ghostty.Input.ScrollMods 同款）：
/// bit0 = precision（精确增量），bit1-3 = momentum（NSEventPhase → 枚举）。
pub fn scroll_mods(
    precise: bool,
    momentum: objc2_app_kit::NSEventPhase,
) -> ghostty_input_scroll_mods_t {
    let mut v: i32 = 0;
    if precise {
        v |= 1;
    }
    let mom = match momentum {
        objc2_app_kit::NSEventPhase::None => GHOSTTY_MOUSE_MOMENTUM_NONE,
        objc2_app_kit::NSEventPhase::Began => GHOSTTY_MOUSE_MOMENTUM_BEGAN,
        objc2_app_kit::NSEventPhase::Stationary => GHOSTTY_MOUSE_MOMENTUM_STATIONARY,
        objc2_app_kit::NSEventPhase::Changed => GHOSTTY_MOUSE_MOMENTUM_CHANGED,
        objc2_app_kit::NSEventPhase::Ended => GHOSTTY_MOUSE_MOMENTUM_ENDED,
        objc2_app_kit::NSEventPhase::Cancelled => GHOSTTY_MOUSE_MOMENTUM_CANCELLED,
        objc2_app_kit::NSEventPhase::MayBegin => GHOSTTY_MOUSE_MOMENTUM_MAY_BEGIN,
        _ => GHOSTTY_MOUSE_MOMENTUM_NONE,
    };
    v |= (mom as i32) << 1;
    v
}

/// flagsChanged 的修饰键识别（SurfaceView_AppKit.flagsChanged 同款表）。
/// 返回该键码对应的 ghostty mods 位（None = 非修饰键）。
pub fn mod_key_of_code(code: u16) -> Option<ghostty_input_mods_e> {
    Some(match code {
        0x39 => GHOSTTY_MODS_CAPS,
        0x38 | 0x3C => GHOSTTY_MODS_SHIFT,
        0x3B | 0x3E => GHOSTTY_MODS_CTRL,
        0x3A | 0x3D => GHOSTTY_MODS_ALT,
        0x37 | 0x36 => GHOSTTY_MODS_SUPER,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mods_basic_and_sided() {
        assert_eq!(mods_from_flags(0), GHOSTTY_MODS_NONE);
        let cmd = mods_from_flags(MASK_CMD);
        assert_eq!(cmd & GHOSTTY_MODS_SUPER, GHOSTTY_MODS_SUPER);
        assert_eq!(cmd & GHOSTTY_MODS_SHIFT, 0);
        // 右 shift 设备位 → SHIFT + SHIFT_RIGHT。
        let rs = mods_from_flags(MASK_SHIFT | NX_DEVICERSHIFTKEYMASK);
        assert_eq!(rs & GHOSTTY_MODS_SHIFT, GHOSTTY_MODS_SHIFT);
        assert_eq!(rs & GHOSTTY_MODS_SHIFT_RIGHT, GHOSTTY_MODS_SHIFT_RIGHT);
        // caps（alphaShift 位）→ CAPS。
        assert_eq!(
            mods_from_flags(MASK_ALPHA_SHIFT) & GHOSTTY_MODS_CAPS,
            GHOSTTY_MODS_CAPS
        );
    }

    #[test]
    fn bare_super_detects_cmd_only() {
        assert!(is_bare_super(MASK_CMD));
        assert!(!is_bare_super(MASK_CMD | MASK_SHIFT));
        assert!(!is_bare_super(MASK_CMD | MASK_ALT));
        assert!(!is_bare_super(MASK_CMD | MASK_CTRL));
        assert!(!is_bare_super(MASK_SHIFT));
        assert!(!is_bare_super(0));
    }

    #[test]
    fn consumed_mods_exclude_ctrl_and_cmd() {
        let m = consumed_mods_from_flags(MASK_CMD | MASK_CTRL | MASK_SHIFT | MASK_ALT);
        assert_eq!(m & GHOSTTY_MODS_SUPER, 0, "⌘ 不算消耗");
        assert_eq!(m & GHOSTTY_MODS_CTRL, 0, "ctrl 不算消耗");
        assert_eq!(m & GHOSTTY_MODS_SHIFT, GHOSTTY_MODS_SHIFT);
        assert_eq!(m & GHOSTTY_MODS_ALT, GHOSTTY_MODS_ALT);
    }

    #[test]
    fn sanitize_text_filters_control_and_pua() {
        assert_eq!(sanitize_text("a").as_deref(), Some("a"));
        assert_eq!(sanitize_text("你好").as_deref(), Some("你好"));
        // C0 控制字符（ctrl+h 的 "\u{8}"）不下发：ghostty 自己编码。
        assert_eq!(sanitize_text("\u{8}"), None);
        assert_eq!(sanitize_text("\r"), None);
        // PUA 功能键（左箭头 F702 等）不下发。
        assert_eq!(sanitize_text("\u{F702}"), None);
        assert_eq!(sanitize_text("\u{F8FF}"), None);
    }

    #[test]
    fn scroll_mods_packing() {
        use objc2_app_kit::NSEventPhase;
        assert_eq!(scroll_mods(false, NSEventPhase::None), 0);
        // precision 单独。
        assert_eq!(scroll_mods(true, NSEventPhase::None) & 1, 1);
        // momentum 编码在 bit1-3：Began=1 → 1<<1。
        assert_eq!(
            scroll_mods(false, NSEventPhase::Began),
            (GHOSTTY_MOUSE_MOMENTUM_BEGAN as i32) << 1
        );
    }

    #[test]
    fn mod_key_table() {
        assert_eq!(mod_key_of_code(0x39), Some(GHOSTTY_MODS_CAPS));
        assert_eq!(mod_key_of_code(0x38), Some(GHOSTTY_MODS_SHIFT));
        assert_eq!(mod_key_of_code(0x3B), Some(GHOSTTY_MODS_CTRL));
        assert_eq!(mod_key_of_code(0x3A), Some(GHOSTTY_MODS_ALT));
        assert_eq!(mod_key_of_code(0x37), Some(GHOSTTY_MODS_SUPER));
        assert_eq!(mod_key_of_code(0x00), None, "字母键不是修饰键");
    }
}
