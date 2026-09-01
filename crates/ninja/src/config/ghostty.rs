//! ghostty 配置面：文件发现、装载管线、键位换算、配置读取与取证 dump。

use std::ffi::{c_void, CStr};
use std::path::{Path, PathBuf};

use ghostty_sys::*;

use super::{
    host_layer_text, odp_layer_text, user_sets_theme, BAKED_RESOURCES_DIR, HostConfig,
    HOST_LAYER_FILE, host_config_path, MENU_ACTIONS, ODP_LAYER_FILE, PLUGIN_THEME_LAYER_FILE,
};

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

/// 用系统默认文本编辑器打开 Ghostty 配置（`ghostty_config_open_path` 会
/// 按 AppSupport/XDG 优先序选路径，不存在则创建）。面板按钮与
/// `open_config` 动作共用。
pub fn open_ghostty_config() {
    let Some(path) = ghostty_config_edit_path() else {
        eprintln!("ninja: 无法确定 ghostty 配置路径");
        return;
    };
    match std::process::Command::new("/usr/bin/open")
        .args(["-t", &path])
        .spawn()
    {
        Ok(_) => eprintln!("ninja: 打开 ghostty 配置 {path}"),
        Err(e) => eprintln!("ninja: 打开 ghostty 配置失败：{e}"),
    }
}

fn ghostty_config_edit_path() -> Option<String> {
    let s = unsafe { ghostty_config_open_path() };
    let path = if s.ptr.is_null() || s.len == 0 {
        None
    } else if s.sentinel {
        Some(unsafe { CStr::from_ptr(s.ptr) }.to_string_lossy().into_owned())
    } else {
        let bytes = unsafe { std::slice::from_raw_parts(s.ptr as *const u8, s.len) };
        Some(String::from_utf8_lossy(bytes).into_owned())
    };
    unsafe { ghostty_string_free(s) };
    path.filter(|p| !p.is_empty())
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

/// 行是否引用 `config-file =`，是则返回其值（去引号；未引用值截到空白）。
pub(crate) fn line_config_file(line: &str) -> Option<String> {
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
    //（surface 层动作实际放行）。
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
// 资源目录解析测试（跟代码走）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod res_tests {
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
}
