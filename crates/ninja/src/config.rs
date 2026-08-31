//! q2 配置系统：ghostty 配置装载管线 + ODP 缺省层 + ninja.toml 收缩 +
//! 键位全量继承（trigger→菜单换算）+ 热重载（文件监视）+ 取证 dump。
//!
//! ## 装载管线（键位/主题/字体全部走 ghostty 配置）
//!
//! [`load_pipeline`]：
//!
//! 1. `config_new`（ghostty 默认值，含默认键位表）；
//! 2. `load_file` 宿主层（[`HOST_LAYER_FILE`]，恒装载）：ninja 特有动作
//!    （插件面板）认领 ghostty 的空闲动作 `toggle_visibility` 绑 ⌘,，
//!    用户随后可用普通 `keybind = …=toggle_visibility` 重绑——ghostty
//!    的 Action 联合是封闭的（Binding.zig 对未知动作名抛 InvalidAction，
//!    vendored 测试 "parse: action invalid"），自定义动作名进不了
//!    keybind 配置，认领空闲动作是唯一让宿主动作进 ghostty 键位系统的路；
//! 3. `load_file` ODP 层（[`ODP_LAYER_FILE`]，仅当用户没设 `theme=`）：
//!    One Dark Pro 钉值（bg/fg/cursor/selection/ANSI16）以显式 ghostty
//!    色键装载在默认文件**之前**——用户显式色键后载覆盖；
//! 4. `load_default_files`（XDG + macOS App Support，bundle_id 钉
//!    com.mitchellh.ghostty → 读用户既有 ~/Library/Application
//!    Support/com.mitchellh.ghostty/config）；
//! 5. `load_recursive_files`（`config-file=` 包含链）；
//! 6. `finalize`（具名 `theme=` 在此装载并压顶：Config.zig loadTheme
//!    先读主题文件、再按 _replay_steps 重放已有配置——**因此 ODP 层绝不能
//!    在用户设了 theme= 时装载**，否则 ODP 色键会反压用户主题；宿主侧
//!    事先扫描用户文件判定，见 [`user_sets_theme`]）；
//! 7. 诊断打印（diagnostics_count/get_diagnostic → stderr）。
//!
//! C API 只能从文件装载（无 config_set，q0 审计 #5 实测），程序化注入走
//! 生成文件。层文件写在 `{{tmp}}/ninja-{pid}/`，取证可见。
//!
//! ## 主题资源
//!
//! 具名主题只从 `<XDG>/ghostty/themes/` 或 `<resources>/themes/` 解析；
//! 嵌入构建的资源目录 = `GHOSTTY_RESOURCES_DIR`（resourcesdir.zig，
//! ReleaseFast 生效，ghostty_init 读一次）。vendored 补丁 0002 把钉版
//! iterm2_themes 装到 `vendor/ghostty/out/share/ghostty/themes`，宿主在
//! `ghostty_init` 前设 `GHOSTTY_RESOURCES_DIR` 指过去
//! （[`ensure_resources_dir`]，build.rs 烘焙路径，已设的环境变量不动）。
//!
//! ## ninja.toml 收缩（宿主/插件特有）
//!
//! v1 的 shell/font-family/font-size/[theme] 终端项与 [keys] 键位段不再
//! 属于宿主：终端项走 ghostty 配置，[keys] 平行键位层语义不复活。q2
//! schema 只收 `[plugins]`（enabled/paths），只解析不拉起（监督器是 q3；
//! 空载零插件进程/零 socket 不变）。出现收缩掉的键一律 stderr 警告并忽略
//! （[`parse_host_config`]）。
//!
//! ## 热重载
//!
//! 宿主自监视（NSTimer 轮询 mtime，[`WatchState`]）：默认路径 4 个 +
//! `config-file=` 递归链 + ninja.toml，任一变化 → 重跑装载管线 →
//! `ghostty_app_update_config`（embedded 克隆新配置并传播全部 surface，
//! 回 CONFIG_CHANGE action）→ host 刷新派生态（bg/焦点环/窗口 chrome、
//! 菜单键位重建）。ghostty 默认键位 ⌘⇧,（reload_config action）同途。

use std::ffi::{c_void, CStr};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ghostty_sys::*;

/// q3：插件主题覆盖层文件名（plugins.rs 的 theme.set 适配器写；装载序
/// 压用户文件之后、finalize 之前——finalize 的 loadTheme 重放会把这层
/// 压顶，见模块头）。
pub const PLUGIN_THEME_LAYER_FILE: &str = "plugin-theme.conf";

/// vendored 构建烘进来的 ghostty 资源目录（含 themes/；无则空串）。
pub const BAKED_RESOURCES_DIR: &str = env!("NINJA_GHOSTTY_RESOURCES_DIR");

/// 宿主层文件名（恒装载：ninja 特有动作的键位认领）。
pub const HOST_LAYER_FILE: &str = "host.conf";
/// ODP 层文件名（用户没设 theme= 时装载）。
pub const ODP_LAYER_FILE: &str = "odp.conf";

/// 菜单镜像的 ghostty 动作名（keyEquivalent 全部由
/// `ghostty_config_trigger(action)` 推导；键位单一来源 = ghostty keybind）。
/// 顺序即 dump 顺序。
pub const MENU_ACTIONS: &[&str] = &[
    "quit",
    "toggle_visibility",
    "new_window",
    "new_tab",
    "close_surface",
    "new_split:right",
    "new_split:down",
    "toggle_split_zoom",
    "goto_split:left",
    "goto_split:right",
    "goto_split:up",
    "goto_split:down",
    "goto_split:previous",
    "goto_split:next",
    "previous_tab",
    "next_tab",
    "copy_to_clipboard",
    "paste_from_clipboard",
    "select_all",
];

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

// ---------------------------------------------------------------------------
// 用户 ghostty 配置文件发现（theme 探测 + 热重载监视共用）
// ---------------------------------------------------------------------------

