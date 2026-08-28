//! p4 命中识别（纯函数，无 GUI / 无 vt 依赖）：给定点击行各列的
//! grapheme 文本 + 点击列 + 该 cell 的 OSC-8 URI，认出可点对象。
//!
//! 输入由 view 侧从 vt 网格收集（[`crate::view`] 的 `cmd_click`），
//! 本模块只做字符串推理，方便单测钉行为：
//!
//! 1. **OSC-8 优先**：点击 cell 带 hyperlink → 直接 `kind=Osc8`、
//!    `text=URI`（显示文本与 URI 可以不同，协议以 URI 为准）。
//! 2. 否则**行内扩展**：以空白（blank/whitespace cell）为界从点击处
//!    向两侧扩展 token；宽字形（CJK）的 spacer 尾巴列是续列不是边界。
//! 3. **尾部/头部标点裁剪**：`(<https://x.com/a>).` 这类括号引号和
//!    `.,;:!?` 不属于链接；`key=value` 形态剥掉 `key=` 前缀。
//! 4. **分类**：URL 优先（合法 `scheme://` 或 `www.` 前缀），其次
//!    Path（`/`、`~/`、`./`、`../` 开头，或含 `/` 且末段带 `.` 的
//!    相对路径——编译器输出的 `src/main.rs:42:13` 属于此类，
//!    `file:line:col` 后缀原样保留在 text 里）。其余不命中。
//!
//! 注意：OSC-8 的覆盖列区间（下划线渲染用）本阶段不展开——只有点击
//! cell 的 URI 可知，起止列先记点击 cell（p5 画层时再按行扫 URI）。

use ninja_protocol::HitKind;

/// 点击行的一列（vt cell 的文本投影）。
#[derive(Clone, Debug, PartialEq)]
pub enum RowCell {
    /// 空白格（无文本或全空白）——token 边界。
    Blank,
    /// 宽字形（CJK）后的 spacer 尾巴 / 软换行占位列——无文本但属于
    /// 前一列的延续，**不是**边界。
    Cont,
    /// 有 grapheme 文本的格。
    Text(String),
}

/// 识别结果：种类 + 命中文本 + 行内列区间 `[start, end)`。
#[derive(Clone, Debug, PartialEq)]
pub struct LinkHit {
    pub kind: HitKind,
    pub text: String,
    pub start_col: u16,
    pub end_col: u16,
}

/// 头部裁剪集：shell 输出常见的包裹符号（`(<https://…>)`、
/// `'/path/x'`、`【url】` 等）。
const LEADING_TRIM: &[char] = &[
    '<', '(', '{', '[', '（', '【', '「', '『', '"', '\'', '“', '‘', '«', '‹',
];
/// 尾部裁剪集：同上收口 + 句读（`path/x,` `https://x/a!`）。
const TRAILING_TRIM: &[char] = &[
    '>', ')', '}', ']', '）', '】', '」', '』', '"', '\'', '”', '’', '»', '›', '.', ',',
    ';', ':', '!', '?', '*', '，', '。', '、', '；', '：', '？', '！',
];

/// token 内出现即整段放弃的字符（管道/反引号几乎不可能属于可点路径）。
const REJECT_CHARS: &[char] = &['|', '`'];

