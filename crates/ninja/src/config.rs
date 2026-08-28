//! p2 配置：`~/.config/ninja/ninja.toml`（`NINJA_CONFIG` 可覆盖路径）。
//!
//! 文件缺失 → 内置默认值照常启动（p2 门禁「缺省文件可启动」）；
//! 文件损坏/字段非法 → stderr 警告 + 对应默认值，不拒绝启动。
//! schema：shell、font-family/font-size、theme（selection-bg/cursor）、
//! keys（动作名 → "cmd+shift+d" 风格绑定）。
//! 只在启动时解析一次，不进空载常驻路径。

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::term::Rgb;

// ---------------------------------------------------------------------------
// 键位
// ---------------------------------------------------------------------------

/// 一个菜单/宿主动作的键绑定（NSMenuItem keyEquivalent + 修饰掩码）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyBinding {
    /// keyEquivalent 字符（箭头等用功能键字符 F700-F703）。
    pub key: String,
    pub cmd: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// NSEventModifierFlags 位（keymap.rs 的 flags 换算同源）。
pub const MASK_SHIFT: u64 = 0x0002_0000;
pub const MASK_CTRL: u64 = 0x0004_0000;
pub const MASK_ALT: u64 = 0x0008_0000;
pub const MASK_CMD: u64 = 0x0010_0000;

impl KeyBinding {
    pub fn flags(&self) -> u64 {
        let mut f = 0u64;
        if self.shift {
            f |= MASK_SHIFT;
        }
        if self.ctrl {
            f |= MASK_CTRL;
        }
        if self.alt {
            f |= MASK_ALT;
        }
        if self.cmd {
            f |= MASK_CMD;
        }
        f
    }
}

/// 解析 `"cmd+shift+d"` / `"cmd+alt+left"` 风格绑定。
/// 修饰段：cmd/command、ctrl/control、alt/option/opt、shift（大小写不敏感）。
/// 键段：left/right/up/down（功能键字符）或单个字符。失败返回 None。
pub fn parse_binding(s: &str) -> Option<KeyBinding> {
    let mut b = KeyBinding {
        key: String::new(),
        cmd: false,
        ctrl: false,
        alt: false,
        shift: false,
    };
    let mut key_seen = false;
    for part in s.split('+') {
        if key_seen {
            return None; // 键段必须是最后一段且只出现一次
        }
        match part.to_ascii_lowercase().as_str() {
            "cmd" | "command" | "super" => b.cmd = true,
            "ctrl" | "control" => b.ctrl = true,
            "alt" | "option" | "opt" => b.alt = true,
            "shift" => b.shift = true,
            _ => {
                let key = match part.to_ascii_lowercase().as_str() {
                    "left" => "\u{F702}",
                    "right" => "\u{F703}",
                    "up" => "\u{F700}",
                    "down" => "\u{F701}",
                    _ => part,
                };
                if key.chars().count() != 1 {
                    return None;
                }
                b.key = key.to_string();
                key_seen = true;
            }
        }
    }
    if !key_seen || b.key.is_empty() {
        return None;
    }
    Some(b)
}

// ---------------------------------------------------------------------------
// 动作表（菜单/快捷键的名字 → 默认绑定；菜单标题与 selector 在 app.rs）
// ---------------------------------------------------------------------------

/// 可在 `[keys]` 里重绑的动作。顺序即默认表顺序。
pub const ACTION_NAMES: &[(&str, &str)] = &[
    ("new_window", "cmd+n"),
    ("new_tab", "cmd+t"),
    ("close", "cmd+w"),
    ("split_right", "cmd+d"),
    ("split_down", "cmd+shift+d"),
    ("close_pane", "cmd+shift+w"),
    ("focus_left", "cmd+alt+left"),
    ("focus_right", "cmd+alt+right"),
    ("focus_up", "cmd+alt+up"),
    ("focus_down", "cmd+alt+down"),
    ("prev_pane", "cmd+["),
    ("next_pane", "cmd+]"),
    ("copy", "cmd+c"),
    ("paste", "cmd+v"),
    ("select_all", "cmd+a"),
    ("quit", "cmd+q"),
];

