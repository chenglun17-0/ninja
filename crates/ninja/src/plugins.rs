//! q3 插件系统（宿主监督器 + hit/layer/input/theme 四个适配器）。
//!
//! # 监督器（单一策略：启用即拉起 / 禁用即回收）
//!
//! - 空载（`[plugins] enabled` 为空，默认）**不创建 socket、不拉任何
//!   插件进程**——[`start`] 直接返回 `None`，宿主里没有任何插件运行时
//!   （空载红线，q1/q2 已有取证须保持）。
//! - 非空：清扫陈旧 socket（[`sweep_stale_sockets`]，宿主 SIGKILL 留下
//!   的 `ninja-ade-<pid>.sock` 尸体：文件名 pid 已死才删）→ 绑定
//!   `${TMPDIR}/ninja-ade-{pid}.sock`（`NINJA_ADE_SOCK` 可覆盖）→
//!   runloop 就绪后（app 的 applicationDidFinishLaunching）按名拉起全部
//!   enabled 插件（spawn 注入 `NINJA_ADE_SOCK`）。
//! - 二进制解析：`[plugins.paths]` 显式路径 → `$NINJA_PLUGIN_DIR/<name>`
//!   → `~/.config/ninja/plugins/<name>` → 宿主二进制同目录（开发布局：
//!   宿主与插件同置一个 target 目录）。
//! - 禁用（面板 off / [`PluginHost::session_disable`] / [`shutdown`]）走
//!   同一条幂等生命周期：杀子进程 + 收层 + 主题覆盖回退 + 断连接 +
//!   名单空则删 socket——「关掉即轻」。
//! - 宿主 SIGKILL 时 socket 尸体由下次启动的清扫收；插件进程因 socket
//!   EOF 自退（正常退出路径零强杀）。
//!
//! # hit 适配器（双数据源，Ghostty 语义坑全停在本模块）
//!
//! - **链接源（路径主源）**：ghostty 自己的 ⌘+click → `OPEN_URL` action
//!   （宿主在 action 分发接管，host.rs）。ghostty 的 URL 匹配器 +
//!   `resolvePathForOpening` 会把路径 token 解析成绝对路径再送出——
//!   无 scheme 的载荷归 `path`。hover/⌘ 修饰判定、`link-previews`
//!   门控、`config_get(link-previews)` 回读怪象全部消化在 ghostty 内核
//!   与本适配器——不进协议。
//! - **网格源（兜底）**：⌘+click 无链接命中时，宿主用
//!   `ghostty_surface_read_text` 读点击行 + 网格占比换算做 token 识别。
//!   cwd：OSC-7/`PWD` action（Ghostty 原样是 `file://…`，适配器剥成
//!   文件系统路径）→ 前台 pid 的真实 cwd（包里没有 shell-integration
//!   时 OSC-7 根本不会来）。空串才放弃相对路径。
//! - 广播 `hit` → 收 `hit.claim`/`hit.ignore`（500ms 同步短超时；静默/
//!   断连=不认领）→ `priority` 大者胜 → 无认领走系统默认
//!   （`/usr/bin/open`）。
//!
//! # layer 适配器（q0 审计 #4 的结构路线）
//!
//! `layer.open` → `placement`（overlay/side/tab）× `surface`（pixels/html）。
//! 像素：宿主建全局 IOSurface，插件写入，`layer.present` 合成。html：宿主建
//! WKWebView，插件发 `layer.html` / `layer.msg`（不透明邮箱，内核不分派名字）。
//! 宿主不出现插件名词。
//!
//! # input 适配器
//!
//! - `input.hotkey` → 对 ghostty 生效键位（`ghostty_config_key_is_binding`）
//!   与已授予插件查冲突 → granted/denied。授予的热键触发经
//!   `input.key{layer:0}` 投递（适配器语义，协议面不变）。
//! - 层前台时 SurfaceHostView 的 keyDown 先查本模块路由 `input.key`
//!   （Esc 语义收口在宿主：直接关层，PRODUCT「任何插件层都能立刻关掉」），
//!   未命中再进既有 surface_key 链。像素层另发 `input.mouse` / `input.scroll`
//!   / `input.focus`。html 表面键鼠留在 WebKit。
//!
//! # theme 适配器（无 `config_set` 的宿主绕法）
//!
//! 嵌入 C API 只能从文件装载配置（q0 审计 #5），程序化注入唯一路径 =
//! 生成文件装载：`theme.set` 校验（`#rrggbb`×20、alpha 0-255，坏值整条
//! 忽略不断连）→ 写 `{{tmp}}/ninja-{pid}/plugin-theme.conf`（bg/fg/cursor/
//! selection/ANSI16 显式色键；装载序压用户文件之后、finalize 之前——
//! finalize 的 loadTheme 重放会把这层压顶，见 crate::config）→ 复用 q2
//! 热重载管线全 surface 传播。插件连接死亡/禁用 → 删层重载，回 ODP/
//! 用户主题基线。
//!
//! # spawn：协议面保留、宿主不接线（防镀金）
//!
//! q3 验收点名接线的是 hit/layer/input/theme.set；`spawn.*` 消息解码合法、
//! 宿主记日志忽略。
//!
//! # pane 适配器
//!
//! 活面的 pane/前台 pid/cwd 变了才广播 `pane.snapshot`（对照 Orca：
//! 身份事件 + 退出时再推一次，不按秒扫）。槽位与 `window-save-state`
//! 恢复顺序一致。插件回 `pane.input` 时经 `ghostty_surface_text` 写入
//! 对应 PTY。找不到 pane 则忽略、不断连。
//!
//! # 超时纪律
//!
//! 同步短超时，绝不卡死主 runloop：claim 汇集 [`HIT_REPLY_TIMEOUT`]
//! （500ms）、层握手 [`LAYER_HANDSHAKE_TIMEOUT`]（1.5s，只在有人认领时
//! 进入）、冷启动 connect（2s，只发生在首击兜底）。层打开/主题覆盖/
//! 有插件连接期间主 runloop 挂 150ms 泵 timer（空载零开销）。

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ninja_protocol::frame::{FrameDecoder, encode_frame};
use ninja_protocol::{
    Hit, HitKind, InputFocus, InputHotkey, InputHotkeyDenied, InputHotkeyGranted,
    InputKey, InputMouse, InputScroll, LayerClose, LayerMsg, LayerOpen, LayerReady, Message,
    Modifier, MouseAction, MouseButton, PaneInfo, PaneInput, PaneSnapshot, Placement, Surface,
    ThemeSet,
};

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class};
use objc2_core_foundation::{
    CFBoolean, CFData, CFDictionary, CFNumber, CFNumberType, CFRetained,
};
use objc2_core_graphics::{
    CGColorRenderingIntent, CGDataProvider, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo,
};
use objc2_app_kit::{NSEvent, NSView, NSWindow};
use objc2_foundation::{NSObject, NSPoint, NSRect, NSSize, NSString};
use objc2_io_surface::IOSurfaceRef;
use objc2_web_kit::{
    WKScriptMessage, WKScriptMessageHandler, WKUserContentController, WKWebView,
    WKWebViewConfiguration,
};

use crate::keymap;
use crate::surface::SurfaceHostView;

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

/// 监督器视角的 `[plugins]` 配置（q2 起在 crate::config 解析；这里换
/// HashMap 便于按名解析二进制）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginsConfig {
    /// 启用的插件名列表。空 = 关（空载门禁）。启用即拉起。
    pub enabled: Vec<String>,
    /// 插件名 → 二进制路径（缺省时按名多段解析，见 [`resolve_plugin_binary`]）。
    pub paths: std::collections::HashMap<String, String>,
}

impl From<&crate::config::PluginsConfig> for PluginsConfig {
    fn from(c: &crate::config::PluginsConfig) -> Self {
        Self {
            enabled: c.enabled.clone(),
            paths: c.paths.iter().cloned().collect(),
        }
    }
}

/// 一个插件在面板/测试眼里的状态快照（[`status_snapshot`]）。
/// 「运行中」按宿主拉起的子进程判（try_wait 未退出）；内存是子进程
/// 真实物理足迹（`proc_pid_rusage` 的 ri_phys_footprint）。
#[derive(Clone, Debug, PartialEq)]
pub struct PluginStatus {
    pub name: String,
    /// 在会话 enabled 名单里（面板开关的「开」）。
    pub enabled: bool,
    /// 子进程活着。
    pub running: bool,
    pub pid: Option<u32>,
    /// 物理足迹字节；进程不在 → None。
    pub memory_bytes: Option<u64>,
    /// 最后一次失败原因（拉起失败/异常退出）；正常在跑 → None。
    pub last_error: Option<String>,
}

// ---------------------------------------------------------------------------
// socket 约定与解析
// ---------------------------------------------------------------------------

/// socket 路径约定：`${TMPDIR:-/tmp}/ninja-ade-{pid}.sock`。
pub fn socket_path() -> PathBuf {
    let pid = std::process::id();
    std::env::temp_dir().join(format!("ninja-ade-{pid}.sock"))
}

/// 实际生效路径：`NINJA_ADE_SOCK` 覆盖（拉起插件进程时经同名环境变量
/// 告知路径；测试钩子同途）。
fn effective_socket_path() -> PathBuf {
    match std::env::var_os("NINJA_ADE_SOCK") {
        Some(p) => PathBuf::from(p),
        None => socket_path(),
    }
}

/// 陈旧 socket 清扫：宿主 SIGKILL/崩溃时 [`PluginHost::shutdown`] 不跑，
/// 约定目录下会留下 `ninja-ade-<pid>.sock` 尸体。规则：文件名里的 pid
/// 已死（`kill(pid,0)`=ESRCH）才删；活 pid 一律不动（并行实例，或 pid
/// 被复用——保守不动）。只在启用插件启动时扫（[`start`]）：空载路径
/// 零文件系统改动。
pub fn sweep_stale_sockets() {
    sweep_stale_sockets_in(&std::env::temp_dir());
}

/// [`sweep_stale_sockets`] 的实现核心（目录可注入，单测用隔离目录）。
fn sweep_stale_sockets_in(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(pid_str) = name
            .strip_prefix("ninja-ade-")
            .and_then(|s| s.strip_suffix(".sock"))
        else {
            continue; // 非本约定的文件不碰
        };
        let Ok(pid) = pid_str.parse::<i32>() else {
            continue; // 名字里不是数字（垃圾名）：不碰
        };
        if pid <= 0 || pid == std::process::id() as i32 {
            continue; // 自己的路径由 bind 处置；非正数必是垃圾名
        }
        // kill(pid, 0)：0/EPERM = 有进程在（不动）；ESRCH = 进程已死。
        let alive = unsafe { libc::kill(pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if !alive {
            let _ = std::fs::remove_file(e.path());
            eprintln!(
                "ninja: 清扫陈旧 ADE socket {}（pid {pid} 已死）",
                e.path().display()
            );
        }
    }
}

/// 用户级插件目录：`~/.config/ninja/plugins`。`HOME` 缺失 → None：该
/// 搜索段整体跳过（其余段照常）。
pub fn user_plugin_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/ninja/plugins"))
}

/// 按名解析插件二进制：`[plugins.paths]` 显式路径 → `$NINJA_PLUGIN_DIR/
/// <name>` → `~/.config/ninja/plugins/<name>` → 宿主二进制同目录
/// `<name>`。都不存在 → None（调用方降级）。
pub fn resolve_plugin_binary(name: &str, cfg: &PluginsConfig) -> Option<PathBuf> {
    resolve_plugin_binary_in(name, cfg, user_plugin_dir().as_deref())
}

