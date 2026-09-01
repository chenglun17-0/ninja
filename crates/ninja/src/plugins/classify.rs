//! hit 识别、cwd 解析、theme.set 校验、键名转换（纯函数，单测钉行为）。

use ninja_protocol::{HitKind, Modifier, ThemeSet};

use objc2::DefinedClass;

use crate::surface::SurfaceHostView;

// ---------------------------------------------------------------------------
// hit 识别（网格源）纯函数
// ---------------------------------------------------------------------------

/// token 字符集：路径/URL 常见字符。`:` 收进来（`:line:col` 后缀由
/// 插件剥）。
fn is_token_char(c: char) -> bool {
    c.is_alphanumeric() || "._/@-~+:=%?#&[]()".contains(c)
}

/// 在整行文本里取点击列处的 token（显示列 0 基；宽字符按 1 列近似：
/// 点击命中的 token 多为 ASCII 路径，近似够用，非 token 字符处自然
/// None）。返回 (token, 起始显示列)。
pub fn line_token_at(line: &str, col: u32) -> Option<(String, u32)> {
    let chars: Vec<char> = line.chars().collect();
    let mut idx = None;
    for (col_now, (i, _c)) in (0_u32..).zip(chars.iter().enumerate()) {
        if col_now >= col {
            idx = Some(i);
            break;
        }
    }
    let idx = idx.unwrap_or(chars.len());
    let c = *chars.get(idx)?;
    if !is_token_char(c) {
        return None;
    }
    let mut lo = idx;
    while lo > 0 && is_token_char(chars[lo - 1]) {
        lo -= 1;
    }
    let mut hi = idx;
    while hi + 1 < chars.len() && is_token_char(chars[hi + 1]) {
        hi += 1;
    }
    let token: String = chars[lo..=hi].iter().collect();
    Some((token, lo as u32))
}

/// token 分类（宿主在广播前先认一遍，不对插件发纯单词噪声）：
/// URL scheme 开头（`scheme://`）→ `url`；否则路径样式（含 `/`、以
/// `~`/`.` 开头、或带短扩展名）→ `path`；都不是 → None。
pub fn classify_token(token: &str) -> Option<HitKind> {
    if token.len() < 2 {
        return None;
    }
    if let Some((scheme, rest)) = token.split_once("://")
        && scheme.len() >= 2
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
        && !rest.is_empty()
    {
        return Some(HitKind::Url);
    }
    let looks_path = token.contains('/')
        || token.starts_with('~')
        || token.starts_with("./")
        || token.starts_with("../")
        || (token.contains('.')
            && token
                .rsplit('.')
                .next()
                .is_some_and(|ext| !ext.is_empty() && ext.len() <= 8));
    looks_path.then_some(HitKind::Path)
}

/// OPEN_URL 载荷分类（适配器取舍）：
/// - `file://`：剥成文件系统路径，归 `path`（pager 只认领 path；
///   把 file URL 当 `url` 会落到 `/usr/bin/open`，预览永远不触发）；
/// - 带其它 `scheme://`：常见 scheme（http/https/mailto/ftp）→ `url`，
///   其余自定义 scheme → `osc8`；
/// - **无 scheme**：ghostty 的 URL 匹配器 + `resolvePathForOpening` 会把
///   路径 token 解析成（绝对）文件路径再送 OPEN_URL——这是 ⌘+click 路径
///   的主数据源，归 `path`。
pub fn classify_url(url: &str) -> HitKind {
    if file_url_to_fs_path(url).is_some() {
        return HitKind::Path;
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return HitKind::Path; // 无 scheme：ghostty 已解析的文件路径
    };
    let _ = rest;
    match scheme {
        "http" | "https" | "mailto" | "ftp" => HitKind::Url,
        _ => HitKind::Osc8,
    }
}

/// Ghostty OSC-7 / OPEN_URL 的 `file://` 载荷 → 文件系统路径。
///
/// 语义坑停在宿主：`terminal.getPwd()` 原样是 `file:///tmp`，插件
/// `PathBuf::from(cwd).join(rel)` 会拼出不存在的路径然后 ignore。
/// 不是 file URL → None（调用方当普通路径用）。
pub fn file_url_to_fs_path(s: &str) -> Option<String> {
    let rest = s.strip_prefix("file://")?;
    let path = if rest.starts_with('/') {
        rest
    } else {
        let slash = rest.find('/')?;
        // file://localhost/tmp 或 file://hostname/tmp
        &rest[slash..]
    };
    Some(percent_decode(path))
}

/// OSC-7 / 已是绝对路径 / 其它：尽量得到可 `join` 的 cwd。
pub fn normalize_cwd(raw: &str) -> String {
    if let Some(p) = file_url_to_fs_path(raw) {
        return p;
    }
    raw.to_string()
}

