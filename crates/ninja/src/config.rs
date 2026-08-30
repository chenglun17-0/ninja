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

use crate::plugins::PluginsConfig;
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
                    // X3：Return/Enter 主键 = NSCarriageReturnCharacter
                    //（NSMenuItem keyEquivalent 惯例，⌘⇧Enter 放大 pane）。
                    "enter" | "return" => "\r",
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

/// 可在 `[keys]` 里重绑的动作。顺序即默认表顺序。`plugins`（面板）
/// 默认 ⌘,：App 菜单区「Plugins…」项（2026-08-29 用户产品决策：
/// 启用即拉起 + 可见的设置面）。
pub const ACTION_NAMES: &[(&str, &str)] = &[
    ("new_window", "cmd+n"),
    ("new_tab", "cmd+t"),
    ("close", "cmd+w"),
    ("split_right", "cmd+d"),
    ("split_down", "cmd+shift+d"),
    ("close_pane", "cmd+shift+w"),
    // X3 ⌘⇧Enter：放大焦点 pane 临时占满窗口，再按还原（Ghostty
    // toggle_split_zoom 语义）；无分屏时等价窗口 zoom（最大化非全屏）。
    ("toggle_zoom", "cmd+shift+enter"),
    ("focus_left", "cmd+alt+left"),
    ("focus_right", "cmd+alt+right"),
    ("focus_up", "cmd+alt+up"),
    ("focus_down", "cmd+alt+down"),
    ("prev_pane", "cmd+["),
    ("next_pane", "cmd+]"),
    ("copy", "cmd+c"),
    ("paste", "cmd+v"),
    ("select_all", "cmd+a"),
    ("plugins", "cmd+,"),
    ("quit", "cmd+q"),
];