/// [`resolve_plugin_binary`] 的实现核心：用户插件目录可注入（单测用
/// 隔离目录，不碰真实 `~/.config`）。段次序见外层文档。
fn resolve_plugin_binary_in(
    name: &str,
    cfg: &PluginsConfig,
    user_dir: Option<&Path>,
) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') {
        return None; // 名字即文件系统注入向量：只收裸名
    }
    if let Some(p) = cfg.paths.get(name) {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
        eprintln!("ninja: plugins.paths.{name} = {} 不存在，跳过该路径", p.display());
    }
    if let Some(dir) = std::env::var_os("NINJA_PLUGIN_DIR") {
        let p = Path::new(&dir).join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(dir) = user_dir {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    // 宿主二进制同目录（开发布局：宿主与插件同置一个 target
    // 目录）。只探测存在性，不写。
    if let Ok(exe) = std::env::current_exe() {
        let p = exe.parent()?.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// env 门控的调度调试（stderr 一步一行；取证用，不设不打印）。
fn ade_debug(msg: &str) {
    if std::env::var_os("NINJA_ADE_DEBUG").is_some() {
        eprintln!("ninja[ade]: {msg}");
    }
}

// ---------------------------------------------------------------------------
// footprint（面板内存列；空载门禁采样器同款口径）
// ---------------------------------------------------------------------------

/// 子进程真实物理足迹（字节）：`proc_pid_rusage` 的 ri_phys_footprint
/// （与 footprint 工具同源；libSystem 自带，无新增链接面）。
///
/// **结构体尺寸坑（旧树实测 + 本机复测，必须照抄）**：内核按**当前**
/// flavor 的完整结构体写穿（v6 = 16B uuid + 31×u64 = 264B；本机内核
/// 实测写得更宽——264B 缓冲在退出期触发 stack-protector abort），固定
/// 给 512B 裕量；flavor 用 V4（只读前缀字段，偏移由 ABI 钉死）——
/// `ri_phys_footprint` 在公共前缀里（uuid[16] + 7×u64 之后，偏移 72）。
pub fn footprint_bytes(pid: u32) -> Option<u64> {
    const RI_PHYS_FOOTPRINT_OFF: usize = 16 + 7 * 8;
    let mut info = [0u8; 512];
    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut std::ffi::c_void) -> i32;
    }
    // RUSAGE_INFO_V4 = 4；只读前缀字段，偏移由 ABI 钉死。
    let r = unsafe { proc_pid_rusage(pid as i32, 4, info.as_mut_ptr() as *mut std::ffi::c_void) };
    (r == 0).then(|| {
        u64::from_le_bytes(
            info[RI_PHYS_FOOTPRINT_OFF..RI_PHYS_FOOTPRINT_OFF + 8]
                .try_into()
                .expect("常量切片恰 8 字节"),
        )
    })
}

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
    let mut col_now: u32 = 0;
    for (i, _c) in chars.iter().enumerate() {
        if col_now >= col {
            idx = Some(i);
            break;
        }
        col_now += 1;
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
fn cwd_for_view(view: &SurfaceHostView) -> String {
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

/// 当前所有 PTY 面：槽位顺序与 [`crate::session::save`] 一致（只计带
/// PaneContainer 的标签；预览 chrome 标签不占号）。
fn collect_pane_snapshot() -> PaneSnapshot {
    let mut panes = Vec::new();
    for (window_idx, group) in crate::session::tab_groups().into_iter().enumerate() {
        for (tab_idx, tw) in group.iter().enumerate() {
            let Some(c) = crate::pane::container_of(tw) else {
                continue;
            };
            for (leaf_idx, leaf) in c.leaves().iter().enumerate() {
                let Some(surface) = leaf.surface_opt() else {
                    continue;
                };
                let fg_pid = unsafe { ghostty_sys::ghostty_surface_foreground_pid(surface) } as u32;
                panes.push(PaneInfo::new(
                    leaf.pane_id(),
                    window_idx as u32,
                    tab_idx as u32,
                    leaf_idx as u32,
                    cwd_for_view(leaf),
                    fg_pid,
                ));
            }
        }
    }
    PaneSnapshot::new(panes)
}

/// 活面 pane/pid/cwd 签名。泵每拍只比这个；变了才走窗口遍历。
fn cheap_pane_sig() -> String {
    let mut parts = Vec::new();
    crate::host::visit_live_panes(|pane, pid, pwd| {
        parts.push((pane, pid, pwd.unwrap_or("").to_string()));
    });
    parts.sort_by_key(|p| p.0);
    let mut s = String::new();
    for (pane, pid, cwd) in parts {
        s.push_str(&pane.to_string());
        s.push(':');
        s.push_str(&pid.to_string());
        s.push(':');
        s.push_str(&cwd);
        s.push(';');
    }
    s
}

fn handle_pane_input(m: &PaneInput) {
    if m.text.is_empty() {
        return;
    }
    let Some(view) = crate::host::view_by_pane_id(m.pane) else {
        eprintln!("ninja: pane.input pane={} 找不到面，忽略", m.pane);
        return;
    };
    let Some(surface) = view.surface_opt() else {
        return;
    };
    unsafe {
        ghostty_sys::ghostty_surface_text(
            surface,
            m.text.as_ptr().cast(),
            m.text.len(),
        );
    }
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

/// 当前生效的插件主题覆盖（config.rs 装载管线消费；None = 无覆盖）。
/// 内容 = (色板名, 层文件文本)。拥有者连接死亡/禁用 →
/// [`revoke_theme_override`]。
pub fn plugin_theme_override() -> Option<(String, String)> {
    THEME_OVERRIDE
        .lock()
        .ok()
        .and_then(|s| s.as_ref().map(|(name, text, _)| (name.clone(), text.clone())))
}

fn theme_owner() -> Option<u64> {
    THEME_OVERRIDE.lock().ok().and_then(|s| s.as_ref().map(|(_, _, conn)| *conn))
}

/// 覆盖槽（主线程纪律；static 要求 Mutex）。
static THEME_OVERRIDE: Mutex<Option<(String, String, u64)>> = Mutex::new(None);

/// theme.set 处置入口（分发/泵/handshake 读窗共用）：色值语义坏 →
/// 警告 + 整条忽略（不断连）；有效 → 覆盖槽落地 + 起泵（盯连接死亡）
/// + 排期热重载（装载管线读覆盖槽，写层文件装载）。
fn handle_theme_set(m: &ThemeSet, conn_id: u64) {
    match theme_conf_text(m) {
        Some(text) => {
            if let Ok(mut slot) = THEME_OVERRIDE.lock() {
                *slot = Some((m.name.clone(), text, conn_id));
            }
            eprintln!("ninja: 主题插件已换色板 {:?}（conn {conn_id}）", m.name);
            ensure_pump_timer();
            crate::host::schedule_reload("theme.set");
        }
        None => {
            eprintln!(
                "ninja: theme.set 色板无效（conn {conn_id}，name={:?}），整条忽略",
                m.name
            );
        }
    }
}

/// 撤销主题覆盖（连接死亡/禁用）。返回是否有覆盖被撤（调用方决定是否
/// 排期重载）。
fn revoke_theme_override() -> bool {
    THEME_OVERRIDE
        .lock()
        .map(|mut s| s.take().is_some())
        .unwrap_or(false)
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
fn hotkey_to_key_event(key: &str, mods: &[Modifier]) -> Option<ghostty_sys::ghostty_input_key_s> {
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

// ---------------------------------------------------------------------------
// PluginHost（监督器本体）
// ---------------------------------------------------------------------------

/// 已绑定的 ADE socket 句柄。[`shutdown`]（幂等）：收层、断连接、收割
/// 子进程、删 socket 文件——正常退出与同会话禁用走同一通路。
#[derive(Debug)]
pub struct PluginHost {
    listener: UnixListener,
    path: PathBuf,
    /// 已连上的插件连接（分发/泵时按需 accept 进来）。每条连接各带
    /// 一个帧解码器（半帧状态跨读保留）。
    conns: Vec<Conn>,
    /// hit id 发号器（回执配对用；从 1 起）。
    next_hit_id: u64,
    /// conn id 发号器（层/热键/主题的回程路由用）。
    next_conn_id: u64,
    /// 已拉起（或已放弃）的插件名。「别再试」语义——外部死亡/拉起失败
    /// 不自动重拉；面板再启用时显式清除重试（[`PluginHost::session_enable`]）。
    spawned: std::collections::BTreeSet<String>,
    /// 拉起的插件进程（面板按名对应 pid/内存；宿主退出时它们也会因
    /// socket EOF 自退）。
    children: Vec<(String, std::process::Child)>,
    /// 拉起失败/异常退出的最后原因（面板「最后错误」列）。
    spawn_errors: std::collections::BTreeMap<String, String>,
    /// 配置快照（按名解析二进制 + 会话 enabled 名单真值）。
    cfg: PluginsConfig,
    /// 已禁用。置位后分发/泵/accept 全部空转，行为等同未启用。
    disabled: bool,
    /// 已授予的热键。
    hotkeys: Vec<HotkeyGrant>,
    /// 上次拉起时插件二进制 mtime。监视拍比对，变了就热重载。
    bin_mtime: std::collections::BTreeMap<String, Option<std::time::SystemTime>>,
    /// 上次广播时的活面签名（pane:pid:cwd）。变了才发 pane.snapshot。
    last_pane_sig: Option<String>,
}

/// 一条已授予的热键。
#[derive(Clone, Debug, PartialEq)]
struct HotkeyGrant {
    conn: u64,
    key: String,
    modifiers: Vec<Modifier>,
}

impl HotkeyGrant {
    fn matches(&self, key: &str, mods: &[Modifier]) -> bool {
        // 修饰集无序比较。
        self.key == key
            && self.modifiers.len() == mods.len()
            && self.modifiers.iter().all(|m| mods.contains(m))
    }
}

#[derive(Debug)]
struct Conn {
    id: u64,
    stream: UnixStream,
    decoder: FrameDecoder,
}

/// 命中分发的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// 没有任何插件连着（或插件未启用）→ 系统默认打开。
    NoPlugins,
    /// 有插件认领（priority 大者胜；平局先连者胜）。
    Claimed { priority: u32 },
    /// 全部回 ignore（或静默/断连降级）→ 系统默认打开。
    AllIgnored,
}

/// claim/ignore 汇集的同步超时预算（点击手势路径上的一次性开销；
/// 超时 = ignore 降级，永不卡死 runloop）。
pub const HIT_REPLY_TIMEOUT: Duration = Duration::from_millis(500);

/// 冷启动（spawn→connect）预算：与回执预算解耦——只约束「等插件进程
/// 连上」。release 二进制 spawn+connect 通常 <50ms；debug 构建/系统
/// 繁忙时可达数百毫秒，太紧会让首击随机降级。
const COLD_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// claim 后层握手（open→ready→present）的同步预算。只在认领方要层的
/// 路径上花；预算耗尽 = 放弃等 present（层仍开着，靠泵 timer 兜）。
pub const LAYER_HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(1500);

/// 层打开/主题覆盖期间插件连接的轮询周期（主 runloop timer；无层无
/// 覆盖时不存在）。
const PUMP_INTERVAL: f64 = 0.15;

impl PluginHost {
    /// 唯一入口：按配置决定绑不绑 socket。
    ///
    /// - `enabled` 为空 → `None`：**不建 socket、不碰文件系统、不拉
    ///   进程**（空载不变量；也不扫陈旧 socket——空载路径零改动）。
    /// - 非空 → 清扫陈旧 socket → 绑定 + listen（非阻塞）；绑定失败
    ///   不炸终端：stderr 警告 + `None`（降级为插件禁用）。
    pub fn start(cfg: &PluginsConfig) -> Option<PluginHost> {
        if cfg.enabled.is_empty() {
            return None;
        }
        sweep_stale_sockets();
        Self::bind(effective_socket_path(), cfg.clone())
    }

    /// 在给定路径上绑定（start 的实现核心；测试用隔离目录直调）。
    fn bind(path: PathBuf, cfg: PluginsConfig) -> Option<PluginHost> {
        // 极端场景：同 pid 复用留下陈旧文件。先清再绑。
        let _ = std::fs::remove_file(&path);
        match UnixListener::bind(&path) {
            Ok(listener) => {
                // 非阻塞 accept：分发/泵路径按需收，无任何路径卡 runloop。
                if let Err(e) = listener.set_nonblocking(true) {
                    eprintln!("ninja: ADE socket 设非阻塞失败（{e}），插件禁用");
                    let _ = std::fs::remove_file(&path);
                    return None;
                }
                Some(PluginHost {
                    listener,
                    path,
                    conns: Vec::new(),
                    next_hit_id: 0,
                    next_conn_id: 0,
                    spawned: std::collections::BTreeSet::new(),
                    children: Vec::new(),
                    spawn_errors: std::collections::BTreeMap::new(),
                    cfg,
                    disabled: false,
                    hotkeys: Vec::new(),
                    bin_mtime: std::collections::BTreeMap::new(),
                    last_pane_sig: None,
                })
            }
            Err(e) => {
                eprintln!("ninja: ADE socket {path:?} 绑定失败（{e}），插件禁用");
                None
            }
        }
    }

    /// 已绑定的路径（取证/日志用）。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 配置快照（会话真值：面板开关已反映进去）。
    pub fn cfg(&self) -> &PluginsConfig {
        &self.cfg
    }

    // ------------------------------------------------------------------
    // 拉起（单一策略：启用即拉起）
    // ------------------------------------------------------------------

    /// 拉起单个插件（解析二进制 → spawn → 登记）。幂等性由调用方
    /// （spawned 集）保证。
    fn spawn_one(&mut self, name: &str) {
        let Some(bin) = resolve_plugin_binary(name, &self.cfg) else {
            eprintln!(
                "ninja: 插件 {name:?} 找不到二进制（[plugins.paths] / NINJA_PLUGIN_DIR / ~/.config/ninja/plugins / 宿主同目录），本次降级为未启用"
            );
            self.spawn_errors
                .insert(name.to_string(), "找不到二进制".into());
            return;
        };
        match std::process::Command::new(&bin)
            .env("NINJA_ADE_SOCK", &self.path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
        {
            Ok(child) => {
                eprintln!(
                    "ninja: 已拉起插件 {name:?}（pid {}，socket {:?}）",
                    child.id(),
                    self.path
                );
                self.spawn_errors.remove(name);
                self.bin_mtime
                    .insert(name.to_string(), std::fs::metadata(&bin).and_then(|m| m.modified()).ok());
                self.children.push((name.to_string(), child));
            }
            Err(e) => {
                eprintln!("ninja: 插件 {name:?}（{}）拉起失败：{e}", bin.display());
                self.spawn_errors
                    .insert(name.to_string(), format!("拉起失败：{e}"));
            }
        }
    }

    /// **启用即拉起**：拉起全部 enabled 且尚未尝试过的插件。宿主启动
    /// （runloop 就绪后，[`spawn_startup_plugins`]）、面板开
    /// （[`PluginHost::session_enable`]）都汇聚到这里。拉起后开一个
    /// 「等首个连接」窗口（[`SPAWN_CONNECT_WINDOW`]）钉住泵 timer：
    /// 插件 connect + 连接即推的 theme.set 靠泵消化。
    pub fn spawn_enabled_now(&mut self) {
        if self.disabled {
            return;
        }
        let mut spawned_any = false;
        for name in self.cfg.enabled.clone() {
            if !self.spawned.insert(name.clone()) {
                continue; // 已试过（成功或失败都不自动重拉）
            }
            self.spawn_one(&name);
            spawned_any = true;
        }
        if spawned_any {
            spawn_pending_arm();
            ensure_pump_timer();
        }
    }

    /// 面板开关「开」：名字进会话 enabled 名单 + 立即拉起（显式清除
    /// 「别再试」标记 → 之前拉不起/被杀的插件可以重试）。名字卫生同
    /// [`resolve_plugin_binary`]（只收裸名）。返回 false = 已禁用/名字
    /// 非法（面板回弹开关）。
    pub fn session_enable(&mut self, name: &str) -> bool {
        if self.disabled || name.is_empty() || name.contains('/') {
            return false;
        }
        if !self.cfg.enabled.iter().any(|n| n == name) {
            self.cfg.enabled.push(name.to_string());
        }
        self.spawned.remove(name); // 面板显式操作：重置重试标记
        self.spawn_one(name);
        spawn_pending_arm();
        ensure_pump_timer();
        true
    }

    /// 面板开关「关」：名字出会话 enabled 名单 + 立即杀它名下的子进程
    /// + 排干 EOF（收层/回退色板与插件死亡同一条通路：pump 摄连接
    /// EOF → [`PluginHost::drop_conn`]）。名单清空 = 整个插件面关掉
    /// （[`PluginHost::shutdown`]：删 socket，回到空载形态）。
    pub fn session_disable(&mut self, name: &str) {
        self.cfg.enabled.retain(|n| n != name);
        let mut killed = false;
        let mut i = 0;
        while i < self.children.len() {
            if self.children[i].0 == name {
                let (_, mut c) = self.children.remove(i);
                let _ = c.kill();
                let _ = c.wait();
                killed = true;
            } else {
                i += 1;
            }
        }
        if killed {
            // 同步排干死亡连接的 EOF：层/色板覆盖当场回收，不等下一拍。
            self.pump_plugins();
        }
        if self.cfg.enabled.is_empty() {
            self.shutdown(); // 名单空 = 零插件：socket 删除，回空载
        }
    }

    /// 二进制 mtime 变了：杀掉旧进程再拉起（不改 enabled 名单）。
    fn restart_plugin(&mut self, name: &str) {
        if self.disabled {
            return;
        }
        let mut i = 0;
        while i < self.children.len() {
            if self.children[i].0 == name {
                let (_, mut c) = self.children.remove(i);
                let _ = c.kill();
                let _ = c.wait();
            } else {
                i += 1;
            }
        }
        self.pump_plugins();
        self.spawn_one(name);
        spawn_pending_arm();
        ensure_pump_timer();
    }

    fn respawn_stale_plugins(&mut self) {
        if self.disabled {
            return;
        }
        let names = self.cfg.enabled.clone();
        for name in names {
            let Some(bin) = resolve_plugin_binary(&name, &self.cfg) else {
                continue;
            };
            let Ok(mt) = std::fs::metadata(&bin).and_then(|m| m.modified()) else {
                continue;
            };
            if mt.elapsed().map(|d| d.as_millis() < 300).unwrap_or(false) {
                continue;
            }
            match self.bin_mtime.get(&name).copied().flatten() {
                Some(prev) if prev == mt => continue,
                None => {
                    self.bin_mtime.insert(name.clone(), Some(mt));
                    continue;
                }
                Some(_) => {}
            }
            eprintln!("ninja: 插件 {name:?} 二进制已更新，热重载");
            self.restart_plugin(&name);
        }
    }

    /// 状态快照（面板/测试）：enabled 名单 ∪ 有子进程 ∪ 有错误记录的
    /// 名字，逐名报告 启用/在跑/pid/内存/最后错误。顺带收割已退出的
    /// 子进程（try_wait）并把异常退出记进 last_error。
    pub fn snapshot(&mut self) -> Vec<PluginStatus> {
        let mut i = 0;
        while i < self.children.len() {
            match self.children[i].1.try_wait() {
                Ok(Some(st)) => {
                    let (name, _) = self.children.remove(i);
                    if !st.success() {
                        self.spawn_errors
                            .insert(name, format!("已退出（code {}）", st.code().unwrap_or(-1)));
                    }
                }
                Ok(None) => i += 1,
                Err(_) => i += 1, // wait 错误：当还活着（下拍再试）
            }
        }
        let mut names: std::collections::BTreeSet<String> =
            self.cfg.enabled.iter().cloned().collect();
        for (n, _) in &self.children {
            names.insert(n.clone());
        }
        names.extend(self.spawn_errors.keys().cloned());
        names
            .into_iter()
            .map(|name| {
                let child = self
                    .children
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, c)| c);
                let running = child.is_some();
                let pid = child.map(|c| c.id());
                let memory_bytes = pid.and_then(footprint_bytes);
                PluginStatus {
                    enabled: self.cfg.enabled.contains(&name),
                    running,
                    pid,
                    memory_bytes: if running { memory_bytes } else { None },
                    last_error: if running {
                        None
                    } else {
                        self.spawn_errors.get(&name).cloned()
                    },
                    name,
                }
            })
            .collect()
    }

    // ------------------------------------------------------------------
    // hit 分发 + 层握手
    // ------------------------------------------------------------------

    /// 发下一个 hit id（回执配对用）。点击路径主线程调用。
    pub fn next_hit_id(&mut self) -> u64 {
        self.next_hit_id = self.next_hit_id.saturating_add(1);
        self.next_hit_id
    }

    /// 把 hit 广播给所有已连插件，收集 claim/ignore，仲裁出结果；
    /// 有人认领时继续层握手（open→ready→present）。
    pub fn dispatch_hit(&mut self, hit: &Hit, geom: Option<&LayerGeom>) -> DispatchOutcome {
        self.dispatch_hit_with_timeout(hit, HIT_REPLY_TIMEOUT, geom)
    }

    /// 按需非阻塞 accept：把内核 backlog 里排队的插件连接收进来。
    /// 不新增线程；没连接就是空操作。已禁用时不再收新连接。
    fn pump_accept(&mut self) {
        if self.disabled {
            return;
        }
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    // 分发路径用阻塞读 + 读超时（收口在超时预算内）。
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(HIT_REPLY_TIMEOUT));
                    self.next_conn_id += 1;
                    ade_debug(&format!("插件连接 conn={} 进来", self.next_conn_id));
                    self.conns.push(Conn {
                        id: self.next_conn_id,
                        stream,
                        decoder: FrameDecoder::new(),
                    });
                    // 新连接立刻推一份快照（agent-restore 靠它恢复）。
                    self.last_pane_sig = None;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break, // 监听器异常：本轮不再收，下次分发再试
            }
        }
        // 注意：这里**不**按「有连接」关等待窗——多插件会话里第一个连上
        // 就关窗，较慢的插件连接会被泵的停转条件卡死在 backlog 里
        //（theme.set 直到首次点击才被消化，实测踩过）。窗口只按时间
        // 过期（5s；拉不起/挂死的插件不拖住空转红线）。
    }

    /// dispatch_hit 的实现核心（超时可注入，单测用短预算）。
    ///
    /// 流程：accept 排队连接 →（无连接时）首击冷启动兜底 → 广播 hit 帧
    /// → 逐连接收回执（共享 deadline；静默/断连/坏消息一律 ignore，坏
    /// 协议断开连接）→ 仲裁（claim 的 priority 最大者胜，平局先连者胜）
    /// → 认领方层握手。
    pub(crate) fn dispatch_hit_with_timeout(
        &mut self,
        hit: &Hit,
        timeout: Duration,
        geom: Option<&LayerGeom>,
    ) -> DispatchOutcome {
        if self.disabled {
            return DispatchOutcome::NoPlugins; // 已禁用 → 系统默认打开
        }
        self.pump_accept();
        if self.conns.is_empty() {
            // 兜底冷启动（常规路径已不依赖：宿主启动/面板开就拉过）。
            let can_spawn = self
                .cfg
                .enabled
                .iter()
                .any(|n| !self.spawned.contains(n));
            if !can_spawn {
                return DispatchOutcome::NoPlugins;
            }
            ade_debug("dispatch: 无连接，冷启动兜底拉插件");
            let t_spawn = Instant::now();
            for name in self.cfg.enabled.clone() {
                if self.spawned.insert(name.clone()) {
                    self.spawn_one(&name);
                }
            }
            let connect_deadline = Instant::now() + COLD_CONNECT_TIMEOUT.min(timeout);
            while self.conns.is_empty() && Instant::now() < connect_deadline {
                std::thread::sleep(Duration::from_millis(10));
                self.pump_accept();
            }
            ade_debug(&format!(
                "dispatch: 冷启动等待 {:?}，连接数 {}",
                t_spawn.elapsed(),
                self.conns.len()
            ));
            if self.conns.is_empty() {
                return DispatchOutcome::NoPlugins;
            }
        }
        // 回执预算从广播后起算（冷启动等待不侵占 500ms 回执窗口）。
        let deadline = Instant::now() + timeout;

        // 写阶段：广播 hit 帧。写失败（断连/缓冲满）→ 摘连接，视为 ignore。
        let frame = match encode_frame(&Message::Hit(hit.clone())) {
            Ok(f) => f,
            Err(_) => return DispatchOutcome::AllIgnored, // 不可能：new() 钉 v
        };
        let mut broken = Vec::new();
        for (i, c) in self.conns.iter_mut().enumerate() {
            if c.stream.write_all(&frame).is_err() {
                broken.push(i);
            }
        }
        for i in broken.iter().rev() {
            self.drop_conn(*i); // 断连 = 无主层一并回收
        }
        if self.conns.is_empty() {
            return DispatchOutcome::AllIgnored; // 广播全失败 = 无认领
        }

        // 收阶段：共享 deadline，逐连接收；responded 后不再读它。
        // 认领者按**连接 id** 记（下方会摘除断连，数组下标不稳）。
        let mut best: Option<(u32, u64)> = None; // (priority, conn id)
        let mut responded = vec![false; self.conns.len()];
        let mut dead: Vec<usize> = Vec::new();
        let mut deferred: Vec<(Message, u64)> = Vec::new();
        let mut buf = [0u8; 4096];
        for (i, c) in self.conns.iter_mut().enumerate() {
            let Some(rem) = deadline.checked_duration_since(Instant::now()) else {
                break; // 预算耗尽：未回执的连接一律按 ignore 降级
            };
            if c.stream.set_read_timeout(Some(rem)).is_err() {
                dead.push(i);
                responded[i] = true;
                continue;
            }
            loop {
                match c.stream.read(&mut buf) {
                    Ok(0) => {
                        dead.push(i); // 对端关连接：不认领
                        responded[i] = true;
                        break;
                    }
                    Ok(n) => {
                        if c.decoder.extend(&buf[..n]).is_err() {
                            dead.push(i);
                            responded[i] = true;
                            break;
                        }
                        while let Some(payload) = c.decoder.pop() {
                            match payload {
                                Err(_) => {
                                    dead.push(i); // 帧级违规：断开
                                    responded[i] = true;
                                }
                                Ok(p) => match Message::decode_host_frame(&p) {
                                    Ok(None) => {
                                        ade_debug("忽略未知 type（插件可比宿主新）");
                                    }
                                    Ok(Some(Message::HitClaim(m))) if m.id == hit.id => {
                                        if best.is_none_or(|(pr, _)| m.priority > pr) {
                                            best = Some((m.priority, c.id));
                                        }
                                        responded[i] = true;
                                    }
                                    Ok(Some(Message::HitIgnore(m))) if m.id == hit.id => {
                                        responded[i] = true;
                                    }
                                    Ok(Some(other)) => {
                                        // 回执窗口内顺带消化 theme.set/hotkey
                                        //（借用期先存，循环外统一处置）。
                                        deferred.push((other, c.id));
                                    }
                                    Err(e) => {
                                        ade_debug(&format!("坏协议断连：{e}"));
                                        dead.push(i);
                                        responded[i] = true;
                                    }
                                },
                            }
                            if responded[i] {
                                break;
                            }
                        }
                        if responded[i] {
                            break;
                        }
                    }
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        // 静默：预算内没等到回执 → ignore（连接保留）。
                        responded[i] = true;
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        dead.push(i);
                        responded[i] = true;
                        break;
                    }
                }
            }
        }
        for i in dead.iter().rev() {
            self.drop_conn(*i); // 断连/坏协议 = 无主层一并回收
        }
        for (msg, conn) in std::mem::take(&mut deferred) {
            self.handle_async_message(&msg, conn);
        }
        let Some((priority, claim_conn)) = best else {
            ade_debug("dispatch: 全 ignore/静默");
            return DispatchOutcome::AllIgnored;
        };
        ade_debug(&format!("dispatch: claim priority={priority} conn={claim_conn}"));
        // 层握手：认领方在同一连接上要层。geom 为 None（无渲染上下文，
        // 如单测）时跳过——认领仍然成立，只是宿主不处理层。
        if let Some(geom) = geom
            && let Some(idx) = self.conns.iter().position(|c| c.id == claim_conn)
        {
            self.layer_handshake(idx, geom, LAYER_HANDSHAKE_TIMEOUT);
        }
        DispatchOutcome::Claimed { priority }
    }

    /// claim 后的层握手：读认领方连接直到 present/close/断连/预算尽。
    /// `layer.open` → 建 IOSurface 回 `layer.ready`；`layer.present` →
    /// 合成；`layer.close` → 摘层。
    fn layer_handshake(&mut self, conn_idx: usize, geom: &LayerGeom, budget: Duration) {
        let deadline = Instant::now() + budget;
        let conn_id = self.conns[conn_idx].id;
        let mut buf = [0u8; 8192];
        loop {
            // 1) 先消化解码器里**已缓冲**的帧——claim 与 layer.open 常
            //    在同一个读块到达（分发阶段只弹到回执就停），不先弹会
            //    在等新字节上白耗整个预算（旧树实测过的竞态）。
            let mut quit = false;
            let mut dead = false;
            while let Some(conn) = self.conns.get_mut(conn_idx)
                && let Some(payload) = conn.decoder.pop()
            {
                match self.handshake_frame(payload, conn_idx, conn_id, geom) {
                    HandshakeStep::Continue => {}
                    HandshakeStep::Presented => {
                        quit = true;
                        break;
                    }
                    HandshakeStep::Dead => {
                        dead = true;
                        break;
                    }
                }
            }
            if dead {
                self.drop_conn(conn_idx);
                return;
            }
            if quit {
                return;
            }
            // 2) 解码器空了才阻塞读（预算内）。
            let Some(rem) = deadline.checked_duration_since(Instant::now()) else {
                break; // 预算尽：层可能仍开着（等 present），泵兜底
            };
            if self.conns[conn_idx].stream.set_read_timeout(Some(rem)).is_err() {
                self.drop_conn(conn_idx);
                return;
            }
            let n = match self.conns[conn_idx].stream.read(&mut buf) {
                Ok(0) => {
                    self.drop_conn(conn_idx); // 插件退了：收它的层
                    return;
                }
                Ok(n) => n,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    break // 静默超预算：不再等
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.drop_conn(conn_idx);
                    return;
                }
            };
            if self.conns[conn_idx].decoder.extend(&buf[..n]).is_err() {
                self.drop_conn(conn_idx);
                return;
            }
        }
    }

    /// 握手期单帧处置。
    fn handshake_frame(
        &mut self,
        payload: Result<Vec<u8>, ninja_protocol::FrameError>,
        conn_idx: usize,
        conn_id: u64,
        geom: &LayerGeom,
    ) -> HandshakeStep {
        let payload = match payload {
            Ok(p) => p,
            Err(_) => return HandshakeStep::Dead,
        };
        match Message::decode_host_frame(&payload) {
            Ok(None) => HandshakeStep::Continue,
            Ok(Some(Message::LayerOpen(m))) => {
                let html = m.surface == Surface::Html;
                match layer_open(geom, &m, conn_id) {
                    Some(ready) => {
                        let f = encode_frame(&Message::LayerReady(ready)).expect("LayerReady 编码");
                        if self.conns[conn_idx].stream.write_all(&f).is_err() {
                            HandshakeStep::Dead
                        } else if html {
                            // html 表面：建 WKWebView 可能重入 runloop。不要握着
                            // PluginHost 锁再等 layer.html，否则泵/监视 try 同一把锁会卡死主线程。
                            ensure_pump_timer();
                            HandshakeStep::Presented
                        } else {
                            HandshakeStep::Continue
                        }
                    }
                    None => {
                        eprintln!("ninja: 层分配失败（IOSurface/视图），拒层");
                        let f = encode_frame(&Message::LayerClose(LayerClose::new(0))).expect("编码");
                        let _ = self.conns[conn_idx].stream.write_all(&f);
                        HandshakeStep::Continue
                    }
                }
            }
            Ok(Some(Message::LayerPresent(m))) => {
                layer_present(m.layer);
                ensure_pump_timer();
                HandshakeStep::Presented
            }
            Ok(Some(Message::LayerHtml(m))) => {
                layer_load_html(m.layer, &m.html);
                ensure_pump_timer();
                HandshakeStep::Presented
            }
            Ok(Some(Message::LayerClose(m))) => {
                layer_close(m.layer);
                stop_pump_timer_if_idle();
                HandshakeStep::Continue
            }
            Ok(Some(other)) => {
                // 握手期也可推色板/热键（认领型插件顺带换色）。
                self.handle_async_message(&other, conn_id);
                HandshakeStep::Continue
            }
            Err(_) => HandshakeStep::Dead, // 坏协议：断
        }
    }

    /// 回执/握手窗口外的插件消息（泵与回执窗口共用）：theme.set 应用、
    /// input.hotkey 授予/拒绝、layer.close 摘层；其余（spawn.*：协议面
    /// 保留，宿主不接线）记 debug 忽略。
    fn handle_async_message(&mut self, msg: &Message, conn_id: u64) {
        match msg {
            Message::ThemeSet(m) => handle_theme_set(m, conn_id),
            Message::InputHotkey(m) => {
                let reply = self.hotkey_decide(m, conn_id);
                if let Some(c) = self.conns.iter_mut().find(|c| c.id == conn_id) {
                    let _ = c.stream.write_all(&encode_frame(&reply).expect("hotkey 回执编码"));
                }
            }
            Message::LayerClose(m) => {
                layer_close(m.layer);
                stop_pump_timer_if_idle();
            }
            Message::LayerHtml(m) => layer_load_html(m.layer, &m.html),
            Message::LayerMsg(m) => layer_post_msg(m.layer, &m.name, &m.body),
            Message::SpawnRequest(m) => {
                ade_debug(&format!(
                    "spawn.request id={} argv={:?}：协议面保留，q3 宿主不接线（忽略）",
                    m.id, m.argv
                ));
            }
            Message::PaneInput(m) => handle_pane_input(m),
            _ => {}
        }
    }

    /// input.hotkey 仲裁：对 ghostty 生效键位（`config_key_is_binding`）
    /// 与已授予的其他插件查冲突。
    fn hotkey_decide(&mut self, m: &InputHotkey, conn_id: u64) -> Message {
        let id = m.id;
        if key_name_to_code(&m.key).is_none() {
            return Message::InputHotkeyDenied(InputHotkeyDenied::new(id, "未知键名"));
        }
        // 已授予的其他插件占着同键 → 拒。
        for g in &self.hotkeys {
            if g.conn != conn_id && g.matches(&m.key, &m.modifiers) {
                return Message::InputHotkeyDenied(InputHotkeyDenied::new(
                    id,
                    "已被另一个插件占用",
                ));
            }
        }
        // 对 ghostty 键位系统冲突 → 拒（ghostty 绑定优先是宿主纪律）。
        if let Some(cfg) = crate::host::config()
            && let Some(key) = hotkey_to_key_event(&m.key, &m.modifiers)
            && unsafe { ghostty_sys::ghostty_config_key_is_binding(cfg, key) }
        {
            return Message::InputHotkeyDenied(InputHotkeyDenied::new(
                id,
                "与 ghostty 键位绑定冲突",
            ));
        }
        self.hotkeys
            .retain(|g| !(g.conn == conn_id && g.matches(&m.key, &m.modifiers)));
        self.hotkeys.push(HotkeyGrant {
            conn: conn_id,
            key: m.key.clone(),
            modifiers: m.modifiers.clone(),
        });
        Message::InputHotkeyGranted(InputHotkeyGranted::new(id))
    }

    /// 泵：层打开/主题覆盖期间轮询所有连接，消化插件异步消息
    /// （present 重合成 / close 摘层 / EOF 收层）。主 runloop timer 调用。
    pub fn pump_plugins(&mut self) {
        self.pump_accept();
        let mut buf = [0u8; 8192];
        let mut i = 0;
        while i < self.conns.len() {
            let conn = &mut self.conns[i];
            let conn_id = conn.id;
            if conn.stream.set_read_timeout(Some(Duration::from_millis(1))).is_err() {
                self.drop_conn(i);
                continue;
            }
            match conn.stream.read(&mut buf) {
                Ok(0) => {
                    self.drop_conn(i); // 插件退了，收它的层
                    continue;
                }
                Ok(n) => {
                    if conn.decoder.extend(&buf[..n]).is_err() {
                        self.drop_conn(i);
                        continue;
                    }
                    let mut dead = false;
                    let mut deferred: Vec<(Message, u64)> = Vec::new();
                    while let Some(payload) = conn.decoder.pop() {
                        match payload {
                            Err(_) => dead = true,
                            Ok(p) => match Message::decode_host_frame(&p) {
                                Ok(None) => {}
                                Ok(Some(Message::LayerPresent(m))) => {
                                    layer_present(m.layer);
                                }
                                Ok(Some(Message::LayerClose(m))) => {
                                    layer_close(m.layer);
                                    stop_pump_timer_if_idle();
                                }
                                Ok(Some(other)) => {
                                    deferred.push((other, conn_id));
                                }
                                Err(_) => dead = true,
                            },
                        }
                        if dead {
                            break;
                        }
                    }
                    if dead {
                        self.drop_conn(i); // 坏协议断连，收它的层
                        continue;
                    }
                    for (msg, conn) in deferred {
                        self.handle_async_message(&msg, conn);
                    }
                    i += 1;
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    i += 1
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => {
                    self.drop_conn(i); // IO 错断连，收它的层
                    continue;
                }
            }
        }
        self.maybe_broadcast_pane_snapshot(false);
        if !any_layers() && self.conns.is_empty() {
            stop_pump_timer_if_idle();
        }
    }

    fn maybe_broadcast_pane_snapshot(&mut self, force: bool) {
        if self.disabled || self.conns.is_empty() {
            return;
        }
        let sig = cheap_pane_sig();
        if !force && self.last_pane_sig.as_deref() == Some(sig.as_str()) {
            return;
        }
        let snap = collect_pane_snapshot();
        let msg = Message::PaneSnapshot(snap);
        let Ok(frame) = encode_frame(&msg) else {
            return;
        };
        for c in &mut self.conns {
            let _ = c.stream.write_all(&frame);
        }
        self.last_pane_sig = Some(sig);
    }

    /// 连接死亡收口（EOF / IO 错 / 坏协议）：摘连接 + 收掉该连接拥有的
    /// 全部层（插件死了它的层就是无主陈旧 overlay：不摘则层永久残留且
    /// 泵 timer 永不停转）+ 撤销其热键 + 色板覆盖回退基线。
    fn drop_conn(&mut self, idx: usize) {
        let Some(c) = self.conns.get(idx) else {
            return;
        };
        let conn_id = c.id;
        self.conns.remove(idx);
        self.hotkeys.retain(|g| g.conn != conn_id);
        if layer_close_by_conn(conn_id) {
            ade_debug(&format!("conn {conn_id} 死亡：已回收其全部层"));
        }
        if theme_owner() == Some(conn_id) && revoke_theme_override() {
            eprintln!("ninja: 主题插件连接 {conn_id} 死亡，色板回退内置/用户基线");
            crate::host::schedule_reload("theme-revoke");
        }
        stop_pump_timer_if_idle();
    }

    /// 幂等关闭（同会话禁用；退出收口复用同一实现）。顺序敏感：
    /// 1. 撤销主题覆盖（色板回退基线）；
    /// 2. 收全部层并尽力通知还连着的拥有者 `layer.close`（插件好清
    ///    状态；已死连接的层一并回收）；
    /// 3. 无层即停泵 timer；
    /// 4. 断全部连接（插件侧读到 EOF 自退——正常路径零强杀）；
    /// 5. kill + wait 子进程（EOF 没退的兜底 + 收尸防僵尸）；
    /// 6. 删 socket 文件（文件消失 = 禁用完成的可观测信号）。
    pub fn shutdown(&mut self) {
        if self.disabled {
            return; // 幂等
        }
        // 对照 Orca quit-capture：断连前再推一次，并给插件一点时间落盘，
        // 再 SIGKILL（否则 EOF 还没读到就被杀掉，json 停在上一拍）。
        self.maybe_broadcast_pane_snapshot(true);
        self.disabled = true;
        if revoke_theme_override() {
            eprintln!("ninja: 插件禁用，色板回退内置/用户基线");
            crate::host::schedule_reload("plugins-disabled");
        }
        for (handle, conn) in layer_close_all() {
            let _ = self.send_message(conn, &Message::LayerClose(LayerClose::new(handle)));
        }
        stop_pump_timer_if_idle();
        self.conns.clear();
        std::thread::sleep(Duration::from_millis(80));
        for (_name, c) in self.children.iter_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.children.clear();
        spawn_pending_disarm();
        let _ = std::fs::remove_file(&self.path);
        eprintln!(
            "ninja: 插件已禁用（层已收、连接已断、子进程已收割、socket {:?} 已删）",
            self.path
        );
    }

}