pub fn default_keys() -> HashMap<String, KeyBinding> {
    ACTION_NAMES
        .iter()
        .filter_map(|(name, def)| {
            parse_binding(def).map(|b| (name.to_string(), b))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 配置本体
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// None = `$SHELL`（再缺省 `/bin/bash`，见 pty.rs）。
    pub shell: Option<String>,
    /// None = 内置默认等宽（Menlo）。
    pub font_family: Option<String>,
    pub font_size_pt: f64,
    /// 主题色（p1 硬编码进 renderer 的两个值）。
    pub selection_bg: Rgb,
    pub cursor: Rgb,
    /// 动作名 → 键绑定（含全部默认，缺项由默认表补齐）。
    pub keys: HashMap<String, KeyBinding>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shell: None,
            font_family: None,
            font_size_pt: 13.0,
            selection_bg: Rgb(0x35, 0x4B, 0x8C),
            cursor: Rgb(0xE6, 0xE6, 0xE6),
            keys: default_keys(),
        }
    }
}

/// `"#354B8C"` / `"0x354B8C"` / `"#ABC"` → Rgb。
fn parse_rgb(s: &str) -> Option<Rgb> {
    let s = s.trim();
    let hex = s.strip_prefix('#').or_else(|| s.strip_prefix("0x"))?;
    if hex.len() == 3 {
        let d: [u8; 3] = [
            u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?,
            u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?,
            u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?,
        ];
        return Some(Rgb(d[0], d[1], d[2]));
    }
    if hex.len() != 6 {
        return None;
    }
    Some(Rgb(
        u8::from_str_radix(&hex[0..2], 16).ok()?,
        u8::from_str_radix(&hex[2..4], 16).ok()?,
        u8::from_str_radix(&hex[4..6], 16).ok()?,
    ))
}

// ---------------------------------------------------------------------------
// TOML 反序列化（serde 镜像，全部 Option，缺失 = 默认）
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct ThemeToml {
    #[serde(alias = "selection-bg")]
    selection_bg: Option<String>,
    cursor: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct FileToml {
    shell: Option<String>,
    /// 字段名两种写法都收：`font-family`（TOML 惯例）/ `font_family`。
    #[serde(alias = "font-family")]
    font_family: Option<String>,
    #[serde(alias = "font-size")]
    font_size: Option<f64>,
    theme: ThemeToml,
    keys: HashMap<String, String>,
}

impl Config {
    /// 解析 TOML 文本（文件读入后走这里；测试也直接走这里）。
    /// 任何字段级错误都降级为默认值 + stderr 警告，不失败。
    pub fn from_toml_str(text: &str) -> Self {
        let mut cfg = Config::default();
        let parsed: FileToml = match toml::from_str(text) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("ninja: 配置解析失败，使用默认值: {e}");
                return cfg;
            }
        };
        if let Some(shell) = parsed.shell {
            if shell.trim().is_empty() {
                cfg.shell = None;
            } else {
                cfg.shell = Some(shell);
            }
        }
        if let Some(family) = parsed.font_family {
            if family.trim().is_empty() {
                cfg.font_family = None;
            } else {
                cfg.font_family = Some(family);
            }
        }
        if let Some(size) = parsed.font_size {
            if size.is_finite() && (4.0..=200.0).contains(&size) {
                cfg.font_size_pt = size;
            } else {
                eprintln!("ninja: font-size {size} 越界（4.0–200.0），用默认 13.0");
            }
        }
        if let Some(s) = parsed.theme.selection_bg {
            match parse_rgb(&s) {
                Some(c) => cfg.selection_bg = c,
                None => eprintln!("ninja: theme.selection_bg {s:?} 解析失败，用默认值"),
            }
        }
        if let Some(s) = parsed.theme.cursor {
            match parse_rgb(&s) {
                Some(c) => cfg.cursor = c,
                None => eprintln!("ninja: theme.cursor {s:?} 解析失败，用默认值"),
            }
        }
        let known: Vec<&str> = ACTION_NAMES.iter().map(|(n, _)| *n).collect();
        for (name, binding) in parsed.keys {
            if !known.contains(&name.as_str()) {
                eprintln!("ninja: keys.{name} 不是可绑定动作，忽略（可用: {known:?}）");
                continue;
            }
            match parse_binding(&binding) {
                Some(b) => {
                    cfg.keys.insert(name, b);
                }
                None => eprintln!("ninja: keys.{name} = {binding:?} 解析失败，保留默认"),
            }
        }
        cfg
    }

    /// 读配置文件：`NINJA_CONFIG` 或 `~/.config/ninja/ninja.toml`。
    /// 缺失/读失败 → 默认值（静默——缺省文件是合法状态）。
    pub fn load() -> Self {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::from_toml_str(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => {
                eprintln!("ninja: 读配置 {path:?} 失败（{e}），使用默认值");
                Config::default()
            }
        }
    }
}

pub fn config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("NINJA_CONFIG") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".config/ninja/ninja.toml")
}