/// 识别入口。`cells` 是点击行按列的文本投影；`click_col` 点击列；
/// `osc8` 点击 cell 的 OSC-8 URI（None/空 = 无链接）。
pub fn recognize(cells: &[RowCell], click_col: usize, osc8: Option<&str>) -> Option<LinkHit> {
    // 1) OSC-8 优先：有链接就认链接，不做文本猜测。
    if let Some(uri) = osc8 {
        if !uri.is_empty() {
            let col = click_col.min(u16::MAX as usize) as u16;
            return Some(LinkHit {
                kind: HitKind::Osc8,
                text: uri.to_string(),
                start_col: col,
                end_col: col.saturating_add(1),
            });
        }
    }

    let n = cells.len();
    if n == 0 || click_col >= n {
        return None;
    }
    // 2) 扩展：空白为界（Cont 不是边界，点在宽字形尾巴上照样能选中它）。
    let is_delim = |c: &RowCell| match c {
        RowCell::Blank => true,
        RowCell::Text(t) => t.is_empty() || t.chars().all(char::is_whitespace),
        RowCell::Cont => false,
    };
    if is_delim(&cells[click_col]) {
        return None; // 点在空白上（Cont 宽字形尾巴不是空白）
    }
    let mut start = click_col;
    while start > 0 && !is_delim(&cells[start - 1]) {
        start -= 1;
    }
    let mut end = click_col + 1;
    while end < n && !is_delim(&cells[end]) {
        end += 1;
    }

    // 逐列文本（带列号，裁剪时同步缩区间）。
    let mut parts: Vec<(usize, String)> = cells[start..end]
        .iter()
        .enumerate()
        .filter_map(|(i, c)| match c {
            RowCell::Text(t) if !t.is_empty() => Some((start + i, t.clone())),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        return None;
    }

    // 3) 裁剪：头/尾标点整列剥；`key=` 前缀剥（url=https://… / path=/x）。
    loop {
        let Some((_, last)) = parts.last_mut() else { break };
        let trimmed = last.trim_end_matches(|c| TRAILING_TRIM.contains(&c)).to_string();
        if trimmed.len() == last.len() {
            break;
        }
        if trimmed.is_empty() {
            parts.pop();
        } else {
            *last = trimmed;
            break;
        }
    }
    loop {
        let Some((_, first)) = parts.first_mut() else { break };
        let trimmed = first.trim_start_matches(|c| LEADING_TRIM.contains(&c)).to_string();
        if trimmed.len() == first.len() {
            break;
        }
        if trimmed.is_empty() {
            parts.remove(0);
        } else {
            *first = trimmed;
            break;
        }
    }
    if parts.is_empty() {
        return None;
    }
    let joined: String = parts.iter().map(|(_, t)| t.as_str()).collect();
    let joined = match joined.find('=') {
        // `label=` 前缀（label 是 [A-Za-z0-9_.+-]+）：`url=https://x.com`。
        Some(eq) if eq > 0 => {
            let (label, rest) = joined.split_at(eq);
            let is_label = !label.is_empty()
                && label
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '-'));
            if is_label {
                rest[1..].to_string()
            } else {
                joined
            }
        }
        _ => joined,
    };

    // 4) 分类。
    let Some(kind) = classify(&joined) else { return None };
    Some(LinkHit {
        kind,
        text: joined,
        start_col: parts.first().map(|(c, _)| *c).unwrap_or(start) as u16,
        end_col: parts.last().map(|(c, _)| *c + 1).unwrap_or(end) as u16,
    })
}

/// token 分类：URL 优先，其次 Path，其余 None。
/// 含 `://` 但 scheme 不合法的串整体放弃（既不是 URL 也不是路径）。
fn classify(token: &str) -> Option<HitKind> {
    if token.is_empty() {
        return None;
    }
    // 管道/反引号：几乎不可能是路径或 URL 的一部分。
    if token.chars().any(|c| REJECT_CHARS.contains(&c)) {
        return None;
    }
    if token.contains("://") {
        return if is_url(token) {
            Some(HitKind::Url)
        } else {
            None
        };
    }
    if is_url(token) {
        return Some(HitKind::Url);
    }
    if is_path(token) {
        return Some(HitKind::Path);
    }
    None
}

/// URL：合法 scheme（RFC 3986 首字符字母，后随字母数字 `+.-`）+ `://`，
/// 或 `www.` 前缀（无 scheme 的裸域，浏览器惯例）。
fn is_url(token: &str) -> bool {
    if token.starts_with("www.") || token.starts_with("WWW.") {
        return true;
    }
    if let Some(pos) = token.find("://") {
        let scheme = &token[..pos];
        let mut chars = scheme.chars();
        match chars.next() {
            Some(c) if c.is_ascii_alphabetic() => {}
            _ => return false,
        }
        return chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
    }
    false
}