/// 握手循环的单步结果。
enum HandshakeStep {
    Continue,
    Presented,
    Dead,
}

// ---------------------------------------------------------------------------
// 层注册表（q0 审计 #4 结构路线：宿主自持 NSView 之上叠 IOSurface 层）
// ---------------------------------------------------------------------------

/// 开层所需的几何（点击路径在主线程收集）。
pub struct LayerGeom {
    /// pane id（键盘路由 + 收层目标）。
    pub pane: u32,
    /// cell 尺寸（points）。
    pub cell_pt: (f64, f64),
    /// 视图尺寸（points）。
    pub view_pt: (f64, f64),
    /// 像素密度（backingScaleFactor；dpi = 72*scale 发给插件）。
    pub scale: f64,
    /// 宿主视图（层叠在其上；Retained 保活）。
    pub view: Retained<SurfaceHostView>,
}

struct LayerEntry {
    /// 协议层句柄（layer.ready 里发给插件的那个）。
    handle: u64,
    /// 拥有该层的 pane（tab 层用 0：不跟终端 pane 的 resize/Esc 走）。
    pane: u32,
    /// 像素层合成视图。html tab 为 None。
    view: Option<Retained<LayerView>>,
    /// 像素层共享 IOSurface。html tab 为 None。
    surface: Option<CFRetained<IOSurfaceRef>>,
    /// html 表面。
    web: Option<Retained<LayerWebView>>,
    /// 所属插件连接 id。
    conn: u64,
    /// tab 层持有 chrome 窗（关层时关窗；窗关时反手收层）。
    tab_window: Option<Retained<NSWindow>>,
    /// layer.open 的 id（resize 重发 layer.ready 时回同一条）。
    open_id: u64,
}