pub fn default_keys() -> HashMap<String, KeyBinding> {
    ACTION_NAMES
        .iter()
        .filter_map(|(name, def)| parse_binding(def).map(|b| (name.to_string(), b)))
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
    /// 主题色（T-主题：One Dark Pro 官方值钉在 [`crate::theme`]；这两个
    /// 是 p2 既有的字段级覆盖入口）。None = 未覆盖（跟随当前生效色板
    /// ——内置 ODP 基线或 T2 插件覆盖）。
    pub selection_bg: Option<Rgb>,
    pub cursor: Option<Rgb>,
    /// 动作名 → 键绑定（含全部默认，缺项由默认表补齐）。
    pub keys: HashMap<String, KeyBinding>,
    /// p3：插件开关。默认空 = 关（空载不建 socket、不拉进程）。
    pub plugins: PluginsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shell: None,
            font_family: None,
            font_size_pt: 13.0,
            selection_bg: None,
            cursor: None,
            keys: default_keys(),
            plugins: PluginsConfig::default(),
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
struct PluginsToml {
    /// 启用的插件名。缺省/空 = 插件全关（空载门禁）。启用即拉起
    /// （宿主启动/面板 on；2026-08-29 决策修订，无 spawn 模式段）。
    enabled: Option<Vec<String>>,
    /// 插件名 → 二进制路径（拉起用）。缺省时按名字在
    /// NINJA_PLUGIN_DIR / ~/.config/ninja/plugins / 宿主二进制同目录解析。
    paths: Option<HashMap<String, String>>,
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
    plugins: PluginsToml,
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
                Some(c) => cfg.selection_bg = Some(c),
                None => eprintln!("ninja: theme.selection_bg {s:?} 解析失败，用默认值"),
            }
        }
        if let Some(s) = parsed.theme.cursor {
            match parse_rgb(&s) {
                Some(c) => cfg.cursor = Some(c),
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
        // p3：[plugins] enabled；空/缺省 = 关。名字去空白去重，
        // 未知名字先收下（宿主在启动时找不到对应插件只警告，拉起在 p5）。
        if let Some(enabled) = parsed.plugins.enabled {
            let mut seen = std::collections::BTreeSet::new();
            let names: Vec<String> = enabled
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| {
                    if s.is_empty() {
                        return false;
                    }
                    seen.insert(s.clone())
                })
                .collect();
            if !names.is_empty() {
                eprintln!("ninja: 插件已启用 {names:?}（宿主启动即拉起）");
            }
            cfg.plugins = PluginsConfig {
                enabled: names,
                paths: parsed
                    .plugins
                    .paths
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|(k, v)| {
                        let v = v.trim().to_string();
                        if v.is_empty() {
                            eprintln!("ninja: plugins.paths.{k} 为空，忽略");
                            None
                        } else {
                            Some((k, v))
                        }
                    })
                    .collect(),
            };
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

// ---------------------------------------------------------------------------
// 面板写回（2026-08-29 决策）：只改 [plugins] enabled 数组，其余字节
// （含注释/字段/顺序）不动——不用 serde 重序列化（那会抹掉注释）。
// ---------------------------------------------------------------------------

/// 名单 → TOML 数组字面量（名字里的引号删除：字符串注入防御）。
fn render_enabled_array(enabled: &[String]) -> String {
    format!(
        "[{}]",
        enabled
            .iter()
            .map(|n| format!("\"{}\"", n.replace('\"', "")))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// 在配置文本里重写 `[plugins]` 的 `enabled` 数组（纯函数，单测直测）。
/// 保留其它一切字节：
/// - 有 `enabled = [...]` 行：只替换 `[` 到 `]` 之间的内容（缩进/行内尾注释保留）；
/// - 有 `[plugins]` 节但无 enabled 行：紧跟节头插入一行；
/// - 无 `[plugins]` 节：文件末尾追加节；
/// - `[plugins.paths]` 等子节不算节头（只有裸 `[plugins]` 算）。
pub fn rewrite_plugins_enabled(text: &str, enabled: &[String]) -> String {
    let array = render_enabled_array(enabled);
    // split_inclusive：每片含结尾换行，重拼零丢失。
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    // 裸节头判定：`[plugins]` 或 `[plugins] # 注释`；`[plugins.x]` 子节不算。
    let is_bare_header = |l: &str| {
        let t = l.trim();
        (t == "[plugins]" || t.starts_with("[plugins]")) && !t.starts_with("[plugins.")
    };
    let Some(header_idx) = lines.iter().position(|l| is_bare_header(l)) else {
        // 无节：末尾追加（保留原文件末尾换行形状）。
        let mut out = text.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("\n[plugins]\nenabled = {array}\n"));
        return out;
    };
    // 节内找 enabled 行（到下一个任何节头为止）。
    let mut end = lines.len();
    for (i, l) in lines.iter().enumerate().skip(header_idx + 1) {
        if l.trim_start().starts_with('[') {
            end = i;
            break;
        }
    }
    let mut replace: Option<(usize, String)> = None; // (行下标, 新行内容)
    let mut insert_after: Option<usize> = None;
    for (i, l) in lines.iter().enumerate().take(end).skip(header_idx + 1) {
        let t = l.trim_start();
        if let Some(rest) = t.strip_prefix("enabled")
            && rest.trim_start().starts_with('=')
        {
            let eq = l.find('=').unwrap();
            match l[eq..].find('[').map(|p| eq + p) {
                Some(open) => {
                    if let Some(close) = l[open..].find(']').map(|p| open + p) {
                        // 只换数组字面量：缩进、`enabled =`、行尾注释都保留。
                        replace = Some((
                            i,
                            format!("{}{array}{}", &l[..open], &l[close + 1..]),
                        ));
                    } else {
                        // 数组跨行（罕见形态）：整行重写为渲染结果。
                        replace = Some((i, format!("enabled = {array}\n")));
                    }
                }
                None => {
                    replace = Some((i, format!("enabled = {array}\n")));
                }
            }
            break;
        }
    }
    if replace.is_none() {
        insert_after = Some(header_idx);
    }
    let mut out = String::with_capacity(text.len() + array.len() + 16);
    for (i, l) in lines.iter().enumerate() {
        if let Some((idx, newline)) = replace.as_ref()
            && *idx == i
        {
            out.push_str(newline);
        } else {
            out.push_str(l);
        }
        if insert_after == Some(i) {
            out.push_str(&format!("enabled = {array}\n"));
        }
    }
    out
}

/// 把新的 enabled 名单写回配置文件（面板开关语义的落盘半边）。
/// 文件不存在 = 用最小节起一份。失败 → false（调用方警告；会话内
/// 状态已生效，落盘失败不让开关回弹）。
pub fn save_plugins_enabled(enabled: &[String]) -> bool {
    let path = config_path();
    let base = std::fs::read_to_string(&path).unwrap_or_default();
    let out = rewrite_plugins_enabled(&base, enabled);
    match std::fs::write(&path, out) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("ninja: 写回配置 {path:?} 失败（{e}）：会话内已生效，重启后不保留");
            false
        }
    }
}