/// loadDefaultFiles 的默认路径镜像（Config.zig/file_load.zig 顺序）：
/// legacy XDG → XDG → legacy App Support → App Support。bundle_id 钉
/// com.mitchellh.ghostty（vendored build_config.zig）。
/// App Support 目录必须与 ghostty 同源解析（NSFileManager/
/// NSSearchPath，**不随 HOME env 变**——实测 HOME 覆盖对它无效，
/// 用 env HOME 会扫到与装载不同的路径，theme 探测就失真了）。
pub fn default_config_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let xdg_base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                let mut p = PathBuf::from(h);
                p.push(".config");
                p
            })
        });
    if let Some(base) = xdg_base {
        for name in ["ghostty/config", "ghostty/config.ghostty"] {
            let mut p = base.clone();
            p.push(name);
            out.push(p);
        }
    }
    if let Some(base) = macos_app_support_dir() {
        for name in [
            "com.mitchellh.ghostty/config",
            "com.mitchellh.ghostty/config.ghostty",
        ] {
            let mut p = base.clone();
            p.push(name);
            out.push(p);
        }
    }
    out
}

/// NSSearchPathForDirectoriesInDomains(.applicationSupportDirectory,
/// .userDomainMask) —— ghostty macos.appSupportDir 同源 API。
fn macos_app_support_dir() -> Option<PathBuf> {
    // objc2 生成的是安全包装（内部处理释放池语义）。
    let paths = objc2_foundation::NSSearchPathForDirectoriesInDomains(
        objc2_foundation::NSSearchPathDirectory::ApplicationSupportDirectory,
        objc2_foundation::NSSearchPathDomainMask::UserDomainMask,
        true,
    );
    let first = paths.firstObject()?;
    Some(PathBuf::from(first.to_string()))
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

/// 行是否引用 `config-file =`，是则返回其值（去引号；未引用值截到空白）。
fn line_config_file(line: &str) -> Option<String> {
    let t = line.trim_start();
    if t.starts_with('#') {
        return None;
    }
    let rest = t.strip_prefix("config-file")?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?;
    let rest = rest.trim_start();
    if rest.is_empty() {
        return None;
    }
    if let Some(stripped) = rest.strip_prefix('"') {
        return stripped.split('"').next().map(|s| s.to_string());
    }
    if let Some(stripped) = rest.strip_prefix('\'') {
        return stripped.split('\'').next().map(|s| s.to_string());
    }
    Some(rest.split_whitespace().next().unwrap_or("").to_string())
}

/// 根文件集 + `config-file=` 递归链（相对路径按所在文件目录解析；
/// visited 防环）。只收**存在**的文件。真实调用方传
/// [`default_config_files`]（测试传临时文件集，保持密闭）。
pub fn collect_ghostty_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut queue: Vec<PathBuf> = roots.to_vec();
    let mut out = Vec::new();
    while let Some(p) = queue.pop() {
        if !p.is_file() {
            continue;
        }
        let canon = p.canonicalize().unwrap_or(p.clone());
        if seen.contains(&canon) {
            continue;
        }
        seen.push(canon);
        if let Ok(text) = std::fs::read_to_string(&p) {
            for line in text.lines() {
                if let Some(rel) = line_config_file(line) {
                    let mut child = PathBuf::from(&rel);
                    if child.is_relative()
                        && let Some(dir) = p.parent()
                    {
                        child = dir.join(child);
                    }
                    queue.push(child);
                }
            }
        }
        out.push(p);
    }
    out
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

// ---------------------------------------------------------------------------
// ninja.toml 收缩（宿主/插件特有）
// ---------------------------------------------------------------------------

/// `[plugins]`：只解析不拉起（监督器 q3；空载零插件进程/零 socket）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginsConfig {
    pub enabled: Vec<String>,
    pub paths: Vec<(String, String)>,
}

/// ninja.toml 收缩后的宿主配置（q2 只有 [plugins]）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostConfig {
    pub plugins: PluginsConfig,
}

/// ninja.toml 路径（NINJA_CONFIG 覆盖 → ~/.config/ninja/ninja.toml）。
pub fn host_config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("NINJA_CONFIG") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".config/ninja/ninja.toml")
}

/// v1 收缩掉的终端项（警告文案用）。
const V1_TERMINAL_KEYS: &[&str] = &["shell", "font-family", "font_family", "font-size", "theme"];

/// 解析收缩后的 ninja.toml。任何不认识的键（含 v1 的终端项与 [keys]）
/// 一律 stderr 警告并忽略——不整体降级（收缩是常态，用户旧文件应尽量
/// 保留可用部分）。
pub fn parse_host_config(text: &str) -> HostConfig {
    let mut cfg = HostConfig::default();
    let value: toml::Value = match toml::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ninja: ninja.toml 解析失败（忽略全部内容）: {e}");
            return cfg;
        }
    };
    let Some(table) = value.as_table() else {
        eprintln!("ninja: ninja.toml 顶层不是表（忽略全部内容）");
        return cfg;
    };
    for (key, val) in table {
        match key.as_str() {
            "plugins" => parse_plugins(val, &mut cfg),
            "keys" => eprintln!(
                "ninja: ninja.toml [keys] 已收缩（键位全量继承 ghostty \
                 keybind 系统），忽略——重绑请写 ghostty 配置 \
                 `keybind = super+x=action`（config 路径见启动日志）"
            ),
            k if V1_TERMINAL_KEYS.contains(&k) => eprintln!(
                "ninja: ninja.toml `{k}` 已收缩——终端配置（shell/字体/主题色）\
                 走 ghostty 配置文件，忽略"
            ),
            other => eprintln!(
                "ninja: ninja.toml 未知键 `{other}`（q2 只收 [plugins]），忽略"
            ),
        }
    }
    cfg
}