/// 层内容视图：layer-backed NSView，drawRect 把「最新一帧」CGImage 画满
/// bounds（AppKit 把绘制结果进 layer contents——比手设 contents 稳：
/// layer-backed 视图的 contents 由 AppKit 接管，手设会被清空，实测；
/// sublayer 挂 ghostty Metal 层的方案几何又不随 view 坐标系走，也弃）。
/// 帧图像来自 [`surface_to_image`]（present 时像素拷贝）。
#[allow(non_snake_case)]
pub struct LayerViewIvars {
    image: RefCell<Option<CFRetained<CGImage>>>,
    handle: Cell<u64>,
    conn: Cell<u64>,
    is_tab: Cell<bool>,
    presented: Cell<bool>,
}

define_class!(
    // SAFETY: NSView 子类化无强约束；drawRect/isFlipped 纯自算；ivars
    // 只经 RefCell 访问（主线程）。
    #[unsafe(super(objc2_app_kit::NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = LayerViewIvars]
    pub struct LayerView;

    impl LayerView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true // 与 SurfaceHostView 同系：左上原点
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            self.ivars().is_tab.get()
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let Some(nsctx) = objc2_app_kit::NSGraphicsContext::currentContext() else {
                return;
            };
            let ctx = nsctx.CGContext();
            let b = self.bounds();
            let rect = objc2_core_foundation::CGRect {
                origin: objc2_core_foundation::CGPoint::new(0.0, 0.0),
                size: objc2_core_foundation::CGSize::new(b.size.width, b.size.height),
            };
            objc2_core_graphics::CGContext::set_rgb_fill_color(
                Some(&ctx),
                40.0 / 255.0,
                44.0 / 255.0,
                52.0 / 255.0,
                1.0,
            );
            objc2_core_graphics::CGContext::fill_rect(Some(&ctx), rect);
            let Some(image) = self.ivars().image.borrow().clone() else {
                return;
            };
            // AppKit flipped 视图的原生 CG 仍是 y-up：先翻 CTM 再画 CGImage。
            objc2_core_graphics::CGContext::save_g_state(Some(&ctx));
            objc2_core_graphics::CGContext::translate_ctm(Some(&ctx), 0.0, rect.size.height);
            objc2_core_graphics::CGContext::scale_ctm(Some(&ctx), 1.0, -1.0);
            objc2_core_graphics::CGContext::draw_image(Some(&ctx), rect, Some(&image));
            objc2_core_graphics::CGContext::restore_g_state(Some(&ctx));
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            let ok: bool = unsafe { objc2::msg_send![super(self), becomeFirstResponder] };
            if ok {
                send_layer_focus(self.ivars().handle.get(), self.ivars().conn.get(), true);
            }
            ok
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            let ok: bool = unsafe { objc2::msg_send![super(self), resignFirstResponder] };
            if ok {
                send_layer_focus(self.ivars().handle.get(), self.ivars().conn.get(), false);
            }
            ok
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if !self.ivars().is_tab.get() {
                let _: () = unsafe { objc2::msg_send![super(self), keyDown: event] };
                return;
            }
            tab_key_down(self, event);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            send_layer_scroll(self, event);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            send_layer_mouse(self, event, MouseAction::Down);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            send_layer_mouse(self, event, MouseAction::Up);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            send_layer_mouse(self, event, MouseAction::Move);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            send_layer_mouse(self, event, MouseAction::Down);
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) {
            send_layer_mouse(self, event, MouseAction::Up);
        }

        #[unsafe(method(rightMouseDragged:))]
        fn right_mouse_dragged(&self, event: &NSEvent) {
            send_layer_mouse(self, event, MouseAction::Move);
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            send_layer_mouse(self, event, MouseAction::Down);
        }

        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, event: &NSEvent) {
            send_layer_mouse(self, event, MouseAction::Up);
        }

        #[unsafe(method(otherMouseDragged:))]
        fn other_mouse_dragged(&self, event: &NSEvent) {
            send_layer_mouse(self, event, MouseAction::Move);
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            let prev = self.frame().size;
            let _: () = unsafe { objc2::msg_send![super(self), setFrameSize: size] };
            if self.ivars().is_tab.get()
                && self.ivars().presented.get()
                && self.ivars().handle.get() != 0
                && !self.inLiveResize()
                && ((prev.width - size.width).abs() > 0.5 || (prev.height - size.height).abs() > 0.5)
            {
                rebuild_tab_layer(self.ivars().handle.get());
            }
        }

        #[unsafe(method(viewDidEndLiveResize))]
        fn view_did_end_live_resize(&self) {
            let _: () = unsafe { objc2::msg_send![super(self), viewDidEndLiveResize] };
            if self.ivars().is_tab.get()
                && self.ivars().presented.get()
                && self.ivars().handle.get() != 0
            {
                rebuild_tab_layer(self.ivars().handle.get());
            }
        }
    }
);

/// html 表面：WKWebView 加载插件 HTML。Esc / ⌘W 宿主关层（PRODUCT）。
/// 其它键留给 WebKit。JS 只能经 `webkit.messageHandlers.layer` 出站。
#[allow(non_snake_case)]
pub struct LayerWebIvars {
    handle: Cell<u64>,
    conn: Cell<u64>,
    handler: RefCell<Option<Retained<LayerMsgHandler>>>,
}

#[allow(non_snake_case)]
pub struct LayerMsgIvars {
    handle: Cell<u64>,
    conn: Cell<u64>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NinjaLayerMsgHandler"]
    #[thread_kind = MainThreadOnly]
    #[ivars = LayerMsgIvars]
    pub struct LayerMsgHandler;

    unsafe impl NSObjectProtocol for LayerMsgHandler {}

    unsafe impl WKScriptMessageHandler for LayerMsgHandler {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        unsafe fn user_content_controller_did_receive_script_message(
            &self,
            _controller: &WKUserContentController,
            message: &WKScriptMessage,
        ) {
            on_layer_script_message(self, message);
        }
    }
);

impl LayerMsgHandler {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = LayerMsgHandler::alloc(mtm).set_ivars(LayerMsgIvars {
            handle: Cell::new(0),
            conn: Cell::new(0),
        });
        unsafe { objc2::msg_send![super(this), init] }
    }
}

define_class!(
    #[unsafe(super(WKWebView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = LayerWebIvars]
    pub struct LayerWebView;

    impl LayerWebView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            let ok: bool = unsafe { objc2::msg_send![super(self), becomeFirstResponder] };
            if ok {
                send_layer_focus(self.ivars().handle.get(), self.ivars().conn.get(), true);
            }
            ok
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            let ok: bool = unsafe { objc2::msg_send![super(self), resignFirstResponder] };
            if ok {
                send_layer_focus(self.ivars().handle.get(), self.ivars().conn.get(), false);
            }
            ok
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let mods = keymap::mods_from_flags(event.modifierFlags().0 as u64);
            let proto = modifiers_from_mods(mods);
            let chars = event.characters().map(|c| c.to_string()).unwrap_or_default();
            let fallback = chars.chars().next();
            let key = code_to_key_name(event.keyCode(), fallback);
            if (key == "esc" && !proto.contains(&Modifier::Cmd))
                || (key == "w"
                    && proto.contains(&Modifier::Cmd)
                    && !proto.contains(&Modifier::Shift))
            {
                if let Some(w) = self.window() {
                    w.performClose(None);
                }
                return;
            }
            let _: () = unsafe { objc2::msg_send![super(self), keyDown: event] };
        }
    }
);

impl LayerWebView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let config = unsafe { WKWebViewConfiguration::new(mtm) };
        let handler = LayerMsgHandler::new(mtm);
        let ucc = unsafe { config.userContentController() };
        let proto = ProtocolObject::from_ref(&*handler);
        unsafe {
            ucc.addScriptMessageHandler_name(proto, &NSString::from_str("layer"));
        }
        let this = LayerWebView::alloc(mtm).set_ivars(LayerWebIvars {
            handle: Cell::new(0),
            conn: Cell::new(0),
            handler: RefCell::new(Some(handler)),
        });
        // SAFETY: WKWebView 指定初始化器；ivars 已就位。
        unsafe {
            objc2::msg_send![super(this), initWithFrame: frame, configuration: &*config]
        }
    }

    fn bind(&self, handle: u64, conn: u64) {
        self.ivars().handle.set(handle);
        self.ivars().conn.set(conn);
        if let Some(h) = self.ivars().handler.borrow().as_ref() {
            h.ivars().handle.set(handle);
            h.ivars().conn.set(conn);
        }
    }
}

/// 注册表本体（只在主线程碰；static 要求手工 Send）。
struct Registry {
    layers: Vec<LayerEntry>,
    next_handle: u64,
}

unsafe impl Send for Registry {}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    layers: Vec::new(),
    next_handle: 1,
});

/// present：IOSurface 像素 → CGImage（LayerView 的 drawRect 画进
/// layer-backed 内容）。
///
/// **适配器取舍（实测）**：`layer.contents = IOSurfaceRef` 与「sublayer
/// 挂 ghostty Metal 层」两条路都不稳（前者在本宿主的 layer-hosting 树
/// 不渲染——宿主直写不透明像素也不显示；后者几何不随 view 坐标系走）；
/// v0 走 **present 拷贝**——lock → 拷出 BGRA 字节 →
/// CGDataProvider(CFData) → CGImage(PremultipliedFirst|ByteOrder32Little)
/// → LayerView 重画。每帧一次 CPU 拷贝（616x220x4 ≈ 0.5MiB），公开
/// API，稳定可见；协议面不变（插件仍写 IOSurface）。
fn surface_to_image(surface: &CFRetained<IOSurfaceRef>) -> Option<CFRetained<CGImage>> {
    let w = surface.width();
    let h = surface.height();
    if w == 0 || h == 0 {
        return None;
    }
    let bpr = surface.bytes_per_row();
    // SAFETY: read/write 锁成对；base_address 在锁内有效。
    let kr = unsafe { surface.lock(objc2_io_surface::IOSurfaceLockOptions::empty(), std::ptr::null_mut()) };
    if kr != 0 {
        return None;
    }
    // SAFETY: 锁内读 w*h*bpr 的共享内存（越界防护按 bpr×h 拷贝）。
    let bytes: Vec<u8> = unsafe {
        std::slice::from_raw_parts(surface.base_address().as_ptr() as *const u8, bpr * h).to_vec()
    };
    // SAFETY: 与 lock 成对。
    let _ = unsafe { surface.unlock(objc2_io_surface::IOSurfaceLockOptions::empty(), std::ptr::null_mut()) };

    let data = CFData::from_bytes(&bytes);
    let provider = CGDataProvider::with_cf_data(Some(&data))?;
    let space = objc2_core_graphics::CGColorSpace::new_device_rgb()?;
    let info = objc2_core_graphics::CGBitmapInfo(
        CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0,
    );
    // SAFETY: 参数与位图布局匹配（BGRA 预乘 32bpp）。
    unsafe {
        CGImage::new(
            w,
            h,
            8,
            32,
            bpr,
            Some(&space),
            info,
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentPerceptual,
        )
    }
}

