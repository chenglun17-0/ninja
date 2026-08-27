//! p0 冒烟：证明 ninja 宿主真的链上了钉版 libghostty-vt 的公开 C API。
//!
//! 不是「编译通过」级别的验证——每个测试都跨 FFI 调用
//! ghostty commit a887df42c56f6de86c0fe6da9c4eeca37931e083 的真实实现：
//! build_info、VT/OSC 解析、键编码。全部走 `include/ghostty/` 公开 C API，
//! 不碰内部 `ghostty.h`。

use libghostty_vt::{
    build_info,
    key::{Action, Encoder as KeyEncoder, Event as KeyEvent, Key, Mods},
    osc::{CommandType, Parser as OscParser},
    terminal::{Options as TerminalOptions, Terminal},
};

#[test]
fn links_against_pinned_libghostty_vt() {
    // build_info 跨 FFI 读编译期配置。钉点（ghostty a887df42，1.3.2-dev）
    // 的 libghostty-vt C API 自报版本 0.1.0-dev（C API pre-1.0，STACK.md 已
    // 接受）；版本对不上说明链到的不是钉版库。
    assert_eq!(build_info::version_string().unwrap(), "0.1.0-dev");
    assert_eq!(build_info::major_version().unwrap(), 0);
    assert_eq!(build_info::minor_version().unwrap(), 1);
    assert_eq!(build_info::patch_version().unwrap(), 0);
    // 默认 features 含 kitty-graphics，vendored 构建应如实上报。
    assert!(build_info::supports_kitty_graphics().unwrap());
}

#[test]
fn terminal_processes_vt_and_osc() {
    let mut term = Terminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: 1000,
    })
    .unwrap();
    assert_eq!(term.cols().unwrap(), 80);
    assert_eq!(term.rows().unwrap(), 24);

    // OSC 0/2 设置标题：走完整 VT 流解析器，落进终端状态。
    term.vt_write(b"\x1b]0;ninja-p0\x07");
    assert_eq!(term.title().unwrap(), "ninja-p0");

    // 普通文本让光标右移（不依赖 0/1 基准）。
    let x0 = term.cursor_x().unwrap();
    term.vt_write(b"hi");
    assert_eq!(term.cursor_x().unwrap(), x0 + 2);

    // ESC 终止的 OSC 与 BEL 等价。
    term.vt_write(b"\x1b]2;ninja-esc\x1b\\");
    assert_eq!(term.title().unwrap(), "ninja-esc");
}

#[test]
fn osc_parser_extracts_title() {
    let mut parser = OscParser::new().unwrap();
    for byte in b"0;ninja-osc" {
        parser.next_byte(*byte);
    }
    let command = parser.end(0x07);
    // 断言到命令类型级：libghostty-vt 0.2.1 包装层对
    // CHANGE_WINDOW_TITLE_STR 的字符串读取有上游缺陷（恒空串），
    // 字符串内容已由 terminal_processes_vt_and_osc 里的 Terminal::title()
    // 走完整 VT 流路径证明。
    assert!(
        matches!(command.command_type(), CommandType::ChangeWindowTitle { .. }),
        "expected ChangeWindowTitle"
    );
}

#[test]
fn key_encoder_encodes_input_events() {
    let mut encoder = KeyEncoder::new().unwrap();

    // 无修饰的 'a' → 字面字节。
    let mut event = KeyEvent::new().unwrap();
    event
        .set_action(Action::Press)
        .set_key(Key::A)
        .set_utf8(Some("a"));
    let mut out = Vec::new();
    encoder.encode_to_vec(&event, &mut out).unwrap();
    assert_eq!(out, b"a");

    // Ctrl+A → 0x01。
    let mut event = KeyEvent::new().unwrap();
    event
        .set_action(Action::Press)
        .set_key(Key::A)
        .set_utf8(Some("a"))
        .set_mods(Mods::CTRL);
    let mut out = Vec::new();
    encoder.encode_to_vec(&event, &mut out).unwrap();
    assert_eq!(out, b"\x01");

    // 左方向键（非应用模式）→ CSI D。
    let mut event = KeyEvent::new().unwrap();
    event.set_action(Action::Press).set_key(Key::ArrowLeft);
    let mut out = Vec::new();
    encoder.encode_to_vec(&event, &mut out).unwrap();
    assert_eq!(out, b"\x1b[D");
}