/// 默认配置的 TOML 文本（文档/测试基线）。
pub fn default_toml() -> String {
    let mut s = String::new();
    s.push_str("# ninja 默认配置（删掉本文件即恢复内置默认值）\n");
    s.push_str("# shell = \"/bin/zsh\"   # 缺省 = $SHELL，再缺省 /bin/bash\n");
    s.push_str("# font-family = \"Menlo\"\n");
    s.push_str("# font-size = 13.0\n\n");
    s.push_str("[theme]\n");
    s.push_str("# selection-bg = \"#354B8C\"\n");
    s.push_str("# cursor = \"#E6E6E6\"\n\n");
    s.push_str("[keys]\n");
    s.push_str("# new_window = \"cmd+n\"\n");
    s.push_str("# split_right = \"cmd+d\"\n\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_binding_forms() {
        let b = parse_binding("cmd+shift+d").unwrap();
        assert_eq!(b.key, "d");
        assert!(b.cmd && b.shift && !b.alt && !b.ctrl);
        assert_eq!(b.flags(), MASK_CMD | MASK_SHIFT);

        let arrow = parse_binding("cmd+alt+left").unwrap();
        assert_eq!(arrow.key, "\u{F702}");
        assert!(arrow.cmd && arrow.alt);

        // 修饰别名与大小写。
        assert!(parse_binding("Command+Control+C").unwrap().cmd);
        assert!(parse_binding("option+x").unwrap().alt);

        // 无键段 / 多键段 / 多字符键 / 空段 → None。
        assert!(parse_binding("cmd").is_none());
        assert!(parse_binding("cmd+a+b").is_none());
        assert!(parse_binding("cmd+ab").is_none());
        assert!(parse_binding("cmd++a").is_none());
    }

    #[test]
    fn rgb_forms() {
        assert_eq!(parse_rgb("#354B8C"), Some(Rgb(0x35, 0x4B, 0x8C)));
        assert_eq!(parse_rgb("0x354B8C"), Some(Rgb(0x35, 0x4B, 0x8C)));
        assert_eq!(parse_rgb("#ABC"), Some(Rgb(0xAA, 0xBB, 0xCC)));
        assert_eq!(parse_rgb("#XYZ"), None);
        assert_eq!(parse_rgb("blue"), None);
        assert_eq!(parse_rgb("#12345"), None);
    }

    #[test]
    fn missing_and_broken_files_use_defaults() {
        let c = Config::from_toml_str("");
        assert_eq!(c, Config::default());

        let c = Config::from_toml_str("this is [ not toml");
        assert_eq!(c, Config::default());

        // 未知顶层字段：deny_unknown_fields → 整体降级默认（不启动失败）。
        let c = Config::from_toml_str("wat = 1");
        assert_eq!(c, Config::default());
    }

    #[test]
    fn full_config_parses() {
        let text = r##"
shell = "/bin/zsh"
font-family = "JetBrains Mono"
font-size = 14.5

[theme]
selection-bg = "#101010"
cursor = "0xFF0000"

[keys]
split_right = "ctrl+cmd+d"
new_tab = "cmd+t"
bogus_action = "cmd+x"
focus_down = "not a key"
"##;
        let c = Config::from_toml_str(text);
        assert_eq!(c.shell.as_deref(), Some("/bin/zsh"));
        assert_eq!(c.font_family.as_deref(), Some("JetBrains Mono"));
        assert_eq!(c.font_size_pt, 14.5);
        assert_eq!(c.selection_bg, Rgb(0x10, 0x10, 0x10));
        assert_eq!(c.cursor, Rgb(0xFF, 0x00, 0x00));
        assert_eq!(c.keys[&"split_right".to_string()].flags(), MASK_CTRL | MASK_CMD);
        // 未覆盖的动作保留默认。
        assert_eq!(
            c.keys[&"split_down".to_string()],
            parse_binding("cmd+shift+d").unwrap()
        );
        // 未知动作/坏绑定不进表（默认仍在）。
        assert!(!c.keys.contains_key("bogus_action"));
        assert_eq!(
            c.keys[&"focus_down".to_string()],
            parse_binding("cmd+alt+down").unwrap()
        );
    }

    #[test]
    fn default_keys_cover_all_actions() {
        let c = Config::default();
        for (name, _) in ACTION_NAMES {
            assert!(c.keys.contains_key(*name), "missing default binding {name}");
        }
    }

    #[test]
    fn default_toml_is_valid_and_noop() {
        // 文档里给的默认片段：解析后应与内置默认一致（全注释 = 无改动）。
        let c = Config::from_toml_str(&default_toml());
        assert_eq!(c, Config::default());
    }
}