/// OPEN_URL / 网格 token → (kind, 给插件的 text)。file URL 剥成 path。
pub fn normalize_open_payload(kind: HitKind, text: &str) -> (HitKind, String) {
    if let Some(p) = file_url_to_fs_path(text) {
        return (HitKind::Path, p);
    }
    (kind, text.to_string())
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(
                std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
                out.push(v);
                i += 3;
                continue;
            }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 点击用的 cwd：规范化的 OSC-7 → 前台进程真实目录 → 空。
pub(crate) fn cwd_for_view(view: &SurfaceHostView) -> String {
    if let Some(raw) = view.ivars().pwd.borrow().as_deref() {
        let n = normalize_cwd(raw);
        if !n.is_empty() && (n.starts_with('/') || n.starts_with('~')) {
            return n;
        }
    }
    let Some(surface) = view.surface_opt() else {
        return String::new();
    };
    let pid = unsafe { ghostty_sys::ghostty_surface_foreground_pid(surface) };
    if pid == 0 {
        return String::new();
    }
    macos_pid_cwd(pid as u32).unwrap_or_default()
}

fn macos_pid_cwd(pid: u32) -> Option<String> {
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let sz = std::mem::size_of::<libc::proc_vnodepathinfo>() as i32;
    let got = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            std::ptr::from_mut(&mut info).cast(),
            sz,
        )
    };
    if got <= 0 {
        return None;
    }
    let bytes = unsafe {
        std::slice::from_raw_parts(info.pvi_cdir.vip_path.as_ptr().cast::<u8>(), 1024)
    };
    let end = bytes.iter().position(|&c| c == 0).unwrap_or(0);
    if end == 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

/// ghostty mods → 协议修饰键列表。
pub fn modifiers_from_mods(mods: ghostty_sys::ghostty_input_mods_e) -> Vec<Modifier> {
    let mut out = Vec::new();
    if mods & ghostty_sys::GHOSTTY_MODS_SHIFT != 0 {
        out.push(Modifier::Shift);
    }
    if mods & ghostty_sys::GHOSTTY_MODS_CTRL != 0 {
        out.push(Modifier::Ctrl);
    }
    if mods & ghostty_sys::GHOSTTY_MODS_ALT != 0 {
        out.push(Modifier::Alt);
    }
    if mods & ghostty_sys::GHOSTTY_MODS_SUPER != 0 {
        out.push(Modifier::Cmd);
    }
    out
}

// ---------------------------------------------------------------------------
// layer 几何（纯函数，单测钉行为）
// ---------------------------------------------------------------------------

/// overlay 层矩形（**points**，视图左上原点）：全宽，从锚点行往下至多
/// 半屏；锚点下方空间不足 1/4 屏时改向上开。宽高下限 64pt 防退化。
pub fn overlay_rect(
    anchor_row: u32,
    anchor_col: u32,
    cell_pt: (f64, f64),
    view_pt: (f64, f64),
) -> (f64, f64, f64, f64) {
    let (cw, ch) = cell_pt;
    let (vw, vh) = view_pt;
    let ax = (f64::from(anchor_col) * cw).clamp(0.0, (vw - 64.0).max(0.0));
    let ay = f64::from(anchor_row) * ch;
    let half = (vh * 0.5).floor().max(64.0);
    let quarter = (vh * 0.25).floor();
    // 锚点下方放得下 1/4 屏 → 往下开（至多半屏）；否则向上开。
    let (y, h) = if vh - ay >= quarter {
        (ay, half.min((vh - ay).max(64.0)))
    } else {
        ((ay - half).max(0.0), half.min(ay.max(64.0)))
    };
    let w = (vw - ax).max(64.0);
    (ax, y, w, h)
}

// ---------------------------------------------------------------------------
// theme.set 校验 → ghostty 配置层文本（纯函数）
// ---------------------------------------------------------------------------

/// 解析 `#rrggbb`（6 位十六进制；大小写均可）。其他形态（短写/0x/命名
/// 色）→ None（语义坏值，整条忽略）。
pub fn parse_hex_color(s: &str) -> Option<(u8, u8, u8)> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some(((v >> 16) as u8, ((v >> 8) & 0xff) as u8, (v & 0xff) as u8))
}

fn hex(c: (u8, u8, u8)) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