/// i64 → CFNumber（kIOSurface* 属性字典值用）。
fn cf_i64(v: i64) -> Option<CFRetained<CFNumber>> {
    // SAFETY: 值指针指向合法 i64；SInt64Type 与之匹配。
    unsafe { CFNumber::new(None::<&objc2_core_foundation::CFAllocator>, CFNumberType::SInt64Type, (&raw const v).cast()) }
}

/// 开层：几何（overlay_rect）→ 全局 IOSurface（BGRA8，跨进程共享）→
/// 宿主自持 LayerView（layer-backed，drawRect 画最新帧）叠在
/// SurfaceHostView 之上（ghostty 的 Metal 层是同一 view 的 layer，
/// q0 审计 #4：宿主 subview 天然在终端之上）。返回发给插件的
/// `layer.ready`。同一 pane 至多一层（重复 open 先收旧层）。
fn layer_open(geom: &LayerGeom, m: &LayerOpen, conn: u64) -> Option<LayerReady> {
    match (m.placement, m.surface) {
        (Placement::Tab, Surface::Html) => layer_open_tab_html(geom, m, conn),
        (Placement::Tab, Surface::Pixels) => layer_open_tab_pixels(geom, m, conn),
        (Placement::Overlay | Placement::Side, Surface::Pixels) => {
            layer_open_overlay(geom, m, conn)
        }
        (Placement::Overlay | Placement::Side, Surface::Html) => {
            eprintln!("ninja: overlay/side 不接 html 表面");
            None
        }
    }
}

fn layer_open_overlay(geom: &LayerGeom, m: &LayerOpen, conn: u64) -> Option<LayerReady> {
    let mtm = MainThreadMarker::new()?;
    // 挂窗视图才有意义（无窗 = 无合成面）。
    let _window = geom.view.window()?;

    // 同 pane 先收旧层（v0 简化：一次一层）。
    host_close_layers_of_pane(geom.pane);

    let rect = match m.placement {
        Placement::Overlay => overlay_rect(m.anchor_row, m.anchor_col, geom.cell_pt, geom.view_pt),
        Placement::Side => overlay_rect(m.anchor_row, 0, geom.cell_pt, geom.view_pt),
        Placement::Tab => unreachable!("tab 走 layer_open_tab"),
    };
    let w_pt = rect.2.round().max(64.0);
    let h_pt = rect.3.round().max(64.0);
    let w_px = (w_pt * geom.scale).round().max(64.0) as u32;
    let h_px = (h_pt * geom.scale).round().max(64.0) as u32;

    // IOSurface：BGRA8Unorm（与插件侧 CGContext 位图布局一致）。
    // kIOSurfaceIsGlobal=true：跨进程按 global id 共享（协议 layer.ready
    // 的 io_surface_id；插件 IOSurfaceLookup 靠它）。
    let dict = unsafe {
        let keys: [*const std::ffi::c_void; 5] = [
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceWidth).cast(),
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceHeight).cast(),
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceBytesPerElement).cast(),
            std::ptr::from_ref(objc2_io_surface::kIOSurfacePixelFormat).cast(),
            #[allow(deprecated)] // 跨进程按 global id 共享是 v0 协议钉死机制
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceIsGlobal).cast(),
        ];
        let w_v = cf_i64(w_px as i64)?;
        let h_v = cf_i64(h_px as i64)?;
        let bpe = cf_i64(4)?;
        // 'BGRA' fourcc（big-endian 字节序常量）。
        let fmt = cf_i64(0x4247_5241_i64)?;
        // isGlobal 接 CFBoolean（不是 0/1 数字）。
        let global = CFBoolean::new(true);
        let values: [*const std::ffi::c_void; 5] = [
            std::ptr::from_ref(&*w_v).cast(),
            std::ptr::from_ref(&*h_v).cast(),
            std::ptr::from_ref(&*bpe).cast(),
            std::ptr::from_ref(&*fmt).cast(),
            std::ptr::from_ref(global).cast(),
        ];
        let mut keys_mut = keys;
        let mut values_mut = values;
        CFDictionary::new(
            None,
            keys_mut.as_mut_ptr(),
            values_mut.as_mut_ptr(),
            5,
            &objc2_core_foundation::kCFTypeDictionaryKeyCallBacks,
            &objc2_core_foundation::kCFTypeDictionaryValueCallBacks,
        )?
    };
    // SAFETY: 字典键值类型匹配（kIOSurface* 键全部接 CF 类型）。
    let surface = unsafe { IOSurfaceRef::new(&dict) }?;
    let surface_id = surface.id() as u64;

    // 合成视图：frame 在（翻转的）父视图坐标系 = 左上原点直配。
    let view = LayerView::new(mtm);
    view.setWantsLayer(true);
    view.setFrame(NSRect::new(
        NSPoint::new(rect.0, rect.1),
        NSSize::new(w_pt, h_pt),
    ));
    geom.view.addSubview(&view); // 父 view 持有 subview

    let mut reg = REGISTRY.lock().ok()?;
    let handle = reg.next_handle;
    reg.next_handle += 1;
    view.ivars().handle.set(handle);
    view.ivars().conn.set(conn);
    view.ivars().is_tab.set(false);
    reg.layers.push(LayerEntry {
        handle,
        pane: geom.pane,
        view: Some(view),
        surface: Some(surface),
        web: None,
        conn,
        tab_window: None,
        open_id: m.id,
    });
    drop(reg);
    ade_debug(&format!(
        "layer.open overlay → ready handle={handle} {w_px}x{h_px}px dpi={} iosurface={surface_id}",
        (72.0 * geom.scale) as u32
    ));
    Some(LayerReady::new(
        m.id,
        handle,
        w_px,
        h_px,
        (72.0 * geom.scale) as u32,
        surface_id,
    ))
}

/// 预览标签不挂终端 pane（避免终端 resize/Esc 误收）。
const TAB_PANE: u32 = u32::MAX;

thread_local! {
    static CLOSING_LAYER_TAB: Cell<bool> = const { Cell::new(false) };
}

fn new_global_iosurface(w_px: u32, h_px: u32) -> Option<CFRetained<IOSurfaceRef>> {
    let dict = unsafe {
        let keys: [*const std::ffi::c_void; 5] = [
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceWidth).cast(),
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceHeight).cast(),
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceBytesPerElement).cast(),
            std::ptr::from_ref(objc2_io_surface::kIOSurfacePixelFormat).cast(),
            #[allow(deprecated)]
            std::ptr::from_ref(objc2_io_surface::kIOSurfaceIsGlobal).cast(),
        ];
        let w_v = cf_i64(w_px as i64)?;
        let h_v = cf_i64(h_px as i64)?;
        let bpe = cf_i64(4)?;
        let fmt = cf_i64(0x4247_5241_i64)?;
        let global = CFBoolean::new(true);
        let values: [*const std::ffi::c_void; 5] = [
            std::ptr::from_ref(&*w_v).cast(),
            std::ptr::from_ref(&*h_v).cast(),
            std::ptr::from_ref(&*bpe).cast(),
            std::ptr::from_ref(&*fmt).cast(),
            std::ptr::from_ref(global).cast(),
        ];
        let mut keys_mut = keys;
        let mut values_mut = values;
        CFDictionary::new(
            None,
            keys_mut.as_mut_ptr(),
            values_mut.as_mut_ptr(),
            5,
            &objc2_core_foundation::kCFTypeDictionaryKeyCallBacks,
            &objc2_core_foundation::kCFTypeDictionaryValueCallBacks,
        )?
    };
    unsafe { IOSurfaceRef::new(&dict) }
}

fn tab_title(m: &LayerOpen) -> &str {
    if m.title.is_empty() {
        "Tab"
    } else {
        m.title.as_str()
    }
}

fn tab_content_size(geom: &LayerGeom) -> NSSize {
    geom.view
        .window()
        .map(|w| w.contentRectForFrameRect(w.frame()).size)
        .unwrap_or(NSSize::new(800.0, 600.0))
}

fn layer_open_tab_html(geom: &LayerGeom, m: &LayerOpen, conn: u64) -> Option<LayerReady> {
    let mtm = MainThreadMarker::new()?;
    let parent = geom.view.window();
    let cs = tab_content_size(geom);
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), cs);
    let web = LayerWebView::new(mtm, frame);
    let window = crate::shell::new_chrome_tab(mtm, tab_title(m), web.as_super(), parent.as_deref());
    let scale = window.backingScaleFactor().max(1.0);
    let b = web.bounds();
    let w_pt = b.size.width.max(64.0);
    let h_pt = b.size.height.max(64.0);
    let w_px = (w_pt * scale).round().max(64.0) as u32;
    let h_px = (h_pt * scale).round().max(64.0) as u32;

    let mut reg = REGISTRY.lock().ok()?;
    let handle = reg.next_handle;
    reg.next_handle += 1;
    web.bind(handle, conn);
    let _ = window.makeFirstResponder(Some(web.as_super()));
    reg.layers.push(LayerEntry {
        handle,
        pane: TAB_PANE,
        view: None,
        surface: None,
        web: Some(web),
        conn,
        tab_window: Some(window),
        open_id: m.id,
    });
    drop(reg);
    eprintln!("ninja: layer tab handle={handle} {w_pt:.0}x{h_pt:.0}pt html");
    Some(LayerReady::new(
        m.id,
        handle,
        w_px,
        h_px,
        (72.0 * scale) as u32,
        0,
    ))
}

fn layer_open_tab_pixels(geom: &LayerGeom, m: &LayerOpen, conn: u64) -> Option<LayerReady> {
    let mtm = MainThreadMarker::new()?;
    let parent = geom.view.window();
    let cs = tab_content_size(geom);
    let view = LayerView::new(mtm);
    view.setWantsLayer(true);
    view.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), cs));
    view.ivars().is_tab.set(true);
    let window = crate::shell::new_chrome_tab(mtm, tab_title(m), view.as_super(), parent.as_deref());
    let scale = window.backingScaleFactor().max(1.0);
    let b = view.bounds();
    let w_pt = b.size.width.max(64.0);
    let h_pt = b.size.height.max(64.0);
    let w_px = (w_pt * scale).round().max(64.0) as u32;
    let h_px = (h_pt * scale).round().max(64.0) as u32;
    let surface = new_global_iosurface(w_px, h_px)?;
    let surface_id = surface.id() as u64;

    let mut reg = REGISTRY.lock().ok()?;
    let handle = reg.next_handle;
    reg.next_handle += 1;
    view.ivars().handle.set(handle);
    view.ivars().conn.set(conn);
    let _ = window.makeFirstResponder(Some(view.as_super()));
    reg.layers.push(LayerEntry {
        handle,
        pane: TAB_PANE,
        view: Some(view),
        surface: Some(surface),
        web: None,
        conn,
        tab_window: Some(window),
        open_id: m.id,
    });
    drop(reg);
    eprintln!("ninja: layer tab handle={handle} {w_px}x{h_px}px pixels iosurface={surface_id}");
    Some(LayerReady::new(
        m.id,
        handle,
        w_px,
        h_px,
        (72.0 * scale) as u32,
        surface_id,
    ))
}

fn tab_key_down(view: &LayerView, event: &NSEvent) {
    let mods = keymap::mods_from_flags(event.modifierFlags().0 as u64);
    let proto = modifiers_from_mods(mods);
    let chars = event.characters().map(|c| c.to_string()).unwrap_or_default();
    let fallback = chars.chars().next();
    let key = code_to_key_name(event.keyCode(), fallback);
    if (key == "esc" && !proto.contains(&Modifier::Cmd))
        || (key == "w" && proto.contains(&Modifier::Cmd) && !proto.contains(&Modifier::Shift))
    {
        if let Some(w) = view.window() {
            w.performClose(None);
        }
        return;
    }
    send_tab_key(view, key, chars, proto);
}

fn send_to_plugin(conn: u64, msg: &Message) {
    if let Some(host) = take_dispatcher()
        && let Ok(mut h) = host.lock()
    {
        let _ = h.send_message(conn, msg);
    }
}

fn send_tab_key(view: &LayerView, key: String, text: String, mods: Vec<Modifier>) {
    let handle = view.ivars().handle.get();
    let conn = view.ivars().conn.get();
    if handle == 0 {
        return;
    }
    send_to_plugin(conn, &Message::InputKey(InputKey::new(handle, key, text, mods)));
}

fn send_layer_focus(handle: u64, conn: u64, focused: bool) {
    if handle == 0 {
        return;
    }
    send_to_plugin(conn, &Message::InputFocus(InputFocus::new(handle, focused)));
}

fn layer_event_px(view: &NSView, event: &NSEvent) -> (u32, u32) {
    let loc = view.convertPoint_fromView(event.locationInWindow(), None);
    let scale = view
        .window()
        .map(|w| w.backingScaleFactor())
        .unwrap_or(1.0)
        .max(1.0);
    let x = (loc.x * scale).round().max(0.0) as u32;
    let y = (loc.y * scale).round().max(0.0) as u32;
    (x, y)
}

