//! 第二语言（Python）只靠文档能解码本协议的帧流。
//!
//! 用 Rust 侧编码器把 17 条样例消息（六类）打成帧流写临时文件，再交给
//! `tests/second_language_decode.py`（纯标准库、纯文档实现）解码，
//! 输出必须与消息集一致；另证版本门（v=1 → 退出码 78，不猜）。
//! 无 python3 的环境跳过（macOS 自带）。

use std::io::Write;
use std::process::Command;

use ninja_protocol::{Message, encode_frame};

fn python_probe(py: &std::ffi::OsStr) -> bool {
    Command::new(py)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn second_language_decoder_decodes_host_frames() {
    let py = std::env::var_os("NINJA_PYTHON3").unwrap_or_else(|| "python3".into());
    if !python_probe(&py) {
        eprintln!("skip: 找不到 {py:?}（可用 NINJA_PYTHON3 指定）");
        return;
    }

    let samples = Message::sample_messages();
    let mut wire = Vec::new();
    for m in &samples {
        wire.extend_from_slice(&encode_frame(m).unwrap());
    }
    let path = std::env::temp_dir().join(format!(
        "ninja_ade_second_language_{}.bin",
        std::process::id()
    ));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(&wire).unwrap();
    drop(f);

    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/second_language_decode.py");
    let out = Command::new(&py)
        .arg(&script)
        .arg(&path)
        .output()
        .expect("跑 python 解码器");
    let _ = std::fs::remove_file(&path);

    assert!(
        out.status.success(),
        "python 解码失败：{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        samples.len(),
        "应解出全部 {} 条",
        samples.len()
    );
    for (line, m) in lines.iter().zip(&samples) {
        assert_eq!(line, &format!("{}\t{}", m.message_type(), m.v()));
    }
}

#[test]
fn second_language_decoder_exits_on_unsupported_v() {
    let py = std::env::var_os("NINJA_PYTHON3").unwrap_or_else(|| "python3".into());
    if !python_probe(&py) {
        eprintln!("skip: 找不到 {py:?}");
        return;
    }
    // 手造 v=1 帧：插件侧必须退出（退出码 78），不猜。
    let json = br#"{"type":"hit","v":1,"id":1,"kind":"path","text":"","row":0,"col":0,"pane":0,"modifiers":[]}"#;
    let mut wire = (json.len() as u32).to_le_bytes().to_vec();
    wire.extend_from_slice(json);
    let path = std::env::temp_dir().join(format!(
        "ninja_ade_second_language_bad_{}.bin",
        std::process::id()
    ));
    std::fs::write(&path, &wire).unwrap();

    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/second_language_decode.py");
    let out = Command::new(&py)
        .arg(&script)
        .arg(&path)
        .output()
        .expect("跑 python 解码器");
    let _ = std::fs::remove_file(&path);

    assert_eq!(
        out.status.code(),
        Some(78),
        "不支持 v 时第二实现必须退出：{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