/// theme.set → 覆盖层文件文本（None = 色板语义非法，整条忽略）。
///
/// - `selection_alpha` 无 ghostty 对应键：按不透明度把选区色合成到
///   背景上（`selection-background` 是 RGB）。
/// - `divider` 无 ghostty 对应键（宿主分隔条色由 background 派生）：
///   留在协议里，本层不落地——文档化取舍。
pub fn theme_conf_text(m: &ThemeSet) -> Option<String> {
    let bg = parse_hex_color(&m.bg)?;
    let fg = parse_hex_color(&m.fg)?;
    let cursor = parse_hex_color(&m.cursor)?;
    let sel = parse_hex_color(&m.selection_bg)?;
    if m.selection_alpha > 255 {
        return None;
    }
    let mut ansi = [(0u8, 0u8, 0u8); 16];
    for (i, c) in m.ansi.iter().enumerate() {
        ansi[i] = parse_hex_color(c)?;
    }
    // 选区合成：sel*alpha + bg*(1-alpha)。
    let a = f64::from(m.selection_alpha) / 255.0;
    let blend = |s: u8, b: u8| ((f64::from(s) * a + f64::from(b) * (1.0 - a)).round()) as u8;
    let selection = (blend(sel.0, bg.0), blend(sel.1, bg.1), blend(sel.2, bg.2));
    let mut s = String::from("# ninja plugin theme layer (generated; theme.set)\n");
    s.push_str(&format!("background = {}\n", hex(bg)));
    s.push_str(&format!("foreground = {}\n", hex(fg)));
    s.push_str(&format!("cursor-color = {}\n", hex(cursor)));
    s.push_str(&format!("selection-background = {}\n", hex(selection)));
    for (i, c) in ansi.iter().enumerate() {
        s.push_str(&format!("palette = {}={}\n", i, hex(*c)));
    }
    Some(s)
}

// ---------------------------------------------------------------------------
// 键名 ↔ macOS 虚拟键码（input 适配器）
// ---------------------------------------------------------------------------

/// 命名键表（协议命名集）：name ↔ kVK 码。
const NAMED_KEYS: &[(&str, u16)] = &[
    ("left", 123),
    ("right", 124),
    ("down", 125),
    ("up", 126),
    ("home", 115),
    ("end", 119),
    ("pageup", 116),
    ("pagedown", 121),
    ("delete", 117),
    ("backspace", 51),
    ("tab", 48),
    ("enter", 36),
    ("esc", 53),
    ("f1", 0x7A),
    ("f2", 0x78),
    ("f3", 0x63),
    ("f4", 0x76),
    ("f5", 0x60),
    ("f6", 0x61),
    ("f7", 0x62),
    ("f8", 0x64),
    ("f9", 0x65),
    ("f10", 0x6D),
    ("f11", 0x67),
    ("f12", 0x6F),
];

/// 协议 key 字符串 → macOS 虚拟键码（单字符按 ASCII 布局）。
pub fn key_name_to_code(key: &str) -> Option<u16> {
    for (name, code) in NAMED_KEYS {
        if *name == key {
            return Some(*code);
        }
    }
    let mut it = key.chars();
    let (c, single) = (it.next()?, it.next().is_none());
    if !single || !c.is_ascii_alphanumeric() {
        return None;
    }
    let lower = c.to_ascii_lowercase();
    const LOWER: &[u16] = &[
        0x00, 0x0B, 0x08, 0x02, 0x0E, 0x03, 0x05, 0x04, 0x22, 0x26, // a-j
        0x28, 0x25, 0x2E, 0x2D, 0x1F, 0x23, 0x0C, 0x0F, 0x01, 0x11, // k-t
        0x20, 0x09, 0x0D, 0x07, 0x10, 0x06, // u-z
    ];
    if lower.is_ascii_lowercase() {
        return Some(LOWER[(lower as u8 - b'a') as usize]);
    }
    const DIGITS: &[u16] = &[0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1A, 0x1C, 0x19, 0x1D];
    Some(DIGITS[(lower as u8 - b'0') as usize])
}

/// 协议 key + modifiers → `ghostty_input_key_s`（供 `config_key_is_binding`
/// 冲突检查；keycode = macOS 原生虚拟键码，嵌入 API 惯例）。
pub(crate) fn hotkey_to_key_event(key: &str, mods: &[Modifier]) -> Option<ghostty_sys::ghostty_input_key_s> {
    let code = key_name_to_code(key)?;
    let mut m: u32 = ghostty_sys::GHOSTTY_MODS_NONE;
    for md in mods {
        m |= match md {
            Modifier::Shift => ghostty_sys::GHOSTTY_MODS_SHIFT,
            Modifier::Ctrl => ghostty_sys::GHOSTTY_MODS_CTRL,
            Modifier::Alt => ghostty_sys::GHOSTTY_MODS_ALT,
            Modifier::Cmd => ghostty_sys::GHOSTTY_MODS_SUPER,
        };
    }
    Some(ghostty_sys::ghostty_input_key_s {
        action: ghostty_sys::GHOSTTY_ACTION_PRESS,
        mods: m,
        consumed_mods: ghostty_sys::GHOSTTY_MODS_NONE,
        keycode: u32::from(code),
        text: std::ptr::null(),
        unshifted_codepoint: 0,
        composing: false,
    })
}

/// macOS 虚拟键码 → 协议 key 字符串（命名键优先，退回单字符文本）。
pub fn code_to_key_name(code: u16, fallback_char: Option<char>) -> String {
    for (name, c) in NAMED_KEYS {
        if *c == code {
            return (*name).to_string();
        }
    }
    if let Some(c) = fallback_char
        && c.is_ascii_graphic()
        && !c.is_whitespace()
    {
        return c.to_ascii_lowercase().to_string();
    }
    format!("key{code}")
}
