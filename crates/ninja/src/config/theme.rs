//! 宿主层文本与用户 `theme=` 判定。

use std::path::PathBuf;

/// Ghostty Config.zig 默认 `background`（`#282c34`）。chrome 兑底用。
pub const GHOSTTY_DEFAULT_BACKGROUND: (u8, u8, u8) = (0x28, 0x2C, 0x34);

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

/// 该行是否设置 `theme =`（行级近似——`$ if` 条件块里的 theme= 也算）。
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

/// 用户是否在 ghostty 配置（默认文件 + config-file 链）里设置了 `theme =`。
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
