//! q3 契约测试：六类消息全覆盖。
//!
//! - 往返：sample → JSON → 解码 == 原值（17 条全量）。
//! - golden：每条消息与 tests/golden/<type>.json 字节一致（钉死线格式，
//!   第二语言实现的参照物；再生成见 examples/dump_messages.rs）。
//! - 信封不变量：每条序列化必含 `"v":0` 与 `"type":<type>`；六类各至少
//!   一条；KNOWN_TYPES 与枚举一一对应。
//! - 策略：宿主 lenient（未知字段忽略）；版本门（v 不符 → 不猜）；
//!   未知 type / 缺 v / 缺 type / 坏 JSON 的错误分类。
//! - 帧：前缀正确、逐字节喂入、背靠背、超长与空载荷拒绝。
//! - 第二语言（Python）解码与版本门退出：tests/second_language.rs。

use ninja_protocol::*;

fn goldens_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

// ---------------------------------------------------------------------------
// 覆盖与信封
// ---------------------------------------------------------------------------

#[test]
fn six_classes_all_covered() {
    let samples = Message::sample_messages();
    assert_eq!(samples.len(), KNOWN_TYPES.len(), "每条消息一个样例");
    for class in ["hit", "layer", "input", "spawn", "config", "theme"] {
        assert!(samples.iter().any(|m| m.class() == class), "六类缺 {class}");
    }
    for (m, ty) in samples.iter().zip(KNOWN_TYPES) {
        assert_eq!(m.message_type(), *ty);
        let json = m.to_json().unwrap();
        assert!(json.contains(r#""v":0"#), "{ty} 序列化缺 v：{json}");
        assert!(
            json.contains(&format!(r#""type":"{ty}""#)),
            "{ty} 序列化缺 type：{json}"
        );
        // 每类还有方向语义（layer.close 双向；其余可判）。
        let _ = m.direction();
    }
}

#[test]
fn known_types_match_enum() {
    for m in Message::sample_messages() {
        assert!(is_known_type(m.message_type()));
    }
    assert!(!is_known_type("hit.claim.v2"));
    assert!(!is_known_type("agent")); // 不存在的类
}

// ---------------------------------------------------------------------------
// 往返 + golden
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_every_message() {
    for m in Message::sample_messages() {
        let json = m.to_json().unwrap();
        let back = Message::from_json(&json).unwrap();
        assert_eq!(back, m, "往返失真：{json}");
        // 两侧入口等价解码。
        assert_eq!(Message::decode_host(json.as_bytes()).unwrap(), m);
        assert_eq!(Message::decode_plugin(json.as_bytes()).unwrap(), m);
    }
}

#[test]
fn golden_files_pin_wire_format() {
    let dir = goldens_dir();
    let mut seen = std::collections::BTreeSet::new();
    for m in Message::sample_messages() {
        let path = dir.join(format!("{}.json", m.message_type()));
        let golden = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "缺 golden {path:?}（{e}）；再生成：cargo run -p ninja-protocol \
                 --example dump_messages，见 examples/dump_messages.rs"
            )
        });
        assert_eq!(
            m.to_json().unwrap(),
            golden.trim_end(),
            "线格式漂移：{}（改字段=改协议，走版本规则；或更新 golden）",
            m.message_type()
        );
        seen.insert(m.message_type().to_string());
    }
    // golden 目录没有多余文件。
    let on_disk = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .trim_end_matches(".json")
                .to_string()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(seen, on_disk, "golden 目录与消息集不一致");
}

// ---------------------------------------------------------------------------
// 策略：lenient / 版本门 / 错误分类
// ---------------------------------------------------------------------------

#[test]
fn host_decode_ignores_unknown_fields() {
    // 未来字段 + 错版命名的多余键：宿主 lenient，照收。
    let json = r#"{"type":"hit","v":0,"id":7,"kind":"path","text":"a.rs","row":1,"col":2,"pane":0,"modifiers":[],"future_field":{"nested":[1,2]},"v2_note":"whatever"}"#;
    let m = Message::decode_host(json.as_bytes()).unwrap();
    match m {
        Message::Hit(h) => {
            assert_eq!(h.id, 7);
            assert_eq!(h.text, "a.rs");
            assert!(h.cwd.is_empty(), "未带的 cwd 走缺省空串");
            assert!(h.modifiers.is_empty());
        }
        other => panic!("应解出 hit，得到 {:?}", other.message_type()),
    }
}

#[test]
fn theme_set_decode_contract() {
    // theme.set 是唯一携带色板的消息。lenient：未知字段照收。
    let json = r##"{"type":"theme.set","v":0,"name":"x","bg":"#002b36","fg":"#839496","cursor":"#93a1a1","selection_bg":"#073642","selection_alpha":102,"divider":"#586e75","ansi":["#000000","#000000","#000000","#000000","#000000","#000000","#000000","#000000","#000000","#000000","#000000","#000000","#000000","#000000","#000000","#000000"],"future":1}"##;
    match Message::decode_host(json.as_bytes()).unwrap() {
        Message::ThemeSet(t) => {
            assert_eq!(t.name, "x");
            assert_eq!(t.bg, "#002b36");
            assert_eq!(t.selection_alpha, 102);
            assert_eq!(t.ansi.len(), 16);
            assert_eq!(t.ansi[15], "#000000");
            assert_eq!(Message::ThemeSet(t).direction(), Direction::PluginToHost);
        }
        other => panic!("应解出 theme.set，得到 {:?}", other.message_type()),
    }
    // ansi 必须「恰好 16 个」：短写/长写都是解码错误（不猜）。
    let mk = |n: usize| {
        let ansi: Vec<String> = (0..n).map(|_| "#000000".to_string()).collect();
        format!(
            r##"{{"type":"theme.set","v":0,"name":"x","bg":"#0","fg":"#0","cursor":"#0","selection_bg":"#0","selection_alpha":1,"divider":"#0","ansi":{:?}}}"##,
            ansi
        )
    };
    for bad in [mk(15), mk(17)] {
        assert!(
            matches!(Message::from_json(&bad), Err(DecodeError::InvalidJson(_))),
            "ansi 长度不是 16 必须拒收：{bad}"
        );
    }
    // 缺必填字段（如 divider）同理拒收。
    let missing = r##"{"type":"theme.set","v":0,"name":"x","bg":"#0","fg":"#0","cursor":"#0","selection_bg":"#0","selection_alpha":1,"ansi":["#000000","#000000","#000000","#000000","#000000","#000000","#000000","#000000","#000000","#000000","#000000","#000000","#000000","#000000","#000000","#000000"]}"##;
    assert!(matches!(
        Message::from_json(missing),
        Err(DecodeError::InvalidJson(_))
    ));
}

#[test]
fn version_gate_rejects_future_and_past() {
    let future = r#"{"type":"hit","v":1,"id":7,"kind":"path","text":"a.rs","row":1,"col":2,"pane":0,"modifiers":[]}"#;
    // 版本错误优先于一切字段错误：下面这条还有坏字段，也必须报版本。
    let future_broken = r#"{"type":"hit","v":99,"id":"not-a-number"}"#;
    for text in [future, future_broken] {
        for entry in [
            Message::from_json(text),
            Message::decode_host(text.as_bytes()),
            Message::decode_plugin(text.as_bytes()),
        ] {
            match entry {
                Err(DecodeError::UnsupportedVersion { got, supported }) => {
                    assert_eq!(supported, PROTOCOL_VERSION);
                    assert!(got > 0);
                }
                other => panic!("版本门失效：{other:?}（{text}）"),
            }
        }
    }
    // 插件侧规则（文档化契约）：此错误 => 必须退出，不猜。错误文本里
    // 就写着处置方式，供嵌入方引用。
    let err = Message::decode_plugin(future.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("退出"));
}

#[test]
fn wrong_version_message_refuses_to_encode() {
    let mut m = Message::sample_messages().into_iter().next().unwrap();
    if let Message::Hit(h) = &mut m {
        h.v = 1;
    } else {
        unreachable!();
    }
    match m.to_json() {
        Err(EncodeError::WrongVersion {
            got: 1,
            supported: 0,
        }) => {}
        other => panic!("编码未门版本：{other:?}"),
    }
    assert!(encode_frame(&m).is_err());
}

#[test]
fn error_classification() {
    // 缺 v。
    let e = Message::from_json(r#"{"type":"hit","id":7}"#).unwrap_err();
    assert_eq!(e, DecodeError::MissingVersion);
    // 缺 type。
    let e = Message::from_json(r#"{"v":0,"id":7}"#).unwrap_err();
    assert_eq!(e, DecodeError::MissingType);
    // 未知 type。
    let e = Message::from_json(r#"{"v":0,"type":"agent"}"#).unwrap_err();
    assert_eq!(e, DecodeError::UnknownType("agent".to_string()));
    // 坏 JSON。
    assert!(matches!(
        Message::from_json("not json"),
        Err(DecodeError::InvalidJson(_))
    ));
    // 已知 type 但必填字段缺失 / 类型错。
    assert!(matches!(
        Message::from_json(r#"{"type":"hit","v":0}"#),
        Err(DecodeError::InvalidJson(_))
    ));
    assert!(matches!(
        Message::from_json(
            r#"{"type":"hit","v":0,"id":"x","kind":"path","text":"","row":0,"col":0,"pane":0,"modifiers":[]}"#
        ),
        Err(DecodeError::InvalidJson(_))
    ));
    // 非 UTF-8 载荷。
    assert!(matches!(
        Message::decode_host(&[0xFF, 0xFE]),
        Err(DecodeError::InvalidJson(_))
    ));
    // 枚举值越界（不能猜）。
    assert!(matches!(
        Message::from_json(
            r#"{"type":"hit","v":0,"id":7,"kind":"agent","text":"","row":0,"col":0,"pane":0,"modifiers":[]}"#
        ),
        Err(DecodeError::InvalidJson(_))
    ));
}

// ---------------------------------------------------------------------------
// 帧编解码
// ---------------------------------------------------------------------------

#[test]
fn frame_prefix_is_u32le_of_payload_only() {
    let m = &Message::sample_messages()[0];
    let json = m.to_json().unwrap();
    let frame = encode_frame(m).unwrap();
    assert_eq!(frame.len(), 4 + json.len());
    assert_eq!(
        &frame[..4],
        &(json.len() as u32).to_le_bytes(),
        "前缀 = 载荷字节数（不含前缀自身），小端"
    );
    assert_eq!(&frame[4..], json.as_bytes());
}

#[test]
fn frame_decoder_handles_byte_at_a_time_and_back_to_back() {
    let samples = Message::sample_messages();
    let mut wire = Vec::new();
    for m in &samples {
        wire.extend_from_slice(&encode_frame(m).unwrap());
    }
    // 逐字节喂：半帧绝不误弹；弹出的必须依次等于整条 JSON。
    let mut dec = FrameDecoder::new();
    let mut out = Vec::new();
    for b in wire.iter() {
        dec.extend(&[*b]).unwrap();
        while let Some(frame) = dec.pop() {
            out.push(frame.unwrap());
        }
    }
    assert_eq!(out.len(), samples.len());
    for (payload, m) in out.iter().zip(&samples) {
        assert_eq!(&payload[..], m.to_json().unwrap().as_bytes());
        assert_eq!(Message::decode_host(payload).unwrap(), *m);
    }
    assert!(dec.pop().is_none(), "无残留");
}

#[test]
fn frame_rejects_oversized_and_empty() {
    // 声明超长：MAX+1（前缀后无需真载荷，pop 即拒）。
    let mut dec = FrameDecoder::new();
    dec.extend(&(MAX_FRAME_BYTES as u32 + 1).to_le_bytes())
        .unwrap();
    match dec.pop() {
        Some(Err(FrameError::FrameTooLarge { declared, max })) => {
            assert_eq!(declared, MAX_FRAME_BYTES + 1);
            assert_eq!(max, MAX_FRAME_BYTES);
        }
        other => panic!("超长帧未拒：{other:?}"),
    }
    // 缓冲炸弹：无脑 extend 超上限也拒（不等前缀声明）。
    let mut dec = FrameDecoder::new();
    let bomb = vec![0u8; MAX_FRAME_BYTES + frame::FRAME_PREFIX_BYTES + 1];
    assert!(dec.extend(&bomb).is_err());
    // 空载荷：len=0 拒。
    let mut dec = FrameDecoder::new();
    dec.extend(&0u32.to_le_bytes()).unwrap();
    match dec.pop() {
        Some(Err(FrameError::EmptyPayload)) => {}
        other => panic!("空帧未拒：{other:?}"),
    }
    // 半帧：只给前缀，pop 挂起等载荷。
    let m = &Message::sample_messages()[0];
    let full = encode_frame(m).unwrap();
    let mut dec = FrameDecoder::new();
    dec.extend(&full[..4]).unwrap();
    assert!(dec.pop().is_none());
    dec.extend(&full[4..]).unwrap();
    assert_eq!(dec.pop().unwrap().unwrap(), m.to_json().unwrap().into_bytes());
}
