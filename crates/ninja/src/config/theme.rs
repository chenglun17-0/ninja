//! ODP 缺省色板与层文件文本（宿主层 / ODP 层 / 用户 theme 判定）。

use std::path::PathBuf;


// ---------------------------------------------------------------------------
// ODP 钉值（One Dark Pro 官方主题源，注明来源键）
// ---------------------------------------------------------------------------

/// editor.background：`#282c34`
pub const ODP_BACKGROUND: (u8, u8, u8) = (0x28, 0x2C, 0x34);
/// editor.foreground：`#abb2bf`
pub const ODP_FOREGROUND: (u8, u8, u8) = (0xAB, 0xB2, 0xBF);
/// editorCursor.foreground：`#528bff`
pub const ODP_CURSOR: (u8, u8, u8) = (0x52, 0x8B, 0xFF);
/// terminal.selectionBackground `#abb2bf30`（带 alpha）盖在 ODP bg 上的
/// 不透明合成色 ≈ `#41454E`（ghostty selection-background 是 RGB，无 alpha）。
pub const ODP_SELECTION_BG: (u8, u8, u8) = (0x41, 0x45, 0x4E);
/// ANSI 16 色（terminal.ansi*，One Dark Pro 官方主题源）。
pub const ODP_ANSI: [(u8, u8, u8); 16] = [
    (0x3F, 0x44, 0x51), // terminal.ansiBlack         #3f4451
    (0xE0, 0x55, 0x61), // terminal.ansiRed           #e05561
    (0x8C, 0xC2, 0x65), // terminal.ansiGreen         #8cc265
    (0xD1, 0x8F, 0x52), // terminal.ansiYellow        #d18f52
    (0x4A, 0xA5, 0xF0), // terminal.ansiBlue          #4aa5f0
    (0xC1, 0x62, 0xDE), // terminal.ansiMagenta       #c162de
    (0x42, 0xB3, 0xC2), // terminal.ansiCyan          #42b3c2
    (0xD7, 0xDA, 0xE0), // terminal.ansiWhite         #d7dae0
    (0x4F, 0x56, 0x66), // terminal.ansiBrightBlack   #4f5666
    (0xFF, 0x61, 0x6E), // terminal.ansiBrightRed     #ff616e
    (0xA5, 0xE0, 0x75), // terminal.ansiBrightGreen   #a5e075
    (0xF0, 0xA4, 0x5D), // terminal.ansiBrightYellow  #f0a45d
    (0x4D, 0xC4, 0xFF), // terminal.ansiBrightBlue    #4dc4ff
    (0xDE, 0x73, 0xFF), // terminal.ansiBrightMagenta #de73ff
    (0x4C, 0xD1, 0xE0), // terminal.ansiBrightCyan    #4cd1e0
    (0xE6, 0xE6, 0xE6), // terminal.ansiBrightWhite   #e6e6e6
];

fn hex(c: (u8, u8, u8)) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

/// ODP 缺省层的 ghostty 配置文本（纯函数，单测直测）。
pub fn odp_layer_text() -> String {
    let mut s = String::from("# ninja ODP default theme layer (generated)\n");
    s.push_str("# One Dark Pro 钉值（官方主题源）。装载在用户默认文件之前：\n");
    s.push_str("# 用户显式色键/theme= 覆盖这里。\n");
    s.push_str(&format!("background = {}\n", hex(ODP_BACKGROUND)));
    s.push_str(&format!("foreground = {}\n", hex(ODP_FOREGROUND)));
    s.push_str(&format!("cursor-color = {}\n", hex(ODP_CURSOR)));
    s.push_str(&format!("selection-background = {}\n", hex(ODP_SELECTION_BG)));
    for (i, c) in ODP_ANSI.iter().enumerate() {
        s.push_str(&format!("palette = {}={}\n", i, hex(*c)));
    }
    s
}

/// 宿主层文本：ninja 特有动作（插件面板）认领 ghostty 空闲动作
/// `toggle_visibility` 绑 ⌘,（替换 ghostty 默认的 ⌘,=open_config——
/// 同一 trigger（super+unicode ','）后载覆盖）。用户可
/// `keybind = super+shift+p=toggle_visibility` 统一重绑。
pub fn host_layer_text() -> String {
    let mut s = String::from("# ninja host layer (generated)\n");
    s.push_str("# 宿主动作经 ghostty keybind 系统统一重绑：插件面板认领空闲动作\n");
    s.push_str("# toggle_visibility；ghostty 动作集封闭，自定义动作名不可用\n");
    s.push_str("#（Binding.zig InvalidAction）。\n");
    s.push_str("keybind = super+,=toggle_visibility\n");
    s
}

/// 该行是否设置 `theme =`（ODP 层跳过判定；行级近似——`$ if` 条件块里
/// 的 theme= 也算设置，宁缺 ODP 不压用户主题）。
fn line_sets_theme(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with('#') {
        return false;
    }
    let Some(rest) = t.strip_prefix("theme") else {
        return false;
    };
    rest.trim_start().starts_with('=')
}

/// 用户是否在 ghostty 配置（默认文件 + config-file 链）里设置了
/// `theme =`。设置了 → finalize 的 loadTheme 会压顶，ODP 层必须让位
/// （否则 ODP 的显式色键反压用户主题）。
pub fn user_sets_theme(files: &[PathBuf]) -> bool {
    files.iter().any(|p| {
        std::fs::read_to_string(p)
            .map(|t| t.lines().any(line_sets_theme))
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_line_detection() {
        assert!(line_sets_theme("theme=Dracula"));
        assert!(line_sets_theme("theme = Dracula"));
        assert!(line_sets_theme("  theme=dark:Dracula,light:Foo"));
        assert!(!line_sets_theme("# theme=commented"));
        assert!(!line_sets_theme("themes = dir"));
        assert!(!line_sets_theme("font-size=18"));
        assert!(!line_sets_theme("window-theme=dark"));
    }
}