/// 默认配置的 TOML 文本（文档/测试基线）。
pub fn default_toml() -> String {
    let mut s = String::new();
    s.push_str("# ninja 默认配置（删掉本文件即恢复内置默认值）\n");
    s.push_str("# shell = \"/bin/zsh\"   # 缺省 = $SHELL，再缺省 /bin/bash\n");
    s.push_str("# font-family = \"Menlo\"\n");
    s.push_str("# font-size = 13.0\n\n");
    s.push_str("[theme]\n");
    s.push_str("# 默认 = One Dark Pro（官方色板钉死在代码，见 theme.rs）；\n");
    s.push_str("# 换主题不是配置面的事：装插件（如官方 ninja-theme）经协议 theme.set 换全色板\n");
    s.push_str("# selection-bg = \"#ABB2BF\"   # 官方 #abb2bf30 带alpha，字段级覆盖只换 RGB 不换alpha\n");
    s.push_str("# cursor = \"#528BFF\"\n\n");
    s.push_str("[keys]\n");
    s.push_str("# new_window = \"cmd+n\"\n");
    s.push_str("# split_right = \"cmd+d\"\n");
    s.push_str("# toggle_zoom = \"cmd+shift+enter\"   # ⌘⇧Enter：放大/还原焦点 pane；无分屏 = 窗口 zoom\n\n");
    s.push_str("[plugins]\n");
    s.push_str("# enabled = [\"preview\"]   # 默认空 = 插件关：不建 ADE socket、不拉进程；\n");
    s.push_str("#                              非空 = 启用即拉起（2026-08-29 决策；面板 ⌘, 开关会写回本行）\n");
    s.push_str("# [plugins.paths]\n");
    s.push_str("# preview = \"/usr/local/bin/ninja-preview\"   # 缺省按名在 NINJA_PLUGIN_DIR / ~/.config/ninja/plugins / 宿主同目录找\n");
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

        // X3：enter/return 键名 → "\r"（Return 键的 keyEquivalent 字符）。
        let z = parse_binding("cmd+shift+enter").unwrap();
        assert_eq!(z.key, "\r");
        assert!(z.cmd && z.shift && !z.ctrl && !z.alt);
        assert_eq!(z.flags(), MASK_CMD | MASK_SHIFT);
        assert_eq!(parse_binding("cmd+return").unwrap().key, "\r");

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
        assert_eq!(c.selection_bg, Some(Rgb(0x10, 0x10, 0x10)));
        assert_eq!(c.cursor, Some(Rgb(0xFF, 0x00, 0x00)));
        assert_eq!(
            c.keys[&"split_right".to_string()].flags(),
            MASK_CTRL | MASK_CMD
        );
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
        // X3：toggle_zoom 默认 ⌘⇧Enter 且可重绑。
        assert_eq!(
            c.keys[&"toggle_zoom".to_string()],
            parse_binding("cmd+shift+enter").unwrap()
        );
        let c = Config::from_toml_str("[keys]\ntoggle_zoom = \"ctrl+alt+z\"\n");
        assert_eq!(
            c.keys[&"toggle_zoom".to_string()],
            parse_binding("ctrl+alt+z").unwrap()
        );
    }

    #[test]
    fn default_toml_is_valid_and_noop() {
        // 文档里给的默认片段：解析后应与内置默认一致（全注释 = 无改动）。
        let c = Config::from_toml_str(&default_toml());
        assert_eq!(c, Config::default());
    }

    // ------------------------------------------------------------------
    // p3 [plugins]
    // ------------------------------------------------------------------

    #[test]
    fn plugins_default_off() {
        // 空载门禁：缺 [plugins] / 空 enabled / 空串 / 全空白 → 关。
        assert!(Config::default().plugins.enabled.is_empty());
        assert!(Config::from_toml_str("").plugins.enabled.is_empty());
        assert!(
            Config::from_toml_str(
                r#"[plugins]
enabled = []"#
            )
            .plugins
            .enabled
            .is_empty()
        );
        assert!(
            Config::from_toml_str(
                r#"[plugins]
enabled = ["  ", ""]"#
            )
            .plugins
            .enabled
            .is_empty()
        );
    }

    #[test]
    fn plugins_enabled_parses_and_dedupes() {
        let c = Config::from_toml_str(
            r#"[plugins]
enabled = [" preview ", "preview", "doc"]"#,
        );
        assert_eq!(
            c.plugins.enabled,
            vec!["preview".to_string(), "doc".to_string()]
        );
        // 未知 [plugins] 字段：整体降级默认（同其他节的行为）。
        let c = Config::from_toml_str("[plugins]\nwat = 1");
        assert_eq!(c, Config::default());
    }

    // ------------------------------------------------------------------
    // 面板写回（2026-08-29 决策：单一策略，无 spawn 段）
    // ------------------------------------------------------------------

    #[test]
    fn no_spawn_section_is_accepted() {
        // 单一策略（2026-08-29 决策修订）：没有 [plugins.spawn] 模式段；
        // 旧配置若带 spawn 段 → deny_unknown_fields 整体降级默认（与其
        // 它未知字段同语义，不炸启动）。
        let c = Config::from_toml_str(
            "[plugins]\nenabled = [\"theme\", \"preview\"]\n\n[plugins.paths]\ntheme = \"/x\"\n",
        );
        assert_eq!(
            c.plugins.enabled,
            vec!["theme".to_string(), "preview".to_string()]
        );
        assert_eq!(c.plugins.paths.get("theme").map(String::as_str), Some("/x"));

        let c = Config::from_toml_str(
            "[plugins]\nenabled = [\"theme\"]\n\n[plugins.spawn]\ntheme = \"enable\"\n",
        );
        assert_eq!(
            c,
            Config::default(),
            "旧 spawn 段 = 未知字段：整体降级默认（启动不炸）"
        );
    }

    #[test]
    fn rewrite_enabled_preserves_everything_else() {
        // 1) 有 enabled 行：只换数组，缩进/尾注释/其它行全保留。
        let src = "# 我的配置，勿动\nshell = \"/bin/zsh\"\n\n[plugins]\n  enabled = [\"theme\"]  # 启用的插件\n\n[plugins.paths]\ntheme = \"/x\"\n";
        let out = rewrite_plugins_enabled(src, &["preview".into(), "theme".into()]);
        let expect = "# 我的配置，勿动\nshell = \"/bin/zsh\"\n\n[plugins]\n  enabled = [\"preview\", \"theme\"]  # 启用的插件\n\n[plugins.paths]\ntheme = \"/x\"\n";
        assert_eq!(out, expect);

        // 语义回读：写回后重解析的 enabled 就是新名单。
        assert_eq!(
            Config::from_toml_str(&out).plugins.enabled,
            vec!["preview".to_string(), "theme".to_string()]
        );

        // 2) 清空：enabled = []。
        let out = rewrite_plugins_enabled(src, &[]);
        assert!(out.contains("  enabled = []  # 启用的插件"), "清空后仍是合法数组（保留尾注释）");
        assert!(Config::from_toml_str(&out).plugins.enabled.is_empty());
    }

    #[test]
    fn rewrite_enabled_inserts_or_appends() {
        // [plugins] 节在但无 enabled 行：紧跟节头插入。
        let src = "[plugins]\n# 注释保留\n[plugins.paths]\npreview = \"/x\"\n";
        let out = rewrite_plugins_enabled(src, &["preview".into()]);
        let expect = "[plugins]\nenabled = [\"preview\"]\n# 注释保留\n[plugins.paths]\npreview = \"/x\"\n";
        assert_eq!(out, expect);

        // 完全无 [plugins] 节：末尾追加。
        let src = "shell = \"/bin/zsh\"\n";
        let out = rewrite_plugins_enabled(src, &["theme".into()]);
        assert_eq!(out, "shell = \"/bin/zsh\"\n\n[plugins]\nenabled = [\"theme\"]\n");

        // 空文本（文件不存在时）：从零起节。
        let out = rewrite_plugins_enabled("", &["theme".into()]);
        assert_eq!(out, "\n[plugins]\nenabled = [\"theme\"]\n");

        // enabled 前有注释/数组跨行等罕见形态：整行重写，不炸。
        let src = "[plugins]\nenabled = [\n  \"theme\",\n]\n";
        let out = rewrite_plugins_enabled(src, &[]);
        assert!(out.contains("enabled = []"));
    }
}