/// Path：`/` `~/` `./` `../` 开头（绝对/家目录/显式相对），或含 `/` 且
/// 末段（剥掉可选 `:line[:col]` 后）带 `.` 的相对路径
/// （`src/main.rs:42:13`；`and/or` 这类纯词组不成路径）。
fn is_path(token: &str) -> bool {
    if token.starts_with('/')
        || token.starts_with("~/")
        || token.starts_with("./")
        || token.starts_with("../")
    {
        return true;
    }
    let Some(last_slash) = token.rfind('/') else {
        return false;
    };
    let last_seg = &token[last_slash + 1..];
    // 剥 :line[:col] 后缀（ASCII 数字）。
    let last_seg = strip_line_col(last_seg);
    last_seg.contains('.')
}

/// 从尾部剥 `:digits`（最多两段：`:line` / `:line:col`）。
/// 非 ASCII 数字尾或无后缀 → 原样返回。
fn strip_line_col(s: &str) -> &str {
    let b = s.as_bytes();
    let mut end = b.len();
    for _ in 0..2 {
        let Some(colon) = b[..end].iter().rposition(|&c| c == b':') else {
            return &s[..end];
        };
        let digits = colon + 1..end;
        if digits.is_empty() || !b[digits].iter().all(u8::is_ascii_digit) {
            return &s[..end];
        }
        end = colon;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 便捷构造：整行字符串按 char 拆列（宽字形测试手动拼 Cont）。
    fn row(s: &str) -> Vec<RowCell> {
        s.chars()
            .map(|c| {
                if c.is_whitespace() {
                    RowCell::Blank
                } else {
                    RowCell::Text(c.to_string())
                }
            })
            .collect()
    }

    fn text_hit(cells: &[RowCell], click: usize) -> Option<LinkHit> {
        recognize(cells, click, None)
    }

    #[test]
    fn absolute_path() {
        let cells = row("see /usr/local/bin/foo now");
        let h = text_hit(&cells, 12).unwrap();
        assert_eq!(h.kind, HitKind::Path);
        assert_eq!(h.text, "/usr/local/bin/foo");
        // 列 4..=21 是 token（end 为开区间），两段空白都不进。
        assert_eq!(h.start_col, 4);
        assert_eq!(h.end_col, 22);
        // 点在 token 任意列都命中同一段。
        let h2 = text_hit(&cells, 21).unwrap();
        assert_eq!(h2.text, "/usr/local/bin/foo");
    }

    #[test]
    fn home_and_dot_paths() {
        let h = text_hit(&row("edit ~/.config/ninja/ninja.toml"), 7).unwrap();
        assert_eq!(h.kind, HitKind::Path);
        assert_eq!(h.text, "~/.config/ninja/ninja.toml");

        let h = text_hit(&row("run ./target/debug/ninja ok"), 5).unwrap();
        assert_eq!(h.kind, HitKind::Path);
        assert_eq!(h.text, "./target/debug/ninja");

        let h = text_hit(&row("go ../sibling/x.txt"), 5).unwrap();
        assert_eq!(h.kind, HitKind::Path);
        assert_eq!(h.text, "../sibling/x.txt");
    }

    #[test]
    fn relative_path_with_line_col_suffix_kept() {
        // 协议 golden 样例形态：src/main.rs:42:13 → Path，后缀原样保留。
        let cells = row("error src/main.rs:42:13 here");
        let h = text_hit(&cells, 9).unwrap();
        assert_eq!(h.kind, HitKind::Path);
        assert_eq!(h.text, "src/main.rs:42:13");
        // 也支持只有行号的形态。
        let h = text_hit(&row("at lib/core.rs:7"), 4).unwrap();
        assert_eq!(h.kind, HitKind::Path);
        assert_eq!(h.text, "lib/core.rs:7");
    }

    #[test]
    fn urls_with_query_and_trailing_punct() {
        // 查询串里的 = & ? 必须完整保留；句尾标点裁掉。
        let cells = row("go https://example.com/s?q=1&b=2.");
        let h = text_hit(&cells, 5).unwrap();
        assert_eq!(h.kind, HitKind::Url);
        assert_eq!(h.text, "https://example.com/s?q=1&b=2");

        // 括号包裹 + 尾部句号：两头裁剪。
        let h = text_hit(&row("see (<https://x.com/a>)"), 9).unwrap();
        assert_eq!(h.kind, HitKind::Url);
        assert_eq!(h.text, "https://x.com/a");

        // 裸 www. 域。
        let h = text_hit(&row("ref www.ghostty.org,"), 5).unwrap();
        assert_eq!(h.kind, HitKind::Url);
        assert_eq!(h.text, "www.ghostty.org");

        // 坏 scheme（数字开头 / 非法字符）不算 URL。
        assert!(text_hit(&row("x 1https://a.com"), 3).is_none());
        assert!(text_hit(&row("x ht!tps://a.com"), 3).is_none());
    }

    #[test]
    fn label_eq_prefix_stripped() {
        let h = text_hit(&row("url=https://x.com/a"), 5).unwrap();
        assert_eq!(h.kind, HitKind::Url);
        assert_eq!(h.text, "https://x.com/a");
    }

    #[test]
    fn quoted_path_trimmed() {
        let h = text_hit(&row("'/tmp/x.txt'"), 2).unwrap();
        assert_eq!(h.kind, HitKind::Path);
        assert_eq!(h.text, "/tmp/x.txt");
    }

    #[test]
    fn cjk_wide_cells_are_not_boundaries() {
        // 你/好 各占 2 列：宽列 + Cont 尾巴；路径必须完整跨过宽字形。
        // 基段 "/Users/jal/my_repos/ninja/" 占 26 列。
        let mut cells = row("/Users/jal/my_repos/ninja/");
        assert_eq!(cells.len(), 26);
        cells.push(RowCell::Text("你".into()));
        cells.push(RowCell::Cont);
        cells.push(RowCell::Text("好".into()));
        cells.push(RowCell::Cont);
        cells.extend(row("/x.txt"));
        // 点在「你」的 spacer 尾巴上（Cont，列 27）也应选中整段。
        let h = text_hit(&cells, 27).unwrap();
        assert_eq!(h.kind, HitKind::Path);
        assert_eq!(h.text, "/Users/jal/my_repos/ninja/你好/x.txt");
        // 中文标点裁剪：`/tmp/x.txt，` → 尾部全角逗号剥掉。
        let mut cells = row("/tmp/x.txt");
        cells.push(RowCell::Text("，".into()));
        let h = text_hit(&cells, 2).unwrap();
        assert_eq!(h.text, "/tmp/x.txt");
    }

    #[test]
    fn no_hit_cases() {
        // 空白列、纯词、无点的相对串、裸点号。
        assert!(text_hit(&row("hello world"), 3).is_none());
        assert!(text_hit(&row("hello world"), 8).is_none());
        assert!(text_hit(&row("a and/or b"), 4).is_none()); // 末段无点
        assert!(text_hit(&row("  "), 0).is_none());
        assert!(text_hit(&row("a.b c"), 0).is_none()); // 无斜杠不按相对路径猜
        // 管道串整体放弃。
        assert!(text_hit(&row("a|b/c.txt"), 1).is_none());
    }

    #[test]
    fn osc8_wins_over_text() {
        let cells = row("just words here");
        let h = recognize(&cells, 6, Some("https://osc8.example/x")).unwrap();
        assert_eq!(h.kind, HitKind::Osc8);
        assert_eq!(h.text, "https://osc8.example/x");
        assert_eq!(h.start_col, 6);
        assert_eq!(h.end_col, 7);
        // 空 URI 视为无链接（继续走文本启发式）。
        assert!(recognize(&cells, 6, Some("")).is_none());
    }

    #[test]
    fn strip_line_col_only_digits() {
        assert_eq!(strip_line_col("main.rs:42:13"), "main.rs");
        assert_eq!(strip_line_col("main.rs:42"), "main.rs");
        // 非数字后缀不剥。
        assert_eq!(strip_line_col("a:b/c.txt"), "a:b/c.txt");
        assert_eq!(strip_line_col("x:1y"), "x:1y");
    }
}