fn parse_plugins(val: &toml::Value, cfg: &mut HostConfig) {
    let Some(t) = val.as_table() else {
        eprintln!("ninja: ninja.toml [plugins] 不是表，忽略");
        return;
    };
    for (key, v) in t {
        match key.as_str() {
            "enabled" => {
                let Some(arr) = v.as_array() else {
                    eprintln!("ninja: [plugins] enabled 不是数组，忽略");
                    continue;
                };
                let mut seen = std::collections::BTreeSet::new();
                for item in arr {
                    match item.as_str() {
                        Some(s) => {
                            let s = s.trim();
                            if s.is_empty() || !seen.insert(s.to_string()) {
                                continue;
                            }
                            cfg.plugins.enabled.push(s.to_string());
                        }
                        None => eprintln!("ninja: [plugins] enabled 项不是字符串，忽略"),
                    }
                }
            }
            "paths" => {
                let Some(pt) = v.as_table() else {
                    eprintln!("ninja: [plugins] paths 不是表，忽略");
                    continue;
                };
                for (name, pv) in pt {
                    match pv.as_str() {
                        Some(s) if !s.trim().is_empty() => {
                            cfg.plugins.paths.push((name.clone(), s.trim().to_string()))
                        }
                        _ => eprintln!("ninja: [plugins.paths] {name} 不是非空字符串，忽略"),
                    }
                }
            }
            other => eprintln!("ninja: ninja.toml [plugins] 未知键 `{other}`，忽略"),
        }
    }
}

/// 读 ninja.toml（缺失 = 默认；q2 只解析不拉起）。
pub fn load_host_config() -> HostConfig {
    let path = host_config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_host_config(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HostConfig::default(),
        Err(e) => {
            eprintln!("ninja: 读 {path:?} 失败（{e}），用默认值");
            HostConfig::default()
        }
    }
}

