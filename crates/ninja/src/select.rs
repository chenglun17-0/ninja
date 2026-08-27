//! 选区文本与粘贴编码（剪贴板走宿主，不走插件——STACK.md）。

use libghostty_vt::fmt::Format;
use libghostty_vt::selection::FormatOptions;
use libghostty_vt::terminal::{Mode, Terminal};

/// 取当前选区的纯文本（unwrap + trim，与 Ghostty copy 行为一致）。
pub fn selection_text(term: &Terminal<'_, '_>) -> Option<String> {
    let opts = FormatOptions::new()
        .with_emit_format(Format::Plain)
        .with_unwrap(true)
        .with_trim(true);
    let bytes = term.format_selection_alloc(None, opts).ok()??;
    let bytes: &[u8] = &bytes;
    String::from_utf8(bytes.to_vec()).ok()
}

/// 粘贴编码：尊重 bracketed paste（mode 2004）。返回写 PTY 的字节。
pub fn paste_bytes(term: &Terminal<'_, '_>, text: &str) -> Vec<u8> {
    let bracketed = term.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
    let mut data = text.as_bytes().to_vec();
    let mut out = vec![0u8; data.len() + 32];
    match libghostty_vt::paste::encode(&mut data, bracketed, &mut out) {
        Ok(n) => {
            out.truncate(n);
            out
        }
        Err(_) => {
            // 缓冲不够（几乎不可能）：退化为裸文本 + CR。
            let mut fallback = data;
            if !bracketed {
                for b in fallback.iter_mut() {
                    if *b == b'\n' {
                        *b = b'\r';
                    }
                }
            }
            fallback
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::{Terminal, TerminalOptions};

    fn term_with(text: &[u8]) -> Terminal<'static, 'static> {
        let mut t = Terminal::new(TerminalOptions {
            cols: 20,
            rows: 4,
            max_scrollback: 50,
        })
        .unwrap();
        t.vt_write(text);
        t
    }

    #[test]
    fn selection_text_via_gesture_word_select() {
        use libghostty_vt::screen::GridRef;
        use libghostty_vt::terminal::Point;
        use libghostty_vt::selection::gesture::{Gesture, PressEvent};
        use libghostty_vt::terminal::PointCoordinate;

        let term = term_with(b"hello world");
        let mut gesture = Gesture::new().unwrap();
        fn at<'t>(t: &'t Terminal<'_, '_>, x: u16, y: u32) -> GridRef<'t> {
            t.grid_ref(Point::Active(PointCoordinate { x, y })).unwrap()
        }

        // 第一击：无选区（单击只定位锚点）。
        let mut press1 = PressEvent::new().unwrap();
        press1.set_time(std::time::Duration::from_millis(100)).unwrap();
        press1.set_repeat_distance(4.0).unwrap();
        press1.set_repeat_interval(std::time::Duration::from_millis(400)).unwrap();
        assert!(
            press1
                .apply(&mut gesture, &term, at(&term, 2, 0))
                .unwrap()
                .is_none()
        );

        let mut press2 = PressEvent::new().unwrap();
        press2.set_time(std::time::Duration::from_millis(400)).unwrap();
        press2.set_repeat_distance(4.0).unwrap();
        press2.set_repeat_interval(std::time::Duration::from_millis(400)).unwrap();
        let sel2 = press2
            .apply(&mut gesture, &term, at(&term, 2, 0))
            .unwrap()
            .expect("press 2");
        term.set_selection(Some(&sel2)).unwrap();

        let text = selection_text(&term).expect("selection text");
        assert_eq!(text.trim(), "hello");
    }

    #[test]
    fn paste_respects_bracketed_mode() {
        let mut term = term_with(b"");
        let plain = paste_bytes(&term, "abc\ndef");
        assert_eq!(plain, b"abc\rdef"); // 非 bracketed：\n → \r

        term.vt_write(b"\x1b[?2004h");
        let bracketed = paste_bytes(&term, "abc\ndef");
        assert_eq!(bracketed, b"\x1b[200~abc\ndef\x1b[201~");
    }
}