fn mouse_button_of(event: &NSEvent) -> MouseButton {
    match event.buttonNumber() {
        1 => MouseButton::Right,
        2 => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

fn send_layer_mouse(view: &LayerView, event: &NSEvent, action: MouseAction) {
    let handle = view.ivars().handle.get();
    let conn = view.ivars().conn.get();
    if handle == 0 {
        return;
    }
    let (x_px, y_px) = layer_event_px(view.as_super(), event);
    let mods = modifiers_from_mods(keymap::mods_from_flags(event.modifierFlags().0 as u64));
    send_to_plugin(
        conn,
        &Message::InputMouse(InputMouse::new(
            handle,
            mouse_button_of(event),
            action,
            x_px,
            y_px,
            mods,
        )),
    );
}

fn send_layer_scroll(view: &LayerView, event: &NSEvent) {
    let handle = view.ivars().handle.get();
    let conn = view.ivars().conn.get();
    if handle == 0 {
        return;
    }
    let dx = event.scrollingDeltaX().round() as i32;
    let dy = event.scrollingDeltaY().round() as i32;
    if dx == 0 && dy == 0 {
        return;
    }
    let mods = modifiers_from_mods(keymap::mods_from_flags(event.modifierFlags().0 as u64));
    send_to_plugin(
        conn,
        &Message::InputScroll(InputScroll::new(handle, dx, dy, mods)),
    );
}

fn rebuild_tab_layer(handle: u64) {
    let Ok(mut reg) = REGISTRY.lock() else {
        return;
    };
    let Some(e) = reg.layers.iter_mut().find(|e| e.handle == handle) else {
        return;
    };
    let Some(view) = e.view.as_ref() else {
        return; // html 表面跟窗口尺寸，不必重建
    };
    let Some(window) = view.window() else {
        return;
    };
    let scale = window.backingScaleFactor().max(1.0);
    let b = view.bounds();
    let w_px = (b.size.width.max(64.0) * scale).round().max(64.0) as u32;
    let h_px = (b.size.height.max(64.0) * scale).round().max(64.0) as u32;
    let Some(surface) = new_global_iosurface(w_px, h_px) else {
        return;
    };
    let surface_id = surface.id() as u64;
    let open_id = e.open_id;
    let conn = e.conn;
    e.surface = Some(surface);
    drop(reg);
    send_to_plugin(
        conn,
        &Message::LayerReady(LayerReady::new(
            open_id,
            handle,
            w_px,
            h_px,
            (72.0 * scale) as u32,
            surface_id,
        )),
    );
}

fn layer_load_html(handle: u64, html: &str) {
    let Ok(reg) = REGISTRY.lock() else { return };
    let Some(e) = reg.layers.iter().find(|e| e.handle == handle) else {
        eprintln!("ninja: layer.html handle={handle} 无此层");
        return;
    };
    let Some(web) = e.web.as_ref() else {
        eprintln!("ninja: layer.html handle={handle} 不是 html 表面");
        return;
    };
    unsafe { web.loadHTMLString_baseURL(&NSString::from_str(html), None) };
    eprintln!("ninja: layer.html handle={handle} {} bytes", html.len());
}

fn layer_post_msg(handle: u64, name: &str, body: &str) {
    let Ok(reg) = REGISTRY.lock() else {
        return;
    };
    let Some(e) = reg.layers.iter().find(|e| e.handle == handle) else {
        return;
    };
    let Some(web) = e.web.as_ref() else {
        return; // 像素层：不透明邮箱不适用
    };
    let payload = serde_json::json!({ "name": name, "body": body });
    let js = format!(
        "(function(){{var d={payload};window.dispatchEvent(new CustomEvent('layer-msg',{{detail:d}}));}})();"
    );
    unsafe {
        web.evaluateJavaScript_completionHandler(&NSString::from_str(&js), None);
    }
}

fn on_layer_script_message(handler: &LayerMsgHandler, message: &WKScriptMessage) {
    let handle = handler.ivars().handle.get();
    let conn = handler.ivars().conn.get();
    if handle == 0 {
        return;
    }
    let raw = unsafe { message.body() };
    let (name, body) = parse_script_msg_body(&raw);
    send_to_plugin(conn, &Message::LayerMsg(LayerMsg::new(handle, name, body)));
}

fn parse_script_msg_body(obj: &AnyObject) -> (String, String) {
    if let Some(s) = obj.downcast_ref::<NSString>() {
        return (String::new(), s.to_string());
    }
    let key_name = NSString::from_str("name");
    let key_body = NSString::from_str("body");
    let name: Option<Retained<NSObject>> = unsafe { objc2::msg_send![obj, objectForKey: &*key_name] };
    let body: Option<Retained<NSObject>> = unsafe { objc2::msg_send![obj, objectForKey: &*key_body] };
    let name = name
        .and_then(|v| v.downcast_ref::<NSString>().map(|s| s.to_string()))
        .unwrap_or_default();
    let body = body
        .and_then(|v| v.downcast_ref::<NSString>().map(|s| s.to_string()))
        .unwrap_or_default();
    (name, body)
}

/// 插件层标签 windowWillClose：收层并通知插件（不再 performClose）。
pub fn layer_tab_closed(content: &NSView) {
    let handle = if content.class() == LayerWebView::class() {
        let view: &LayerWebView = unsafe { &*std::ptr::from_ref(content).cast() };
        view.ivars().handle.get()
    } else if content.class() == LayerView::class() {
        let view: &LayerView = unsafe { &*std::ptr::from_ref(content).cast() };
        view.ivars().handle.get()
    } else {
        return;
    };
    if handle == 0 {
        return;
    }
    CLOSING_LAYER_TAB.set(true);
    let conn = {
        let Ok(reg) = REGISTRY.lock() else {
            CLOSING_LAYER_TAB.set(false);
            return;
        };
        reg.layers
            .iter()
            .find(|e| e.handle == handle)
            .map(|e| e.conn)
    };
    layer_close(handle);
    CLOSING_LAYER_TAB.set(false);
    if let Some(conn) = conn
        && let Some(host) = take_dispatcher()
        && let Ok(mut h) = host.try_lock()
    {
        let _ = h.send_layer_close(conn, handle);
    }
}

/// present：插件画完本帧——重设 contents（nil→surface）强制 CA 重采样
/// 共享内存并重合成（同一 IOSurfaceRef 反复写，CA 不会自己发现变化）。
fn layer_present(handle: u64) {
    let Ok(reg) = REGISTRY.lock() else { return };
    let Some(e) = reg.layers.iter().find(|e| e.handle == handle) else {
        return;
    };
    let (Some(surface), Some(view)) = (e.surface.as_ref(), e.view.as_ref()) else {
        return;
    };
    let Some(image) = surface_to_image(surface) else {
        ade_debug(&format!("layer.present handle={handle}：像素拷贝失败"));
        return;
    };
    *view.ivars().image.borrow_mut() = Some(image);
    view.ivars().presented.set(true);
    view.setNeedsDisplay(true);
    eprintln!(
        "ninja: layer.present handle={handle} image={}x{} view={:.0}x{:.0}",
        surface.width(),
        surface.height(),
        view.bounds().size.width,
        view.bounds().size.height
    );
}

/// 摘一个层（返回 是否摘到）。
fn layer_close(handle: u64) -> bool {
    let Ok(mut reg) = REGISTRY.lock() else {
        return false;
    };
    let Some(pos) = reg.layers.iter().position(|e| e.handle == handle) else {
        return false;
    };
    let e = reg.layers.remove(pos);
    drop(reg);
    remove_overlay(&e);
    ade_debug(&format!("layer.close handle={handle}（已摘）"));
    true
}

/// 把合成视图从父视图摘掉（主线程）。
fn remove_overlay(e: &LayerEntry) {
    if let Some(view) = e.view.as_ref() {
        view.discard_frame();
    }
    if let Some(w) = &e.tab_window {
        if !CLOSING_LAYER_TAB.get() {
            w.performClose(None);
        }
    } else if let Some(view) = e.view.as_ref() {
        view.removeFromSuperview();
    }
}

impl LayerView {
    /// 收层时立即丢弃帧像素（CGImage）——不等 AppKit dealloc 视图。
    fn discard_frame(&self) {
        *self.ivars().image.borrow_mut() = None;
    }

    /// 建视图（ivars 先就位再 super init；无自定义 initWithFrame 需求）。
    fn new(mtm: objc2::MainThreadMarker) -> Retained<Self> {
        let this = LayerView::alloc(mtm).set_ivars(LayerViewIvars {
            image: RefCell::new(None),
            handle: Cell::new(0),
            conn: Cell::new(0),
            is_tab: Cell::new(false),
            presented: Cell::new(false),
        });
        // SAFETY: super 的 initWithFrame:；ivars 已就位（零尺寸——
        // 调用方随即 setFrame）。
        unsafe { objc2::msg_send![super(this), initWithFrame: NSRect::ZERO] }
    }
}

/// 收某连接的全部层（连接死亡）。返回是否收过。
fn layer_close_by_conn(conn: u64) -> bool {
    let Ok(mut reg) = REGISTRY.lock() else {
        return false;
    };
    let all: Vec<LayerEntry> = std::mem::take(&mut reg.layers);
    let mut removed = Vec::new();
    for e in all {
        if e.conn == conn {
            removed.push(e);
        } else {
            reg.layers.push(e);
        }
    }
    drop(reg);
    for e in &removed {
        remove_overlay(e);
    }
    !removed.is_empty()
}

/// 收某 pane 的全部层（pane 关闭 / resize / Esc 兜底）。返回 (handle, conn)
/// 列表（调用方负责通知插件 layer.close）。
fn layer_close_pane(pane: u32) -> Vec<(u64, u64)> {
    let Ok(mut reg) = REGISTRY.lock() else {
        return Vec::new();
    };
    let all: Vec<LayerEntry> = std::mem::take(&mut reg.layers);
    let mut removed = Vec::new();
    for e in all {
        if e.pane == pane {
            removed.push(e);
        } else {
            reg.layers.push(e);
        }
    }
    drop(reg);
    for e in &removed {
        remove_overlay(e);
    }
    removed.iter().map(|e| (e.handle, e.conn)).collect()
}

/// 收全部层（禁用/退出）。返回 (handle, conn) 列表。
fn layer_close_all() -> Vec<(u64, u64)> {
    let removed: Vec<LayerEntry> = REGISTRY
        .lock()
        .ok()
        .map(|mut reg| std::mem::take(&mut reg.layers))
        .unwrap_or_default();
    for e in &removed {
        remove_overlay(e);
    }
    removed.iter().map(|e| (e.handle, e.conn)).collect()
}

/// pane 是否有前台层（键盘先给插件）。返回 (layer, conn)。
fn layer_foreground(pane: u32) -> Option<(u64, u64)> {
    REGISTRY.lock().ok().and_then(|reg| {
        reg.layers
            .iter()
            .find(|e| e.pane == pane)
            .map(|e| (e.handle, e.conn))
    })
}

fn any_layers() -> bool {
    REGISTRY.lock().map(|reg| !reg.layers.is_empty()).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// 泵 timer（层/主题覆盖/等连接期间存在；主 runloop）
// ---------------------------------------------------------------------------

/// CFRunLoopTimer 的存储（CF 类型不自动 Send；只在主线程碰，手工标注
/// 满足 static 要求）。
struct TimerSlot(
    Option<
        objc2_core_foundation::CFRetained<objc2_core_foundation::CFRunLoopTimer>,
    >,
);
unsafe impl Send for TimerSlot {}

static PUMP_TIMER: Mutex<TimerSlot> = Mutex::new(TimerSlot(None));

/// 拉起后「等首个连接」的窗口：插件被拉起后，它的 connect + 连接即推
/// 的 theme.set 要靠泵消化；但此时可能既无层也无色板覆盖（泵的常规
/// 启停条件都不满足），泵会自停 → 连接永远没人 accept。窗口内泵不自
/// 停；首个连接进来（或窗口过期——拉不起/挂死的插件不该拖住空转红线）
/// 即恢复常规规则。
const SPAWN_CONNECT_WINDOW: Duration = Duration::from_secs(5);

static SPAWN_PENDING: Mutex<Option<Instant>> = Mutex::new(None);

fn spawn_pending_arm() {
    if let Ok(mut s) = SPAWN_PENDING.lock() {
        *s = Some(Instant::now() + SPAWN_CONNECT_WINDOW);
    }
}

fn spawn_pending_disarm() {
    if let Ok(mut s) = SPAWN_PENDING.lock() {
        *s = None;
    }
}

fn spawn_pending_active() -> bool {
    SPAWN_PENDING
        .lock()
        .map(|s| s.map(|dl| Instant::now() < dl).unwrap_or(false))
        .unwrap_or(false)
}

/// 泵回调（CFRunLoopTimer callout，主线程）。
/// 安全 fn 可强制转换成 CFRunLoopTimerCallBack 的 unsafe 函数指针。
extern "C-unwind" fn pump_tick(
    _timer: *mut objc2_core_foundation::CFRunLoopTimer,
    _info: *mut std::ffi::c_void,
) {
    pump_now();
}

/// 起泵（幂等）：首个层打开/主题覆盖/拉起后由各路径调用。
fn ensure_pump_timer() {
    let Some(main) = objc2_core_foundation::CFRunLoop::main() else {
        return;
    };
    let mut slot = match PUMP_TIMER.lock() {
        Ok(s) => s,
        Err(_) => return,
    };
    if slot.0.is_some() {
        return;
    }
    let mut context = objc2_core_foundation::CFRunLoopTimerContext {
        version: 0,
        info: std::ptr::null_mut(),
        retain: None,
        release: None,
        copyDescription: None,
    };
    // SAFETY: context 布局正确；callout 只跑在主 runloop。
    let timer = unsafe {
        objc2_core_foundation::CFRunLoopTimer::new(
            None,
            0.0, // 立即首发
            PUMP_INTERVAL,
            0,
            0,
            Some(pump_tick),
            &raw mut context,
        )
    };
    if let Some(t) = timer {
        // SAFETY: t 合法；加入主 runloop common modes。
        unsafe { main.add_timer(Some(&t), objc2_core_foundation::kCFRunLoopCommonModes) };
        slot.0 = Some(t);
    }
}

fn has_plugin_conns() -> bool {
    take_dispatcher()
        .and_then(|h| h.try_lock().ok().map(|g| !g.conns.is_empty()))
        .unwrap_or(false)
}

/// 停泵（幂等）：层/覆盖/等待窗口/插件连接都没了才停。
fn stop_pump_timer_if_idle() {
    if any_layers()
        || plugin_theme_override().is_some()
        || spawn_pending_active()
        || has_plugin_conns()
    {
        return;
    }
    if let Ok(mut slot) = PUMP_TIMER.lock()
        && let Some(t) = slot.0.take()
            && let Some(main) = objc2_core_foundation::CFRunLoop::main()
    {
        // SAFETY: t 曾加入主 runloop。
        unsafe { main.remove_timer(Some(&t), objc2_core_foundation::kCFRunLoopCommonModes) };
    }
}

/// 泵入口（timer 回调直调；测试可直调）。
pub fn pump_now() {
    match take_dispatcher() {
        Some(host) => {
            // try_lock：点击握手若正握着同一把锁，嵌套 timer 不能再阻塞。
            if let Ok(mut h) = host.try_lock() {
                h.pump_plugins();
                let keep = any_layers()
                    || plugin_theme_override().is_some()
                    || spawn_pending_active()
                    || !h.conns.is_empty();
                drop(h);
                if !keep {
                    stop_pump_timer_if_idle();
                }
            }
        }
        None => stop_pump_timer_if_idle(),
    }
}

// ---------------------------------------------------------------------------
// 全局分发器：surface（⌘+点击）/ 面板 / 取证钩子 → PluginHost 的通路
// ---------------------------------------------------------------------------

// PluginHost 住在本静态槽的 Arc 里（生命周期 = 进程；面板把插件从零
// 拉起需要随时可造新 host）。只在主线程读写（点击/面板/钩子本就主
// 线程），Mutex 只为满足 static 要求。

static DISPATCHER: Mutex<Option<Arc<Mutex<PluginHost>>>> = Mutex::new(None);

/// 启动配置快照（会话真值的回退源：host 还没进（空 enabled）时，面板
/// 开关用这里的 paths 解析插件）。host::init 装一次。
static SESSION_CFG: Mutex<Option<PluginsConfig>> = Mutex::new(None);

/// 初始化（host::init 调）：enabled 空 = 空载（不绑 socket，只装配置
/// 快照供面板首开用）；非空 = 绑定 + 登记（拉起发生在 runloop 就绪后，
/// [`spawn_startup_plugins`]）。
pub fn init(cfg: PluginsConfig) {
    if let Some(host) = PluginHost::start(&cfg)
        && let Ok(mut slot) = DISPATCHER.lock() {
            *slot = Some(Arc::new(Mutex::new(host)));
        }
    if let Ok(mut slot) = SESSION_CFG.lock() {
        *slot = Some(cfg);
    }
}

/// 取当前分发器（没装（空载/从未启用）→ None）。
pub fn take_dispatcher() -> Option<Arc<Mutex<PluginHost>>> {
    DISPATCHER.lock().ok().and_then(|slot| slot.clone())
}

/// 会话真值的配置快照：host 在 → 它的 cfg（面板开关已反映进去）；
/// host 不在（空载）→ 启动快照。面板行发现与写回名单都以它为准。
pub fn session_cfg() -> PluginsConfig {
    match take_dispatcher() {
        Some(host) => host.lock().map(|h| h.cfg().clone()).unwrap_or_default(),
        None => SESSION_CFG
            .lock()
            .ok()
            .and_then(|s| s.clone())
            .unwrap_or_default(),
    }
}

/// **启用即拉起**的宿主启动半边（app 的 applicationDidFinishLaunching
/// 调；runloop 就绪后）。空载（无分发器）= 无操作——门禁不变。
pub fn spawn_startup_plugins() {
    if let Some(host) = take_dispatcher()
        && let Ok(mut h) = host.lock()
    {
        h.spawn_enabled_now();
    }
}

/// 配置监视拍顺带看插件二进制 mtime：`cp` 新文件后热重载，不必退宿主。
pub fn watch_plugin_binaries() {
    if let Some(host) = take_dispatcher()
        && let Ok(mut h) = host.try_lock()
    {
        h.respawn_stale_plugins();
    }
}

/// 状态接线：全部插件的状态快照（面板与测试用）。无分发器 → 空表。
pub fn status_snapshot() -> Vec<PluginStatus> {
    match take_dispatcher() {
        Some(host) => host.lock().map(|mut h| h.snapshot()).unwrap_or_default(),
        None => Vec::new(),
    }
}

/// 宿主退出收口（applicationWillTerminate / host::shutdown 调；幂等）：
/// `NSApplication terminate:` 直接 `exit(0)`，Rust 栈不展开、静态槽不
/// drop——必须显式调本函数（SIGKILL 路径的 socket 尸体由下次启动
/// [`sweep_stale_sockets`] 清扫）。
pub fn host_shutdown() {
    if let Some(host) = take_dispatcher()
        && let Ok(mut h) = host.lock()
    {
        h.shutdown();
    }
}

/// **面板开关的宿主侧入口**（与 NINJA_PANEL_PLUGIN_FILE 钩子同一条
/// 幂等生命周期路径；写回 ninja.toml 由调用方 panel 模块做）。
/// - on：名字进会话 enabled 名单 + 立即拉起；host 不在/已禁用时先
///   重绑（从零拉起用启动快照的 paths）。
/// - off：名字出名单 + 立即杀进程/收层/断连/撤色板；名单空 → 整个
///   关掉（shutdown：删 socket，回空载）。
/// 返回 false = 开且拉不起 host（绑定失败）；关恒 true。
pub fn toggle_plugin(name: &str, on: bool) -> bool {
    if !on {
        if let Some(host) = take_dispatcher() {
            if let Ok(mut h) = host.lock() {
                h.session_disable(name);
            }
        } else if let Ok(mut slot) = SESSION_CFG.lock()
            && let Some(cfg) = slot.as_mut()
        {
            // host 不在（空载）：从启动快照名单里剔除（下次启动生效）。
            cfg.enabled.retain(|n| n != name);
        }
        return true;
    }
    match take_dispatcher() {
        Some(host) => {
            let Ok(mut h) = host.lock() else {
                return false;
            };
            if h.disabled {
                // 整面被关过：重绑。名字先进名单，新 host 一次拉起全部
                // enabled（含本次要开的）。
                let path = h.path().to_path_buf();
                let mut cfg = h.cfg().clone();
                if !cfg.enabled.iter().any(|n| n == name) {
                    cfg.enabled.push(name.to_string());
                }
                let Some(nh) = PluginHost::bind(path, cfg) else {
                    return false;
                };
                *h = nh;
                h.spawn_enabled_now();
                return true;
            }
            h.session_enable(name)
        }
        None => {
            // 空载 → 从零拉起：启动快照 + 名字 → 新 host。
            let mut cfg = session_cfg();
            if !cfg.enabled.iter().any(|n| n == name) {
                cfg.enabled.push(name.to_string());
            }
            match PluginHost::start(&cfg) {
                Some(host) => {
                    if let Ok(mut slot) = DISPATCHER.lock() {
                        *slot = Some(Arc::new(Mutex::new(host)));
                    }
                    spawn_startup_plugins();
                    true
                }
                None => false,
            }
        }
    }
}

/// 宿主关层（Esc 兜底 / resize / pane 关闭）：摘层 + 通知插件
/// `layer.close`。PRODUCT：「任何插件层都能立刻关掉」。
pub fn host_close_layers_of_pane(pane: u32) {
    for (handle, conn) in layer_close_pane(pane) {
        if let Some(host) = take_dispatcher()
            && let Ok(mut h) = host.try_lock()
        {
            let _ = h.send_layer_close(conn, handle);
        }
    }
    stop_pump_timer_if_idle();
}

// ---------------------------------------------------------------------------
// 点击上下文（surface.rs mouseUp ↔ OPEN_URL action 的同步通信）
// ---------------------------------------------------------------------------

/// 一次 ⌘+click 的上下文（mouseUp 在调 `ghostty_surface_mouse_button`
/// 前登记；OPEN_URL action 同步重入时分发器读取）。
struct ClickCtx {
    pane: u32,
    row: u32,
    col: u32,
    mods: Vec<Modifier>,
}

static CLICK_CTX: Mutex<Option<ClickCtx>> = Mutex::new(None);

/// mouseUp 前登记（surface.rs 调）。row/col 由像素→cell 换算。
pub fn click_begin(view: &SurfaceHostView, pt: NSPoint, mods: ghostty_sys::ghostty_input_mods_e) {
    let Some((row, col)) = point_to_cell(view, pt) else {
        return;
    };
    let pane = view.pane_id();
    if let Ok(mut slot) = CLICK_CTX.lock() {
        *slot = Some(ClickCtx {
            pane,
            row,
            col,
            mods: modifiers_from_mods(mods),
        });
    }
}

/// mouseUp 后清理（surface.rs 调）：返回 Some(ctx) 表示这是一次待分发
/// 的 ⌘+click 且上下文还在（OPEN_URL 取走过 = 链接源已分发，网格源
/// 不再重复）。
pub fn click_end(view: &SurfaceHostView) -> Option<(u32, u32, u32, Vec<Modifier>)> {
    let ctx = CLICK_CTX.lock().ok().and_then(|mut s| s.take())?;
    if ctx.pane != view.pane_id() {
        return None;
    }
    ctx.mods
        .contains(&Modifier::Cmd)
        .then_some((ctx.pane, ctx.row, ctx.col, ctx.mods))
}

/// OPEN_URL action 的宿主半边（host.rs dispatch 调；点击同步栈内）：
/// 读点击上下文（**取走**——之后的网格源发现 ctx 没了就不再分发）→
/// hit 广播仲裁 → 无认领 `open` 系统默认。
pub fn handle_open_url(view: &SurfaceHostView, url: &str) {
    let ctx = CLICK_CTX.lock().ok().and_then(|mut s| s.take());
    let (row, col, pane, mods) = match ctx {
        Some(c) => (c.row, c.col, c.pane, c.mods),
        // 无上下文（悬停路径 / 状态错位）：行列 0 兜底，kind 照分类。
        None => (0, 0, view.pane_id(), Vec::new()),
    };
    let kind = classify_url(url);
    let (kind, text) = normalize_open_payload(kind, url);
    dispatch_hit_with_default(view, kind, &text, row, col, pane, mods);
}

/// 网格源分发（surface.rs mouseUp 后调）：读点击行 → token → 分类 →
/// 广播仲裁 → 无认领且可解析 → `open` 系统默认。
pub fn handle_grid_hit(view: &SurfaceHostView, row: u32, col: u32, mods: Vec<Modifier>) {
    let Some(surface) = view.surface_opt() else {
        return;
    };
    let sz = unsafe { ghostty_sys::ghostty_surface_size(surface) };
    if sz.columns == 0 {
        return;
    }
    let line = crate::host::read_text(surface, 0, row, sz.columns as u32 - 1, row);
    let Some((token, _start)) = line_token_at(&line, col) else {
        ade_debug(&format!(
            "grid: 点击处非 token（row={row} col={col}，行内容 {line:?}）"
        ));
        return;
    };
    let Some(kind) = classify_token(&token) else {
        ade_debug(&format!("grid: token {token:?} 不像路径/URL，不分发"));
        return;
    };
    let (kind, text) = normalize_open_payload(kind, &token);
    dispatch_hit_with_default(view, kind, &text, row, col, view.pane_id(), mods);
}

/// 广播 + 仲裁 + 无认领系统默认 的公共出口。
fn dispatch_hit_with_default(
    view: &SurfaceHostView,
    kind: HitKind,
    text: &str,
    row: u32,
    col: u32,
    pane: u32,
    mods: Vec<Modifier>,
) {
    let id = next_hit_id();
    if id == 0 {
        // 无分发器（空载）：链接照走系统默认，路径仅在可解析时打开。
        default_open(kind, text, view);
        return;
    }
    let cwd = cwd_for_view(view);
    let hit = Hit::new(id, kind, text, &cwd, row, col, pane, mods);
    let geom = collect_geom(view);
    let outcome = dispatch_hit(&hit, geom.as_ref());
    ade_debug(&format!(
        "hit id={id} kind={kind:?} text={text:?} cwd={cwd:?} → {outcome:?}"
    ));
    match outcome {
        DispatchOutcome::Claimed { .. } => {}
        _ => default_open(kind, text, view),
    }
}

/// 无认领 → 系统默认：url/osc8 用 `/usr/bin/open` 打开；path 仅当可
/// 解析（绝对 / 按 cwd 拼上存在）时打开，否则安静放弃（不对纯文本
/// 噪声弹 Finder）。
fn default_open(kind: HitKind, text: &str, view: &SurfaceHostView) {
    let target = match kind {
        HitKind::Url | HitKind::Osc8 => Some(text.to_string()),
        HitKind::Path => {
            let cwd = cwd_for_view(view);
            let resolved = if text.starts_with('/') || text.starts_with('~') {
                std::path::PathBuf::from(text)
            } else if !cwd.is_empty() {
                PathBuf::from(cwd).join(text)
            } else {
                return; // 相对路径且无 cwd：不猜
            };
            resolved.exists().then(|| resolved.to_string_lossy().to_string())
        }
    };
    let Some(target) = target else {
        return;
    };
    ade_debug(&format!("系统默认打开 {target:?}"));
    let _ = std::process::Command::new("/usr/bin/open")
        .arg(&target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// 点击点（视图 points）→ cell（row, col）。换算用**网格占比**（视口
/// bounds ÷ surface 网格行列）而非 CELL_SIZE px——宿主的 scale 记账
/// （backingScaleFactor vs content_scale）可能随跨屏移动漂移，占比换算
/// 与最终渲染几何始终一致。无 surface / 网格未就绪 → None。
fn point_to_cell(view: &SurfaceHostView, pt: NSPoint) -> Option<(u32, u32)> {
    let surface = view.surface_opt()?;
    let sz = unsafe { ghostty_sys::ghostty_surface_size(surface) };
    if sz.rows == 0 || sz.columns == 0 {
        return None;
    }
    let b = view.bounds();
    if b.size.width <= 0.0 || b.size.height <= 0.0 {
        return None;
    }
    let col = ((pt.x.max(0.0) / b.size.width) * f64::from(sz.columns)).floor() as u32;
    let row = ((pt.y.max(0.0) / b.size.height) * f64::from(sz.rows)).floor() as u32;
    Some((
        row.min(sz.rows as u32 - 1),
        col.min(sz.columns as u32 - 1),
    ))
}

/// 广播一站式入口（无分发器/锁坏 → NoPlugins）。
fn dispatch_hit(hit: &Hit, geom: Option<&LayerGeom>) -> DispatchOutcome {
    match take_dispatcher() {
        Some(host) => host
            .lock()
            .map(|mut h| h.dispatch_hit(hit, geom))
            .unwrap_or(DispatchOutcome::NoPlugins),
        None => DispatchOutcome::NoPlugins,
    }
}

/// 点击路径一站式入口：给 hit 发号（无分发器 → 0）。
fn next_hit_id() -> u64 {
    match take_dispatcher() {
        Some(host) => host.lock().map(|mut h| h.next_hit_id()).unwrap_or(0),
        None => 0,
    }
}

/// 收集开层几何（主线程；无窗/无网格 → None）。cell 尺寸用**网格占比**
/// （视口 bounds ÷ surface 网格行列）——与 [`point_to_cell`] 同一换算
/// 纪律（CELL_SIZE 的 px 记账可能跨屏漂移）。
fn collect_geom(view: &SurfaceHostView) -> Option<LayerGeom> {
    let window = view.window()?;
    let scale = window.backingScaleFactor().max(1.0);
    let b = view.bounds();
    if b.size.width <= 0.0 || b.size.height <= 0.0 {
        return None;
    }
    let grid = view
        .surface_opt()
        .map(|s| unsafe { ghostty_sys::ghostty_surface_size(s) })?;
    if grid.rows == 0 || grid.columns == 0 {
        return None;
    }
    Some(LayerGeom {
        pane: view.pane_id(),
        cell_pt: (
            b.size.width / f64::from(grid.columns),
            b.size.height / f64::from(grid.rows),
        ),
        view_pt: (b.size.width, b.size.height),
        scale,
        // SAFETY: 同类指针 retain（AppKit 引用计数安全；view 在主线程存活）。
        view: unsafe {
            Retained::retain(std::ptr::from_ref(view) as *mut SurfaceHostView)
        }
        .expect("view alive"),
    })
}

// ---------------------------------------------------------------------------
// 键盘路由（surface.rs keyDown 先走这里：层前台 / 已授予热键）
// ---------------------------------------------------------------------------

/// keyDown 的插件路由。返回 true = 已消费（不进终端）：
/// - 本 pane 有插件层 → 层前台：Esc 宿主直接关层（PRODUCT 语义），
///   其余键转 `input.key` 发给拥有该层的插件连接；
/// - 已授予的全局热键命中 → `input.key{layer:0}` 发给拥有方。
pub fn key_route(view: &SurfaceHostView, keycode: u16, mods: ghostty_sys::ghostty_input_mods_e, chars: Option<String>) -> bool {
    let pane = view.pane_id();
    let proto_mods = modifiers_from_mods(mods);
    // 层前台优先。
    if let Some((layer, conn)) = layer_foreground(pane) {
        // Esc：宿主直接关层（不依赖插件响应速度）；⌘Esc 例外（系统语义）。
        if keycode == 53 && !proto_mods.contains(&Modifier::Cmd) {
            eprintln!("ninja: Esc 关层（pane {pane}）");
            host_close_layers_of_pane(pane);
            return true;
        }
        let fallback = chars.as_deref().and_then(|s| s.chars().next());
        let key = code_to_key_name(keycode, fallback);
        let msg = Message::InputKey(InputKey::new(layer, key, chars.unwrap_or_default(), proto_mods));
        if let Some(host) = take_dispatcher()
            && let Ok(mut h) = host.lock()
        {
            let _ = h.send_message(conn, &msg);
        }
        return true;
    }
    // 已授予热键。
    let fallback = chars.as_deref().and_then(|s| s.chars().next());
    let key = code_to_key_name(keycode, fallback);
    let grant = take_dispatcher().and_then(|host| {
        host.lock().ok().and_then(|h| h.hotkey_owner(&key, &proto_mods))
    });
    if let Some(conn) = grant {
        let msg =
            Message::InputKey(InputKey::new(0, key, chars.unwrap_or_default(), proto_mods));
        if let Some(host) = take_dispatcher()
            && let Ok(mut h) = host.lock()
        {
            let _ = h.send_message(conn, &msg);
        }
        return true;
    }
    false
}

impl PluginHost {
    /// 已授予热键的拥有连接（未授予 → None）。
    fn hotkey_owner(&self, key: &str, mods: &[Modifier]) -> Option<u64> {
        self.hotkeys
            .iter()
            .find(|g| g.matches(key, mods))
            .map(|g| g.conn)
    }

    /// 按连接发任意消息（input.key / layer.close 回程的公开包装）。
    fn send_message(&mut self, conn_id: u64, msg: &Message) -> std::io::Result<()> {
        let frame =
            encode_frame(msg).map_err(|e| std::io::Error::other(format!("encode: {e}")))?;
        let c = self
            .conns
            .iter_mut()
            .find(|c| c.id == conn_id)
            .ok_or_else(|| std::io::Error::other("plugin conn gone"))?;
        c.stream.write_all(&frame)
    }

    /// send_layer_close（host_close_layers_of_pane 的公开包装）。
    fn send_layer_close(&mut self, conn_id: u64, handle: u64) -> std::io::Result<()> {
        self.send_message(conn_id, &Message::LayerClose(LayerClose::new(handle)))
    }
}

// ---------------------------------------------------------------------------
// 单元测试（纯函数 + 隔离目录的 socket 级集成）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn sandbox(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ninja_plug_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ------------------------------------------------------------------
    // hit 识别纯函数
    // ------------------------------------------------------------------

    #[test]
    fn token_extraction_at_click_col() {
        let line = "  src/main.rs:42:13  other.txt  ";
        // 点在 's'（col 2）→ 整个 src/main.rs:42:13。
        let got = line_token_at(line, 2);
        assert_eq!(got.as_ref().map(|(t, s)| (t.as_str(), *s)), Some(("src/main.rs:42:13", 2)));
        // 点在路径中间也拿整个 token。
        let got = line_token_at(line, 6);
        assert_eq!(got.as_ref().map(|(t, s)| (t.as_str(), *s)), Some(("src/main.rs:42:13", 2)));
        // 点在空白处 → None。
        assert!(line_token_at(line, 0).is_none());
        assert!(line_token_at(line, 19).is_none());
        // CJK 行：token 化会拿到「你好」（alphanumeric），但分类层
        // 不认（无路径样式）——噪声不出宿主。
        assert!(classify_token(&line_token_at("你好 世界", 0).unwrap().0).is_none());
    }

    #[test]
    fn token_classification() {
        assert_eq!(classify_token("/abs/a.rs"), Some(HitKind::Path));
        assert_eq!(classify_token("src/main.rs"), Some(HitKind::Path));
        assert_eq!(classify_token("./rel.c"), Some(HitKind::Path));
        assert_eq!(classify_token("~/x/y.md"), Some(HitKind::Path));
        assert_eq!(classify_token("notes.txt"), Some(HitKind::Path));
        assert_eq!(classify_token("https://x.io/a?b=1"), Some(HitKind::Url));
        assert_eq!(classify_token("file:///tmp/a"), Some(HitKind::Url));
        // 纯单词/太短不认（不给插件发噪声）。
        assert_eq!(classify_token("hello"), None);
        assert_eq!(classify_token("a"), None);
        assert_eq!(classify_token("run."), None);
    }

    #[test]
    fn url_classification_for_open_url_action() {
        assert_eq!(classify_url("https://ghostty.org"), HitKind::Url);
        // file:// 归 path：pager 只认领 path，不能落到系统 open。
        assert_eq!(classify_url("file:///tmp/a.txt"), HitKind::Path);
        assert_eq!(classify_url("myapp://deep/link"), HitKind::Osc8);
        // 无 scheme：ghostty resolvePathForOpening 已解析的文件路径 → path
        //（⌘+click 路径的主数据源，ninja-preview 只认领 path）。
        assert_eq!(classify_url("/tmp/nq3p/sample.txt"), HitKind::Path);
        assert_eq!(classify_url("~/notes.md"), HitKind::Path);
    }

    #[test]
    fn file_url_and_osc7_become_fs_paths() {
        assert_eq!(
            file_url_to_fs_path("file:///Users/jal/src").as_deref(),
            Some("/Users/jal/src")
        );
        assert_eq!(
            file_url_to_fs_path("file://localhost/tmp/a").as_deref(),
            Some("/tmp/a")
        );
        assert_eq!(
            file_url_to_fs_path("file:///Users/foo%20bar").as_deref(),
            Some("/Users/foo bar")
        );
        assert_eq!(file_url_to_fs_path("/tmp/a"), None);
        assert_eq!(file_url_to_fs_path("https://x.io/a"), None);
        assert_eq!(normalize_cwd("file:///Users/jal"), "/Users/jal");
        assert_eq!(normalize_cwd("/Users/jal"), "/Users/jal");
        let (k, t) = normalize_open_payload(HitKind::Url, "file:///tmp/a.txt");
        assert_eq!(k, HitKind::Path);
        assert_eq!(t, "/tmp/a.txt");
    }

    // ------------------------------------------------------------------
    // layer 几何
    // ------------------------------------------------------------------

    #[test]
    fn overlay_rect_anchor_semantics() {
        // 锚点行在上半（下方放得下 1/4 屏）→ 往下开，至多半屏。
        let (x, y, w, h) = overlay_rect(10, 0, (8.0, 18.0), (590.0, 390.0));
        assert!((y - 10.0 * 18.0).abs() < 0.01, "y 锚在点击行");
        assert!(h <= 195.0 + 0.01 && h >= 64.0, "至多半屏");
        assert_eq!(w, 590.0);
        let _ = x;

        // 锚点行贴近底部（下方不足 1/4 屏）→ 向上开。
        let (_, y2, _, h2) = overlay_rect(21, 0, (8.0, 18.0), (590.0, 390.0));
        assert!(y2 < 21.0 * 18.0, "向上开");
        let _ = h2;

        // 视图极小 → 64pt 下限防退化。
        let (_, _, w3, h3) = overlay_rect(0, 0, (8.0, 18.0), (10.0, 10.0));
        assert!(w3 >= 64.0 && h3 >= 64.0);
    }

    // ------------------------------------------------------------------
    // theme.set 校验
    // ------------------------------------------------------------------

    fn sample_theme() -> ThemeSet {
        let ansi = std::array::from_fn::<String, 16, _>(|i| format!("#00{i:02x}00"));
        ThemeSet::new("t", "#101010", "#202020", "#303030", "#404040", 128, "#505050", ansi)
    }

    #[test]
    fn theme_conf_is_explicit_ghostty_config() {
        let text = theme_conf_text(&sample_theme()).unwrap();
        assert!(text.contains("background = #101010"));
        assert!(text.contains("foreground = #202020"));
        assert!(text.contains("cursor-color = #303030"));
        // 选区合成：0.5×#404040 + 0.5×#101010 = #282828。
        assert!(text.contains("selection-background = #282828"));
        for i in 0..16 {
            assert!(text.contains(&format!("palette = {i}=")));
        }
    }

    #[test]
    fn theme_invalid_values_rejected_whole() {
        let mut m = sample_theme();
        m.bg = "#123".into(); // 短写
        assert!(theme_conf_text(&m).is_none());
        m.bg = "0x112233".into(); // 前缀错
        assert!(theme_conf_text(&m).is_none());
        m.bg = "#11223g".into(); // 非十六进制
        assert!(theme_conf_text(&m).is_none());
        m.bg = "#112233".into();
        m.selection_alpha = 256; // alpha 越界
        assert!(theme_conf_text(&m).is_none());
        m.selection_alpha = 255; // 边界合法
        assert!(theme_conf_text(&m).is_some());
        m.ansi[3] = "red".into();
        assert!(theme_conf_text(&m).is_none());
        // 大写十六进制可收。
        m.ansi[3] = "#AABBCC".into();
        assert!(theme_conf_text(&m).is_some());
    }

    // ------------------------------------------------------------------
    // 键名映射
    // ------------------------------------------------------------------

    #[test]
    fn key_name_roundtrip() {
        assert_eq!(key_name_to_code("esc"), Some(53));
        assert_eq!(key_name_to_code("enter"), Some(36));
        assert_eq!(key_name_to_code("p"), Some(0x23));
        assert_eq!(key_name_to_code("1"), Some(0x13));
        assert_eq!(key_name_to_code("f12"), Some(0x6F));
        assert_eq!(key_name_to_code("multi char"), None);
        assert_eq!(key_name_to_code(""), None);
        assert_eq!(code_to_key_name(53, None), "esc");
        assert_eq!(code_to_key_name(0x23, Some('P')), "p");
        // 命名表外 + 无可显字符 → key<code>（不猜）。
        assert_eq!(code_to_key_name(40, Some('\u{f702}')), "key40");
        assert_eq!(code_to_key_name(40, Some('X')), "x");
    }

    // ------------------------------------------------------------------
    // socket 清扫 / 二进制解析 / footprint
    // ------------------------------------------------------------------

    #[test]
    fn sweep_removes_only_dead_pid_sockets() {
        let dir = sandbox("sweep");
        // 死 pid：拉一个真子进程收尸。
        let mut dead = std::process::Command::new("/bin/sleep")
            .arg("0")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let dead_pid = dead.id() as i32;
        let _ = dead.wait();
        // 活 pid：本进程。
        let mine = std::process::id() as i32;
        let dead_sock = dir.join(format!("ninja-ade-{dead_pid}.sock"));
        std::fs::write(&dead_sock, b"").unwrap();
        let alive_sock = dir.join(format!("ninja-ade-{mine}.sock"));
        std::fs::write(&alive_sock, b"").unwrap();
        let other = dir.join("ninja-ade-garbage.sock");
        std::fs::write(&other, b"").unwrap();
        sweep_stale_sockets_in(&dir);
        assert!(!dead_sock.exists(), "死 pid 的 socket 必须清");
        assert!(alive_sock.exists(), "活 pid 的不动");
        assert!(other.exists(), "非约定名不碰");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_plugin_binary_segments() {
        let dir = sandbox("resolve");
        let user_dir = dir.join("user-plugins");
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::write(user_dir.join("good"), b"#!/bin/sh\n").unwrap();
        let mut cfg = PluginsConfig::default();
        cfg.paths.insert(
            "explicit".into(),
            user_dir.join("good").to_string_lossy().to_string(),
        );
        // 显式路径段。
        assert_eq!(
            resolve_plugin_binary_in("explicit", &cfg, Some(&user_dir)).map(|p| p.is_file()),
            Some(true)
        );
        // 用户目录段。
        assert!(resolve_plugin_binary_in("good", &PluginsConfig::default(), Some(&user_dir)).is_some());
        // 不存在 / 名字带斜杠 / 空 → None。
        assert!(resolve_plugin_binary_in("nope", &cfg, Some(&user_dir)).is_none());
        assert!(resolve_plugin_binary_in("a/b", &cfg, Some(&user_dir)).is_none());
        assert!(resolve_plugin_binary_in("", &cfg, Some(&user_dir)).is_none());
        assert!(resolve_plugin_binary_in("good", &PluginsConfig::default(), None).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn footprint_reads_own_pid() {
        // 口径冒烟：本进程能读出非零 footprint（尺寸坑的回归防线——
        // 缓冲短了内核写穿是 SIGBUS，读出非零说明布局对）。
        let v = footprint_bytes(std::process::id()).expect("own footprint");
        assert!(v > 1024 * 1024, "宿主进程 footprint 应 >1MiB，实得 {v}");
    }

    // ------------------------------------------------------------------
    // socket 级集成（python3 最小插件；无 GUI）
    // ------------------------------------------------------------------

    /// 最小 ADE 插件脚本：连 $NINJA_ADE_SOCK → 收 hit 帧 → 按 mode 回
    /// hit.ignore / hit.claim。
    const PLUGIN_PY: &str = r#"
import json, os, socket, struct, sys
mode = os.environ.get("NINJA_FAKE_MODE", "ignore")
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
for _ in range(200):
    try:
        s.connect(os.environ["NINJA_ADE_SOCK"])
        break
    except OSError:
        import time; time.sleep(0.05)
raw = b""
while True:
    chunk = s.recv(4096)
    if not chunk:
        sys.exit(0)
    raw += chunk
    while len(raw) >= 4:
        (n,) = struct.unpack_from("<I", raw)
        if len(raw) < 4 + n:
            break
        msg = json.loads(raw[4:4+n].decode("utf-8"))
        raw = raw[4+n:]
        if msg.get("type") == "hit":
            reply_type = "hit.claim" if mode == "claim" else "hit.ignore"
            reply = {"type": reply_type, "v": 0, "id": msg["id"]}
            if mode == "claim":
                reply["priority"] = 7
            out = json.dumps(reply, separators=(",", ":")).encode()
            s.sendall(struct.pack("<I", len(out)) + out)
"#;

    /// 造一个可执行 python 插件脚本（行为模式经 NINJA_FAKE_MODE 传给
    /// 子进程——spawn_one 不带 argv）。
    fn fake_plugin(dir: &Path, tag: &str) -> PathBuf {
        let f = dir.join(format!("plug_{tag}.py"));
        std::fs::write(&f, format!("#!/usr/bin/env python3\n{PLUGIN_PY}\n")).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        f.canonicalize().unwrap_or(f)
    }

    /// python3 缺席时跳过集成段（CI 环境）。
    fn python_ok() -> bool {
        std::process::Command::new("python3")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn dispatch_hit_full_cycle_and_lifecycle() {
        if !python_ok() {
            eprintln!("skip: 无 python3");
            return;
        }
        let dir = sandbox("cycle");
        // socket 路径必须短（sun_path ≤104）。
        let sock = std::env::temp_dir().join(format!("np_{}.sock", std::process::id()));
        // SAFETY: 测试进程内无并发读 env 的线程（set_var 的 Rust 2024 契约）。
        unsafe { std::env::set_var("NINJA_FAKE_MODE", "claim") };
        let script = fake_plugin(&dir, "cycle");
        let mut cfg = PluginsConfig {
            enabled: vec!["fake".into()],
            paths: std::collections::HashMap::from([(
                "fake".into(),
                script.to_string_lossy().to_string(),
            )]),
        };
        let mut host = PluginHost::bind(sock.clone(), cfg.clone()).expect("bind");
        // 首击冷启动：无连接 → 兜底拉起 → claim priority 7。
        let hit = Hit::new(1, HitKind::Path, "/tmp/a.rs", "", 3, 2, 1, vec![Modifier::Cmd]);
        let out = host.dispatch_hit_with_timeout(
            &hit,
            Duration::from_millis(3000),
            None, // 无 GUI：跳过层握手
        );
        assert_eq!(out, DispatchOutcome::Claimed { priority: 7 }, "claim 必须仲裁出来");
        // 子进程活着 + 快照可见。
        let snap = host.snapshot();
        assert!(snap.iter().any(|s| s.name == "fake" && s.running && s.enabled), "{snap:?}");
        // ignore 模式：再造一个 ignore 插件（第二个 host 段）。
        drop(host);
        // SAFETY: 同上。
        unsafe { std::env::set_var("NINJA_FAKE_MODE", "ignore") };
        let script2 = fake_plugin(&dir, "ig");
        cfg.paths.insert("fake".into(), script2.to_string_lossy().to_string());
        let mut host = PluginHost::bind(sock.clone(), cfg).expect("bind2");
        let hit = Hit::new(1, HitKind::Path, "/tmp/a.rs", "", 3, 2, 1, vec![Modifier::Cmd]);
        let out = host.dispatch_hit_with_timeout(&hit, Duration::from_millis(3000), None);
        assert_eq!(out, DispatchOutcome::AllIgnored);
        // 禁用：幂等回收（杀子进程 + 断连 + 删 socket）。
        host.session_disable("fake");
        assert!(!sock.exists(), "禁用后 socket 文件必须删");
        assert!(!pgrep_fake(), "禁用后无插件进程");
        // 快照不再有 running 行。
        let snap = host.snapshot();
        assert!(snap.iter().all(|s| !s.running), "{snap:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 佐料：确认没有遗留的 fake 插件 python 进程（按 socket env 特征
    /// 搜不了，这里用「没有任何 plug_ 脚本进程」近似——脚本路径唯一）。
    fn pgrep_fake() -> bool {
        let out = std::process::Command::new("pgrep")
            .arg("-f")
            .arg("plug_cycle.py")
            .output();
        match out {
            Ok(o) => !o.stdout.is_empty(),
            Err(_) => false,
        }
    }

    #[test]
    fn start_with_empty_enabled_is_none() {
        // 空载门禁：enabled 空 → 不 bind（返回 None，零 socket）。
        assert!(PluginHost::start(&PluginsConfig::default()).is_none());
        assert!(!socket_path().exists(), "空载不得创建 socket 文件");
    }

    #[test]
    fn version_gate_kills_connection_not_host() {
        // 坏协议（错版本）连接：分发路径断开它并按 ignore 降级，宿主不炸。
        if !python_ok() {
            eprintln!("skip: 无 python3");
            return;
        }
        let dir = sandbox("badv");
        let sock = std::env::temp_dir().join(format!("nb_{}.sock", std::process::id()));
        // 脚本：连上后直接写一条 v=1 的 hit 回执帧。
        let script = dir.join("bad.py");
        std::fs::write(
            &script,
            r#"#!/usr/bin/env python3
import json, os, socket, struct, time
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
for _ in range(200):
    try:
        s.connect(os.environ["NINJA_ADE_SOCK"]); break
    except OSError:
        time.sleep(0.05)
bad = b'{"type":"hit.claim","v":1,"id":1,"priority":9}'
s.sendall(struct.pack("<I", len(bad)) + bad)
time.sleep(30)
"#,
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = PluginsConfig {
            enabled: vec!["bad".into()],
            paths: std::collections::HashMap::from([(
                "bad".into(),
                script.to_string_lossy().to_string(),
            )]),
        };
        let mut host = PluginHost::bind(sock.clone(), cfg).expect("bind");
        let hit = Hit::new(1, HitKind::Path, "/tmp/x", "", 0, 0, 1, vec![]);
        let out = host.dispatch_hit_with_timeout(&hit, Duration::from_millis(3000), None);
        assert_eq!(out, DispatchOutcome::AllIgnored, "错版本回执按 ignore 降级");
        assert!(host.conns.is_empty(), "坏协议连接必须已断开");
        host.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