/// q3 面板写回：把 `[plugins] enabled` 名单写回 ninja.toml（toml 往返：
/// 其它段与键保留，注释/格式不保——面板是机器写入方，手写文件请自行
/// 备份；文件不存在则创建）。返回写入路径。
pub fn write_plugins_enabled(enabled: &[String]) -> std::io::Result<PathBuf> {
    let path = host_config_path();
    let mut value: toml::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_else(|| toml::Value::Table(toml::map::Map::new()));
    if !value.is_table() {
        value = toml::Value::Table(toml::map::Map::new()); // 顶层坏值：重建
    }
    let table = value.as_table_mut().expect("顶层是表");
    let plugins = table
        .entry("plugins".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    if !plugins.is_table() {
        *plugins = toml::Value::Table(toml::map::Map::new());
    }
    plugins["enabled"] = toml::Value::Array(
        enabled.iter().map(|s| toml::Value::String(s.clone())).collect(),
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = toml::to_string(&value)
        .map_err(|e| std::io::Error::other(format!("toml 序列化失败：{e}")))?;
    std::fs::write(&path, text)?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// 装载管线
// ---------------------------------------------------------------------------

/// 一次装载的决策取证（dump/日志用）。
#[derive(Clone, Debug)]
pub struct LoadInfo {
    /// 用户是否设置了 theme=（设置 → ODP 层让位）。
    pub user_theme: bool,
    /// ODP 层是否装载。
    pub odp_applied: bool,
    /// 插件主题覆盖层是否装载（q3：theme.set 适配器；色板名）。
    pub plugin_theme: Option<String>,
    /// 层文件目录。
    pub layer_dir: PathBuf,
    /// 监视的配置文件集（热重载用）。
    pub watched: Vec<PathBuf>,
    /// finalize 后的诊断条数。
    pub diagnostics: u32,
}

fn layer_dir() -> PathBuf {
    let mut d = std::env::temp_dir();
    d.push(format!("ninja-{}", std::process::id()));
    d
}

/// 分发 bundle 的资源目录（q4）：可执行文件在 `Contents/MacOS/` 下时，
/// `Contents/Resources/ghostty`（打包脚本拷入的 574 主题随包资源）存在
/// `themes/` 即认定有效。
fn bundle_resources_dir(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    if macos.file_name()?.to_str()? != "MacOS" {
        return None;
    }
    let dir = macos.parent()?.join("Resources/ghostty");
    dir.join("themes").is_dir().then_some(dir)
}

/// 资源目录解析核心（纯函数，单测覆盖分支）：**bundle 相对 > 烘入开发
/// 路径**——分发机上烘入的绝对开发路径（本机构建）不存在，bundle 相对
/// 是唯一真源；开发树里两者都在时 bundle 相对同样优先（装进 /Applications
/// 的副本不该回头看开发树）。
fn resolve_resources_dir(exe: Option<&Path>, baked: &str) -> Option<PathBuf> {
    if let Some(dir) = exe.and_then(bundle_resources_dir) {
        return Some(dir);
    }
    if !baked.is_empty() && Path::new(baked).join("themes").is_dir() {
        return Some(PathBuf::from(baked));
    }
    None
}

/// 在 `ghostty_init` 前解析并设 `GHOSTTY_RESOURCES_DIR`（具名主题解析需要；
/// resourcesdir.zig 只在 init 读一次）。优先级：已设的环境变量（用户覆盖/
/// 调试，不动）> bundle 相对（q4 分发）> build.rs 烘入的开发路径。都解析
/// 不到则不设，具名主题会解析失败并出现在诊断里。
pub fn ensure_resources_dir() {
    if std::env::var_os("GHOSTTY_RESOURCES_DIR").is_some() {
        return;
    }
    let exe = std::env::current_exe().ok();
    if let Some(dir) = resolve_resources_dir(exe.as_deref(), BAKED_RESOURCES_DIR) {
        // SAFETY: main 线程早期、ghostty_init 之前（唯一入口 main 调）。
        unsafe { std::env::set_var("GHOSTTY_RESOURCES_DIR", &dir) };
    }
}

fn load_file_cfg(cfg: ghostty_config_t, path: &Path) {
    let cpath = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .expect("config path has no NUL");
    unsafe { ghostty_config_load_file(cfg, cpath.as_ptr()) };
}

/// 全量装载管线（见模块头）。返回宿主自有的 config 句柄（host 负责 free）。
pub fn load_pipeline() -> (ghostty_config_t, LoadInfo) {
    // ninja.toml（收缩后宿主配置）与 ghostty 配置一起监视/重读。
    let ninja_toml = host_config_path();
    let ghostty_files = collect_ghostty_files(&default_config_files());
    let watched = {
        let mut w = ghostty_files.clone();
        w.push(ninja_toml);
        w
    };
    let user_theme = user_sets_theme(&ghostty_files);
    let odp_applied = !user_theme;
    // q3：插件主题覆盖（theme.set 适配器写层文件压顶）。
    let plugin_theme = crate::plugins::plugin_theme_override();

    let dir = layer_dir();
    let _ = std::fs::create_dir_all(&dir);
    let host_layer = dir.join(HOST_LAYER_FILE);
    let _ = std::fs::write(&host_layer, host_layer_text());
    let odp_layer = dir.join(ODP_LAYER_FILE);
    if odp_applied {
        let _ = std::fs::write(&odp_layer, odp_layer_text());
    }
    let plugin_layer = dir.join(PLUGIN_THEME_LAYER_FILE);
    if let Some((_, text)) = &plugin_theme {
        let _ = std::fs::write(&plugin_layer, text);
    } else {
        let _ = std::fs::remove_file(&plugin_layer);
    }

    unsafe {
        let cfg = ghostty_config_new();
        // 宿主层恒装载（ninja 特有动作的键位认领）。
        load_file_cfg(cfg, &host_layer);
        // ODP 层：仅当用户没设 theme=（见模块头：finalize 的 loadTheme
        // 会重放已有配置，ODP 先载会反压用户主题）。
        if odp_applied {
            load_file_cfg(cfg, &odp_layer);
        }
        ghostty_config_load_default_files(cfg);
        ghostty_config_load_recursive_files(cfg);
        // 插件主题层：压用户文件之后、finalize 之前——loadTheme 的
        // _replay_steps 重放会把这层压在一切之上（q3 theme.set 适配器）。
        if plugin_theme.is_some() {
            load_file_cfg(cfg, &plugin_layer);
        }
        ghostty_config_finalize(cfg);
        let diagnostics = print_diagnostics(cfg);
        (cfg, LoadInfo {
            user_theme,
            odp_applied,
            plugin_theme: plugin_theme.map(|(name, _)| name),
            layer_dir: dir,
            watched,
            diagnostics,
        })
    }
}

/// finalize 后的诊断全部打到 stderr（配置错误对用户可见：
/// theme 找不到、非法值等——ghostty 内部 log 之外的第二道可见层）。
unsafe fn print_diagnostics(cfg: ghostty_config_t) -> u32 {
    let n = unsafe { ghostty_config_diagnostics_count(cfg) };
    for i in 0..n {
        let d = unsafe { ghostty_config_get_diagnostic(cfg, i) };
        if !d.message.is_null() {
            // SAFETY: message 是 config 存活期内的 C 字符串（读拷贝立即用）。
            let msg = unsafe { std::ffi::CStr::from_ptr(d.message).to_string_lossy() };
            eprintln!("ninja: ghostty 配置诊断: {msg}");
        }
    }
    n
}

// ---------------------------------------------------------------------------
// 热重载监视（mtime 轮询）
// ---------------------------------------------------------------------------

/// 配置文件集的 mtime 快照（None = 文件不存在）。
#[derive(Clone, Debug, Default)]
pub struct WatchState {
    mtimes: Vec<Option<SystemTime>>,
}

pub fn snapshot_watch(files: &[PathBuf]) -> WatchState {
    WatchState {
        mtimes: files
            .iter()
            .map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
            .collect(),
    }
}

impl WatchState {
    /// 与当前文件集比较（集合本身变化也算变化）。变化时更新快照。
    pub fn changed(&mut self, files: &[PathBuf]) -> bool {
        let now = snapshot_watch(files);
        let diff = now.mtimes != self.mtimes;
        if diff {
            self.mtimes = now.mtimes;
        }
        diff
    }
}

// ---------------------------------------------------------------------------
// trigger → 菜单 keyEquivalent 换算（纯函数）
// ---------------------------------------------------------------------------

/// 菜单 keyEquivalent（keyEquivalent 字符 + 修饰；无绑定 → None）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEquivalent {
    /// keyEquivalent 字符（箭头等功能键用 F700 系字符，AppKit 惯例）。
    pub key: u16,
    pub cmd: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// ghostty 物理/unicode 键 → keyEquivalent 字符。
fn physical_key_to_char(k: ghostty_input_key_e) -> Option<u16> {
    // NSResponder 功能键字符（NSEvent.h）。bindgen 把键枚举铺成常量，
    // 常量可直接作 match 模式（模块头已 glob use ghostty_sys::*）。
    const F1: u16 = 0xF704;
    const HOME: u16 = 0xF729;
    const INSERT: u16 = 0xF727;
    const DELETE: u16 = 0xF728;
    const END: u16 = 0xF72B;
    const PAGE_UP: u16 = 0xF72C;
    const PAGE_DOWN: u16 = 0xF72D;
    const UP: u16 = 0xF700;
    const DOWN: u16 = 0xF701;
    const LEFT: u16 = 0xF702;
    const RIGHT: u16 = 0xF703;
    Some(match k {
        GHOSTTY_KEY_A => 'a' as u16,
        GHOSTTY_KEY_B => 'b' as u16,
        GHOSTTY_KEY_C => 'c' as u16,
        GHOSTTY_KEY_D => 'd' as u16,
        GHOSTTY_KEY_E => 'e' as u16,
        GHOSTTY_KEY_F => 'f' as u16,
        GHOSTTY_KEY_G => 'g' as u16,
        GHOSTTY_KEY_H => 'h' as u16,
        GHOSTTY_KEY_I => 'i' as u16,
        GHOSTTY_KEY_J => 'j' as u16,
        GHOSTTY_KEY_K => 'k' as u16,
        GHOSTTY_KEY_L => 'l' as u16,
        GHOSTTY_KEY_M => 'm' as u16,
        GHOSTTY_KEY_N => 'n' as u16,
        GHOSTTY_KEY_O => 'o' as u16,
        GHOSTTY_KEY_P => 'p' as u16,
        GHOSTTY_KEY_Q => 'q' as u16,
        GHOSTTY_KEY_R => 'r' as u16,
        GHOSTTY_KEY_S => 's' as u16,
        GHOSTTY_KEY_T => 't' as u16,
        GHOSTTY_KEY_U => 'u' as u16,
        GHOSTTY_KEY_V => 'v' as u16,
        GHOSTTY_KEY_W => 'w' as u16,
        GHOSTTY_KEY_X => 'x' as u16,
        GHOSTTY_KEY_Y => 'y' as u16,
        GHOSTTY_KEY_Z => 'z' as u16,
        GHOSTTY_KEY_DIGIT_0 => '0' as u16,
        GHOSTTY_KEY_DIGIT_1 => '1' as u16,
        GHOSTTY_KEY_DIGIT_2 => '2' as u16,
        GHOSTTY_KEY_DIGIT_3 => '3' as u16,
        GHOSTTY_KEY_DIGIT_4 => '4' as u16,
        GHOSTTY_KEY_DIGIT_5 => '5' as u16,
        GHOSTTY_KEY_DIGIT_6 => '6' as u16,
        GHOSTTY_KEY_DIGIT_7 => '7' as u16,
        GHOSTTY_KEY_DIGIT_8 => '8' as u16,
        GHOSTTY_KEY_DIGIT_9 => '9' as u16,
        GHOSTTY_KEY_COMMA => ',' as u16,
        GHOSTTY_KEY_PERIOD => '.' as u16,
        GHOSTTY_KEY_SLASH => '/' as u16,
        GHOSTTY_KEY_SEMICOLON => ';' as u16,
        GHOSTTY_KEY_QUOTE => '\'' as u16,
        GHOSTTY_KEY_MINUS => '-' as u16,
        GHOSTTY_KEY_EQUAL => '=' as u16,
        GHOSTTY_KEY_BRACKET_LEFT => '[' as u16,
        GHOSTTY_KEY_BRACKET_RIGHT => ']' as u16,
        GHOSTTY_KEY_BACKSLASH => '\\' as u16,
        GHOSTTY_KEY_BACKQUOTE => '`' as u16,
        GHOSTTY_KEY_SPACE => ' ' as u16,
        GHOSTTY_KEY_ENTER => 0x0D,
        GHOSTTY_KEY_TAB => 0x09,
        GHOSTTY_KEY_BACKSPACE => 0x08,
        GHOSTTY_KEY_ESCAPE => 0x1B,
        GHOSTTY_KEY_ARROW_UP => UP,
        GHOSTTY_KEY_ARROW_DOWN => DOWN,
        GHOSTTY_KEY_ARROW_LEFT => LEFT,
        GHOSTTY_KEY_ARROW_RIGHT => RIGHT,
        GHOSTTY_KEY_HOME => HOME,
        GHOSTTY_KEY_END => END,
        GHOSTTY_KEY_PAGE_UP => PAGE_UP,
        GHOSTTY_KEY_PAGE_DOWN => PAGE_DOWN,
        GHOSTTY_KEY_INSERT => INSERT,
        GHOSTTY_KEY_DELETE => DELETE,
        GHOSTTY_KEY_F1 => F1,
        GHOSTTY_KEY_F2 => F1 + 1,
        GHOSTTY_KEY_F3 => F1 + 2,
        GHOSTTY_KEY_F4 => F1 + 3,
        GHOSTTY_KEY_F5 => F1 + 4,
        GHOSTTY_KEY_F6 => F1 + 5,
        GHOSTTY_KEY_F7 => F1 + 6,
        GHOSTTY_KEY_F8 => F1 + 7,
        GHOSTTY_KEY_F9 => F1 + 8,
        GHOSTTY_KEY_F10 => F1 + 9,
        GHOSTTY_KEY_F11 => F1 + 10,
        GHOSTTY_KEY_F12 => F1 + 11,
        _ => return None,
    })
}

/// `ghostty_config_trigger(action)` 结果 → 菜单 keyEquivalent。
/// 空 trigger（动作未绑定）→ None（菜单项无快捷键、点击不驱动）。
pub fn trigger_to_equivalent(t: ghostty_input_trigger_s) -> Option<KeyEquivalent> {
    // SAFETY: 联合体按 tag 读对应字段（bindgen 生成联合的常态读法）。
    let key = unsafe {
        match t.tag {
            GHOSTTY_TRIGGER_UNICODE => u16::try_from(t.key.unicode).ok()?,
            GHOSTTY_TRIGGER_PHYSICAL => physical_key_to_char(t.key.physical)?,
            _ => return None, // catch_all / 空
        }
    };
    let mods = t.mods;
    Some(KeyEquivalent {
        key,
        cmd: mods & GHOSTTY_MODS_SUPER != 0,
        ctrl: mods & GHOSTTY_MODS_CTRL != 0,
        alt: mods & GHOSTTY_MODS_ALT != 0,
        shift: mods & GHOSTTY_MODS_SHIFT != 0,
    })
}

/// 便捷：config + 动作名 → keyEquivalent。
pub fn action_equivalent(cfg: ghostty_config_t, action: &str) -> Option<KeyEquivalent> {
    let t = unsafe { ghostty_config_trigger(cfg, action.as_ptr() as *const _, action.len()) };
    trigger_to_equivalent(t)
}

// ---------------------------------------------------------------------------
// config 读值 + 取证 dump
// ---------------------------------------------------------------------------

/// 读一个颜色键（null/不支持 → None）。
pub fn get_color(cfg: ghostty_config_t, key: &str) -> Option<(u8, u8, u8)> {
    let mut c = ghostty_config_color_s { r: 0, g: 0, b: 0 };
    let ok = unsafe {
        ghostty_config_get(
            cfg,
            &mut c as *mut ghostty_config_color_s as *mut c_void,
            key.as_ptr() as *const _,
            key.len(),
        )
    };
    ok.then_some((c.r, c.g, c.b))
}

/// 读 f32 键（font-size 等）。
pub fn get_f32(cfg: ghostty_config_t, key: &str) -> Option<f32> {
    let mut v: f32 = 0.0;
    let ok = unsafe {
        ghostty_config_get(
            cfg,
            &mut v as *mut f32 as *mut c_void,
            key.as_ptr() as *const _,
            key.len(),
        )
    };
    ok.then_some(v)
}

/// 读可选 i16（window-position-x/y；未设 → None）。
pub fn get_i16(cfg: ghostty_config_t, key: &str) -> Option<i16> {
    let mut v: i16 = 0;
    let ok = unsafe {
        ghostty_config_get(
            cfg,
            &mut v as *mut i16 as *mut c_void,
            key.as_ptr() as *const _,
            key.len(),
        )
    };
    ok.then_some(v)
}

/// 读枚举键（C API 给出 C 字符串，如 window-save-state = "always"）。
pub fn get_enum_str(cfg: ghostty_config_t, key: &str) -> Option<String> {
    let mut p: *const std::ffi::c_char = std::ptr::null();
    let ok = unsafe {
        ghostty_config_get(
            cfg,
            &mut p as *mut *const std::ffi::c_char as *mut c_void,
            key.as_ptr() as *const _,
            key.len(),
        )
    };
    if !ok || p.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// 读 bool 键（q0 审计遗留的 link-previews 回读怪象记录用）。
pub fn get_bool(cfg: ghostty_config_t, key: &str) -> Option<bool> {
    // ghostty.h：config_get 的 bool 按 bool（1 字节）读。
    let mut v: bool = false;
    let ok = unsafe {
        ghostty_config_get(
            cfg,
            &mut v as *mut bool as *mut c_void,
            key.as_ptr() as *const _,
            key.len(),
        )
    };
    ok.then_some(v)
}

/// 读 palette 前 16 色（ANSI；不支持 → None）。
pub fn get_palette16(cfg: ghostty_config_t) -> Option<Vec<(u8, u8, u8)>> {
    let mut p = ghostty_config_palette_s {
        colors: [ghostty_config_color_s { r: 0, g: 0, b: 0 }; 256],
    };
    let ok = unsafe {
        ghostty_config_get(
            cfg,
            &mut p as *mut ghostty_config_palette_s as *mut c_void,
            c"palette".as_ptr(),
            7,
        )
    };
    ok.then(|| p.colors.iter().take(16).map(|c| (c.r, c.g, c.b)).collect())
}

fn json_rgb(c: (u8, u8, u8)) -> String {
    format!("[{},{},{}]", c.0, c.1, c.2)
}

fn json_str(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

/// 触发器可读描述（纯文本）：super+shift+'t'。
fn equivalent_plain(e: Option<KeyEquivalent>) -> String {
    let e = match e {
        Some(e) => e,
        None => return "null".to_string(),
    };
    let mut s = String::new();
    if e.ctrl {
        s.push_str("ctrl+");
    }
    if e.alt {
        s.push_str("alt+");
    }
    if e.shift {
        s.push_str("shift+");
    }
    if e.cmd {
        s.push_str("super+");
    }
    // 控制字符（\r 等）与私用区（F700 系功能键）转 \uXXXX——dump
    // 是合法 JSON（E2E 用 json.load 断言）且功能键可见。
    match char::from_u32(u32::from(e.key)) {
        Some(c) if (c as u32) < 0x20 || (0xE000..=0xF8FF).contains(&(c as u32)) => {
            s.push_str(&format!("'\\u{:04x}'", e.key));
        }
        Some(c) => s.push_str(&format!("'{c}'")),
        None => s.push_str("'?'"),
    }
    s
}

/// 触发器可读描述（dump 用，输出为 JSON 字符串字面量；None → null）：
/// "super+shift+'t'"。
fn equivalent_desc(e: Option<KeyEquivalent>) -> String {
    match e {
        None => "null".to_string(),
        Some(_) => json_str(&equivalent_plain(e)),
    }
}

/// 写生效配置取证 JSON（NINJA_CFG_DUMP=<path>；启动 + 每次重载后调）。
pub fn dump_effective_config(
    path: &str,
    cfg: ghostty_config_t,
    info: &LoadInfo,
    host_cfg: &HostConfig,
) {
    let mut s = String::from("{\n");
    s.push_str(&format!(
        "  \"resources_dir\": {},\n",
        json_str(&std::env::var("GHOSTTY_RESOURCES_DIR").unwrap_or_default())
    ));
    s.push_str(&format!("  \"user_theme\": {},\n", info.user_theme));
    s.push_str(&format!("  \"odp_applied\": {},\n", info.odp_applied));
    s.push_str(&format!(
        "  \"plugin_theme\": {},\n",
        info.plugin_theme
            .as_deref()
            .map(json_str)
            .unwrap_or_else(|| "null".into())
    ));
    s.push_str(&format!(
        "  \"layer_dir\": {},\n",
        json_str(&info.layer_dir.to_string_lossy())
    ));
    s.push_str(&format!(
        "  \"watched\": [{}],\n",
        info.watched
            .iter()
            .map(|p| json_str(&p.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    s.push_str(&format!(
        "  \"background\": {},\n",
        get_color(cfg, "background").map(json_rgb).unwrap_or_else(|| "null".into())
    ));
    s.push_str(&format!(
        "  \"foreground\": {},\n",
        get_color(cfg, "foreground").map(json_rgb).unwrap_or_else(|| "null".into())
    ));
    s.push_str(&format!(
        "  \"cursor_color\": {},\n",
        get_color(cfg, "cursor-color").map(json_rgb).unwrap_or_else(|| "null".into())
    ));
    s.push_str(&format!(
        "  \"selection_background\": {},\n",
        get_color(cfg, "selection-background")
            .map(json_rgb)
            .unwrap_or_else(|| "null".into())
    ));
    s.push_str(&format!(
        "  \"font_size\": {},\n",
        get_f32(cfg, "font-size")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".into())
    ));
    s.push_str(&format!(
        "  \"palette16\": {},\n",
        get_palette16(cfg)
            .map(|ps| {
                format!(
                    "[{}]",
                    ps.iter().map(|c| json_rgb(*c)).collect::<Vec<_>>().join(",")
                )
            })
            .unwrap_or_else(|| "null".into())
    ));
    s.push_str("  \"triggers\": {\n");
    for (i, a) in MENU_ACTIONS.iter().enumerate() {
        let desc = equivalent_desc(action_equivalent(cfg, a));
        s.push_str(&format!("    {}: {}", json_str(a), desc));
        if i + 1 < MENU_ACTIONS.len() {
            s.push(',');
        }
        s.push('\n');
    }
    s.push_str("  },\n");
    s.push_str(&format!("  \"diagnostics\": {},\n", info.diagnostics));
    // q0 审计遗留记录：app 级句柄读 link-previews 恒 false 的怪象
    //（surface 层动作实际放行；见 docs/Q0-CAPABILITY-AUDIT.md #2）。
    s.push_str(&format!(
        "  \"link_previews_readback\": {},\n",
        get_bool(cfg, "link-previews")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".into())
    ));
    s.push_str(&format!(
        "  \"plugins_enabled\": [{}]\n",
        host_cfg
            .plugins
            .enabled
            .iter()
            .map(|n| json_str(n))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    s.push_str("}\n");
    if let Err(e) = std::fs::write(path, s) {
        eprintln!("ninja: 写 NINJA_CFG_DUMP {path:?} 失败: {e}");
    }
}

// ---------------------------------------------------------------------------
// 单元测试（纯函数；装载管线/E2E 见 docs/q2-evidence）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_res_dir(dir: &Path) -> PathBuf {
        // 在 dir 下造 themes/（含一个主题文件）＝ 有效资源目录。
        std::fs::create_dir_all(dir.join("themes")).unwrap();
        std::fs::write(dir.join("themes/OneDarkPro"), "palette = 0=#3f4451\n").unwrap();
        dir.to_path_buf()
    }

    #[test]
    fn resources_bundle_relative_wins_over_baked_dev_path() {
        // q4 分发：安装副本（Contents/MacOS 可执行 + Resources/ghostty）在
        // 开发机上也不得回头看烘入路径——bundle 相对优先。
        let root = std::env::temp_dir().join(format!("ninja-res-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("app/Contents/MacOS")).unwrap();
        let bundle = mk_res_dir(&root.join("app/Contents/Resources/ghostty"));
        std::fs::write(root.join("app/Contents/MacOS/ninja"), b"").unwrap();
        let baked = mk_res_dir(&root.join("dev-ghostty")); // 烘入路径同样有效
        let got = resolve_resources_dir(Some(&root.join("app/Contents/MacOS/ninja")), baked.to_str().unwrap());
        assert_eq!(got.as_deref(), Some(bundle.as_path()), "bundle 相对必须优先于烘入路径");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resources_baked_dev_path_when_no_bundle() {
        // 开发树（target/release/ninja，无 Resources 布局）：烘入路径生效。
        let root = std::env::temp_dir().join(format!("ninja-res-baked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("target/release")).unwrap();
        std::fs::write(root.join("target/release/ninja"), b"").unwrap();
        let baked = mk_res_dir(&root.join("dev-ghostty"));
        let got = resolve_resources_dir(Some(&root.join("target/release/ninja")), baked.to_str().unwrap());
        assert_eq!(got.as_deref(), Some(baked.as_path()));
        // bundle 布局在但 Resources/ghostty 缺 themes/（坏包）→ 走烘入。
        std::fs::create_dir_all(root.join("app2/Contents/MacOS")).unwrap();
        std::fs::write(root.join("app2/Contents/MacOS/ninja"), b"").unwrap();
        std::fs::create_dir_all(root.join("app2/Contents/Resources/ghostty")).unwrap();
        let got = resolve_resources_dir(Some(&root.join("app2/Contents/MacOS/ninja")), baked.to_str().unwrap());
        assert_eq!(got.as_deref(), Some(baked.as_path()), "无 themes/ 的 bundle 目录不算数");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resources_none_when_neither() {
        // 外部产物（NINJA_GHOSTTY_EMBED_DIR 无资源）：exe 无 bundle、烘入空。
        let root = std::env::temp_dir().join(format!("ninja-res-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let got = resolve_resources_dir(Some(&root.join("bin/ninja")), "");
        assert_eq!(got, None);
        let got = resolve_resources_dir(None, "");
        assert_eq!(got, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn odp_layer_text_is_explicit_ghostty_config() {
        let text = odp_layer_text();
        // 色键齐全且值 = ODP 钉值。
        assert!(text.contains("background = #282c34"));
        assert!(text.contains("foreground = #abb2bf"));
        assert!(text.contains("cursor-color = #528bff"));
        assert!(text.contains("selection-background = #41454e"));
        assert!(text.contains("palette = 0=#3f4451"));
        assert!(text.contains("palette = 15=#e6e6e6"));
        // 全部 16 条都在。
        for i in 0..16 {
            assert!(text.contains(&format!("palette = {i}=")), "missing palette {i}");
        }
    }

    #[test]
    fn host_layer_claims_toggle_visibility_on_bare_comma() {
        // 认领触发器必须与 ghostty 默认 ⌘,（super + unicode ','）同形，
        // 才能替换默认的 open_config 绑定（Trigger.set 同键覆盖）。
        let text = host_layer_text();
        assert!(text.contains("keybind = super+,=toggle_visibility\n"));
    }

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

    #[test]
    fn config_file_line_parsing() {
        assert_eq!(line_config_file("config-file = /abs/x.conf"), Some("/abs/x.conf".into()));
        assert_eq!(line_config_file("config-file=\"a b.conf\""), Some("a b.conf".into()));
        assert_eq!(line_config_file("config-file = rel.conf # 尾注释"), Some("rel.conf".into()));
        assert_eq!(line_config_file("config-file ="), None);
        assert_eq!(line_config_file("font-size = 18"), None);
    }

    #[test]
    fn collect_follows_config_file_chain_with_cycle_guard() {
        let dir = std::env::temp_dir().join(format!("ninja-cfg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.conf"), "theme=Dracula\nconfig-file = inc.conf\n").unwrap();
        // 环：inc 反指 main（visited 防环不死循环）。
        std::fs::write(dir.join("inc.conf"), "config-file = main.conf\nbackground = #010203\n").unwrap();
        let files = collect_ghostty_files(&[dir.join("main.conf")]);
        assert_eq!(files.len(), 2, "chain followed, cycle cut: {files:?}");
        assert!(user_sets_theme(&files));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn odp_yields_when_user_sets_theme_anywhere_in_chain() {
        // ODP 让位扫描：默认文件或 config-file 链上任一处 theme= 都让位。
        let dir = std::env::temp_dir().join(format!("ninja-yield-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a"), "font-size = 14\nconfig-file = b\n").unwrap();
        std::fs::write(dir.join("b"), "theme = Dracula\n").unwrap();
        let files = collect_ghostty_files(&[dir.join("a")]);
        assert!(user_sets_theme(&files), "链上 theme= 必须探测到");
        // 注释里的 theme= 不算。
        std::fs::write(dir.join("b"), "# theme = Dracula\n").unwrap();
        let files = collect_ghostty_files(&[dir.join("a")]);
        assert!(!user_sets_theme(&files));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------------
    // ninja.toml 收缩
    // ------------------------------------------------------------------

    #[test]
    fn host_config_plugins_only() {
        let cfg = parse_host_config(
            r##"
[plugins]
enabled = [" preview ", "preview", "doc"]
[plugins.paths]
preview = "/usr/local/bin/ninja-preview"
"##,
        );
        assert_eq!(
            cfg.plugins.enabled,
            vec!["preview".to_string(), "doc".to_string()]
        );
        assert_eq!(
            cfg.plugins.paths,
            vec![("preview".to_string(), "/usr/local/bin/ninja-preview".to_string())]
        );
    }

    #[test]
    fn host_config_default_empty_no_spawn() {
        assert_eq!(parse_host_config(""), HostConfig::default());
        assert_eq!(
            parse_host_config("[plugins]\nenabled = []").plugins.enabled,
            Vec::<String>::new()
        );
        // 破损 TOML：全忽略（不炸）。
        assert_eq!(parse_host_config("this is [ not toml"), HostConfig::default());
    }

    #[test]
    fn host_config_terminal_keys_and_keys_ignored() {
        // v1 终端项 + [keys] + 未知键：警告 + 忽略，但 [plugins] 仍生效
        //（收缩是常态，不整体降级）。
        let cfg = parse_host_config(
            r##"shell = "/bin/zsh"
font-family = "Menlo"
font-size = 14.0

[theme]
cursor = "#528BFF"

[keys]
new_window = "cmd+n"

[plugins]
enabled = ["theme"]

[unknown_section]
x = 1
"##,
        );
        assert_eq!(cfg.plugins.enabled, vec!["theme".to_string()]);
        assert_eq!(cfg.plugins.paths, Vec::new());
        // 只有 [plugins] 进结果——shell/font/theme/keys 无字段可查（结构体里没有）。
    }

    // ------------------------------------------------------------------
    // trigger → keyEquivalent
    // ------------------------------------------------------------------

    fn uni(cp: u32, mods: u32) -> ghostty_input_trigger_s {
        ghostty_input_trigger_s {
            tag: GHOSTTY_TRIGGER_UNICODE,
            key: ghostty_input_trigger_key_u { unicode: cp },
            mods: mods as _,
        }
    }

    fn phys(k: ghostty_input_key_e, mods: u32) -> ghostty_input_trigger_s {
        ghostty_input_trigger_s {
            tag: GHOSTTY_TRIGGER_PHYSICAL,
            key: ghostty_input_trigger_key_u { physical: k },
            mods: mods as _,
        }
    }

    fn empty_trigger() -> ghostty_input_trigger_s {
        ghostty_input_trigger_s {
            tag: GHOSTTY_TRIGGER_PHYSICAL,
            key: ghostty_input_trigger_key_u { physical: 0 },
            mods: 0,
        }
    }

    #[test]
    fn trigger_conversion_forms() {
        // ⌘T（默认 new_tab：unicode 't' + super）。
        let e = trigger_to_equivalent(uni('t' as u32, GHOSTTY_MODS_SUPER)).unwrap();
        assert_eq!(e.key, 't' as u16);
        assert!(e.cmd && !e.shift && !e.alt && !e.ctrl);

        // ⌘⇧Enter（toggle_split_zoom：physical enter）。
        let e = trigger_to_equivalent(phys(
            GHOSTTY_KEY_ENTER,
            GHOSTTY_MODS_SUPER | GHOSTTY_MODS_SHIFT,
        ))
        .unwrap();
        assert_eq!(e.key, 0x0D);
        assert!(e.cmd && e.shift);

        // ⌥⌘←（goto_split:left：physical arrow → F702）。
        let e = trigger_to_equivalent(phys(
            GHOSTTY_KEY_ARROW_LEFT,
            GHOSTTY_MODS_SUPER | GHOSTTY_MODS_ALT,
        ))
        .unwrap();
        assert_eq!(e.key, 0xF702);
        assert!(e.cmd && e.alt);

        // 空 trigger（动作未绑定：physical 0 = unidentified）→ None。
        assert_eq!(trigger_to_equivalent(empty_trigger()), None);
        // catch-all → None。
        assert_eq!(
            trigger_to_equivalent(ghostty_input_trigger_s {
                tag: GHOSTTY_TRIGGER_CATCH_ALL,
                ..empty_trigger()
            }),
            None
        );
        // BMP 外 unicode → None。
        assert_eq!(trigger_to_equivalent(uni(0x1F600, 0)), None);
    }

    // ------------------------------------------------------------------
    // 热重载快照
    // ------------------------------------------------------------------

    #[test]
    fn watch_state_detects_change_and_new_file() {
        let dir = std::env::temp_dir().join(format!("ninja-watch-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("config");
        std::fs::write(&f, "font-size = 13\n").unwrap();
        let files = vec![f.clone()];
        let mut st = snapshot_watch(&files);
        assert!(!st.changed(&files), "unchanged");
        std::fs::write(&f, "font-size = 14\n").unwrap();
        assert!(st.changed(&files), "mtime change detected");
        assert!(!st.changed(&files), "snapshot updated");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
