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
//! 热重载管线全 surface 传播。插件连接死亡/禁用 → 删层重载，回 Ghostty/
//! 用户配置基线。
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
//! 进入）、冷启动 connect（2s，只发生在首击兜底）。异步消息走
//! **读事件源**（CFSocket 挂 listener 与每条连接，主 runloop）：fd
//! 可读才唤醒，空闲零轮询；配置监视拍（0.5s）作全量排干与 pane
//! 快照/内存上限的慢拍兜底。
mod binary;
mod classify;
mod layer;

pub(crate) use binary::{ade_debug, effective_socket_path};
pub use binary::{
    discover_plugin_names, footprint_bytes, resolve_plugin_binary, sweep_stale_sockets,
    user_plugin_dir,
};
#[cfg(test)]
use binary::{resolve_plugin_binary_in, socket_path, sweep_stale_sockets_in};
pub use classify::{
    classify_token, classify_url, code_to_key_name, key_name_to_code, line_token_at,
    modifiers_from_mods, normalize_cwd, normalize_open_payload, theme_conf_text,
};
pub(crate) use classify::{cwd_for_view, hotkey_to_key_event};
#[cfg(test)]
use classify::{file_url_to_fs_path, overlay_rect};
pub use layer::{LayerGeom, layer_tab_closed};
pub(crate) use layer::{
    layer_close, layer_close_all, layer_close_by_conn, layer_close_pane, layer_foreground,
    layer_load_html, layer_open, layer_post_msg, layer_present,
};

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ninja_protocol::frame::{FrameDecoder, encode_frame};
use ninja_protocol::{
    Hit, HitKind, InputHotkey, InputHotkeyDenied, InputHotkeyGranted, InputKey, LayerClose,
    Message, Modifier, PaneInfo, PaneInput, PaneSnapshot, Surface, ThemeSet,
};

use objc2::rc::Retained;

use objc2_foundation::NSPoint;

use crate::surface::SurfaceHostView;

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

/// 监督器视角的 `[plugins]` 配置（crate::config 解析；这里换 HashMap
/// 便于按名解析二进制）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginsConfig {
    /// 启用的插件名列表。空 = 关（空载门禁）。启用即拉起。
    pub enabled: Vec<String>,
    /// 插件名 → 二进制路径（缺省时按名多段解析，见 [`resolve_plugin_binary`]）。
    pub paths: std::collections::HashMap<String, String>,
    /// 单插件物理足迹上限（字节）。0 = 不限；缺省取
    /// [`DEFAULT_PLUGIN_MEMORY_LIMIT_MB`]（PRODUCT：「内存有上限」）。
    pub memory_limit_bytes: u64,
}

/// 缺省单插件内存上限（MiB）。超大值只约束失控插件，不碰正常 pager。
pub const DEFAULT_PLUGIN_MEMORY_LIMIT_MB: u64 = 512;

impl From<&crate::config::PluginsConfig> for PluginsConfig {
    fn from(c: &crate::config::PluginsConfig) -> Self {
        Self {
            enabled: c.enabled.clone(),
            paths: c.paths.iter().cloned().collect(),
            memory_limit_bytes: match c.memory_limit_mb {
                Some(mb) => mb * 1024 * 1024,
                None => DEFAULT_PLUGIN_MEMORY_LIMIT_MB * 1024 * 1024,
            },
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
        ghostty_sys::ghostty_surface_text(surface, m.text.as_ptr().cast(), m.text.len());
    }
}

/// 当前生效的插件主题覆盖（config.rs 装载管线消费；None = 无覆盖）。
/// 内容 = (色板名, 层文件文本)。拥有者连接死亡/禁用 →
/// [`revoke_theme_override`]。
pub fn plugin_theme_override() -> Option<(String, String)> {
    THEME_OVERRIDE.lock().ok().and_then(|s| {
        s.as_ref()
            .map(|(name, text, _)| (name.clone(), text.clone()))
    })
}

fn theme_owner() -> Option<u64> {
    THEME_OVERRIDE
        .lock()
        .ok()
        .and_then(|s| s.as_ref().map(|(_, _, conn)| *conn))
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
    /// 上次内存采样时刻（1s 冷却，避免 150ms 泵拍每拍 syscall）。
    last_mem_check: Option<Instant>,
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
    /// 该连接的读事件源（accept 时挂、断连时摘；仅主线程）。非主线程
    /// 场景（单测）为 None——靠直调泵。
    src: Option<ConnSource>,
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
/// 路径上花；预算耗尽 = 放弃等 present（层仍开着，靠读源/慢拍兜）。
pub const LAYER_HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(1500);

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
                    last_mem_check: None,
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

    /// 监听 fd（读事件源挂载用）。
    fn listener_fd(&self) -> std::os::unix::io::RawFd {
        use std::os::unix::io::AsRawFd;
        self.listener.as_raw_fd()
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
                self.bin_mtime.insert(
                    name.to_string(),
                    std::fs::metadata(&bin).and_then(|m| m.modified()).ok(),
                );
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
    /// （[`PluginHost::session_enable`]）都汇聚到这里。拉起前挂监听
    /// 读源（[`ensure_socket_sources`]）：插件 connect 即唤醒 accept，
    /// 连接即推的 theme.set 由读源消化。
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
            ensure_socket_sources();
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
        ensure_socket_sources();
        true
    }

    /// 面板开关「关」：名字出会话 enabled 名单，立即杀它名下的子进程，
    /// 并排干 EOF（收层/回退色板与插件死亡同一条通路：pump 摄连接
    /// EOF → [`PluginHost::drop_conn`]）。名单清空即整个插件面关掉
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
        ensure_socket_sources();
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

    /// 内存上限执行（PRODUCT「内存有上限」）：按 [`footprint_bytes`] 口径
    /// 采样每个子进程物理足迹，超限即 kill + wait + 记
    /// [`PluginHost::spawn_errors`]（面板显示「已停止（超内存上限…）」）。
    /// 层/色板回收走插件死亡同一条路（EOF → 泵 → drop_conn）。
    /// `memory_limit_bytes == 0` = 不限。`force` 绕过 1s 采样冷却
    /// （面板刷新/测试用；常规调用方：泵拍与配置监视拍，频率 150ms/0.5s
    /// 都被冷却压到 ~1s 一次，`proc_pid_rusage` 成本可忽略）。
    fn enforce_memory_limits(&mut self, force: bool) {
        if self.cfg.memory_limit_bytes == 0 || self.children.is_empty() {
            return;
        }
        if !force
            && self
                .last_mem_check
                .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        self.last_mem_check = Some(Instant::now());
        let limit = self.cfg.memory_limit_bytes;
        let offenders: Vec<(String, u64)> = self
            .children
            .iter()
            .filter_map(|(name, child)| {
                footprint_bytes(child.id())
                    .filter(|&u| u > limit)
                    .map(|u| (name.clone(), u))
            })
            .collect();
        if offenders.is_empty() {
            return;
        }
        for (name, usage) in offenders {
            let reason = format!(
                "超内存上限（{:.1} MB > 限 {:.0} MB）",
                usage as f64 / 1e6,
                limit as f64 / 1e6
            );
            eprintln!("ninja: 插件 {name:?} {reason}，已杀");
            if let Some(i) = self.children.iter().position(|(n, _)| *n == name) {
                let (_, mut c) = self.children.remove(i);
                let _ = c.kill();
                let _ = c.wait();
            }
            self.spawn_errors.insert(name, reason);
        }
        // 同步排干死亡连接：层/色板当场回收，不等下一拍。
        self.pump_plugins();
    }

    /// 状态快照（面板/测试）：enabled 名单 ∪ 有子进程 ∪ 有错误记录的
    /// 名字，逐名报告 启用/在跑/pid/内存/最后错误。顺带收割已退出的
    /// 子进程（try_wait）并把异常退出记进 last_error。
    pub fn snapshot(&mut self) -> Vec<PluginStatus> {
        self.enforce_memory_limits(true);
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
        names.extend(discover_plugin_names(&self.cfg));
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
                    let src = {
                        use std::os::unix::io::AsRawFd;
                        add_read_source(stream.as_raw_fd())
                    };
                    self.conns.push(Conn {
                        id: self.next_conn_id,
                        stream,
                        decoder: FrameDecoder::new(),
                        src,
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
            let can_spawn = self.cfg.enabled.iter().any(|n| !self.spawned.contains(n));
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
        ade_debug(&format!(
            "dispatch: claim priority={priority} conn={claim_conn}"
        ));
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
            if self.conns[conn_idx]
                .stream
                .set_read_timeout(Some(rem))
                .is_err()
            {
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
                    break; // 静默超预算：不再等
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
                            HandshakeStep::Presented
                        } else {
                            HandshakeStep::Continue
                        }
                    }
                    None => {
                        eprintln!("ninja: 层分配失败（IOSurface/视图），拒层");
                        let f =
                            encode_frame(&Message::LayerClose(LayerClose::new(0))).expect("编码");
                        let _ = self.conns[conn_idx].stream.write_all(&f);
                        HandshakeStep::Continue
                    }
                }
            }
            Ok(Some(Message::LayerPresent(m))) => {
                layer_present(m.layer);
                HandshakeStep::Presented
            }
            Ok(Some(Message::LayerHtml(m))) => {
                layer_load_html(m.layer, &m.html);
                HandshakeStep::Presented
            }
            Ok(Some(Message::LayerClose(m))) => {
                layer_close(m.layer);
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
                    let _ = c
                        .stream
                        .write_all(&encode_frame(&reply).expect("hotkey 回执编码"));
                }
            }
            Message::LayerClose(m) => {
                layer_close(m.layer);
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
            if conn
                .stream
                .set_read_timeout(Some(Duration::from_millis(1)))
                .is_err()
            {
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
        let Some(c) = self.conns.get_mut(idx) else {
            return;
        };
        let conn_id = c.id;
        if let Some(src) = c.src.take() {
            remove_conn_source(&src);
        }
        self.conns.remove(idx);
        self.hotkeys.retain(|g| g.conn != conn_id);
        if layer_close_by_conn(conn_id) {
            ade_debug(&format!("conn {conn_id} 死亡：已回收其全部层"));
        }
        if theme_owner() == Some(conn_id) && revoke_theme_override() {
            eprintln!("ninja: 主题插件连接 {conn_id} 死亡，色板回退内置/用户基线");
            crate::host::schedule_reload("theme-revoke");
        }
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
        for c in &mut self.conns {
            if let Some(src) = c.src.take() {
                remove_conn_source(&src);
            }
        }
        remove_listener_source();
        self.conns.clear();
        std::thread::sleep(Duration::from_millis(80));
        for (_name, c) in self.children.iter_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.children.clear();
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
// 泵 timer（层/主题覆盖/等连接期间存在；主 runloop）
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 读事件源（CFSocket；事件驱动，空闲零唤醒）
// ---------------------------------------------------------------------------

/// 一对 CF 引用（socket + 它的 runloop source）。CF 类型不自动 Send；
/// 只在主线程创建/移除，static 与 Conn 字段要求手工标注。
struct ConnSource(
    objc2_core_foundation::CFRetained<objc2_core_foundation::CFSocket>,
    objc2_core_foundation::CFRetained<objc2_core_foundation::CFRunLoopSource>,
);

unsafe impl Send for ConnSource {}

impl std::fmt::Debug for ConnSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnSource").finish_non_exhaustive()
    }
}

/// 监听器读源（新连接事件）。挂在主 runloop：只在有插件 connect 时醒。
static LISTENER_SRC: Mutex<Option<ConnSource>> = Mutex::new(None);

/// 读回调（listener 与每条 conn 共用；主 runloop）。与旧 150ms 泵同一
/// 条入口 [`pump_now`]：accept + 全量排干，幂等，try_lock 防嵌套死锁。
extern "C-unwind" fn socket_readable(
    _sock: *mut objc2_core_foundation::CFSocket,
    _kind: objc2_core_foundation::CFSocketCallBackType,
    _addr: *const objc2_core_foundation::CFData,
    _data: *const std::ffi::c_void,
    _info: *mut std::ffi::c_void,
) {
    pump_now();
}

/// 给 fd 挂 CFSocket 读源并加进主 runloop。只在主线程挂（非主线程
/// 调用 → None：单测走直调泵，不依赖 runloop）。flags 只带自动重挂
/// （kCFSocketAutomaticallyReenableReadCallBack）；**不带**
/// kCFSocketCloseOnInvalidate——fd 由 Rust 侧的 UnixStream/UnixListener
/// 独占持有，CFSocket 不许代关。
fn add_read_source(fd: std::os::unix::io::RawFd) -> Option<ConnSource> {
    let mtm = objc2::MainThreadMarker::new()?;
    let _ = mtm;
    use objc2_core_foundation::{CFRunLoop, CFSocket, CFSocketContext};
    let main = CFRunLoop::main()?;
    let ctx = CFSocketContext {
        version: 0,
        info: std::ptr::null_mut(),
        retain: None,
        release: None,
        copyDescription: None,
    };
    // SAFETY: ctx 布局正确；callout 只调 pump_now（try_lock，主线程）；
    // fd 归调用方所有（flags 不带 CloseOnInvalidate）。
    let sock = unsafe {
        CFSocket::with_native(
            None,
            fd,
            objc2_core_foundation::CFSocketCallBackType::ReadCallBack.bits(),
            Some(socket_readable),
            &ctx,
        )
    }?;
    sock.set_socket_flags(objc2_core_foundation::kCFSocketAutomaticallyReenableReadCallBack);
    let src = CFSocket::new_run_loop_source(None, Some(&sock), 0)?;
    // SAFETY: 读 extern 常量字符串静态（CF 已随进程初始化）。
    let mode = unsafe { objc2_core_foundation::kCFRunLoopCommonModes };
    main.add_source(Some(&src), mode);
    Some(ConnSource(sock, src))
}

/// 摘源 + 失效（fd 仍归 Rust 所有；与 add 的主线程分支对称）。
fn remove_conn_source(src: &ConnSource) {
    use objc2_core_foundation::CFRunLoop;
    if objc2::MainThreadMarker::new().is_none() {
        return; // 非主线程没挂过源
    }
    if let Some(main) = CFRunLoop::main() {
        // SAFETY: 同上。
        let mode = unsafe { objc2_core_foundation::kCFRunLoopCommonModes };
        main.remove_source(Some(&src.1), mode);
    }
    src.0.invalidate();
}

/// 起监听读源（幂等）：拉起插件前由各路径调用（旧泵 timer 的替代）。
/// 空闲成本为零——没有 connect 就没有唤醒。
pub(crate) fn ensure_socket_sources() {
    let mut slot = match LISTENER_SRC.lock() {
        Ok(s) => s,
        Err(_) => return,
    };
    if slot.is_some() {
        return;
    }
    let Some(host) = take_dispatcher() else {
        return;
    };
    let Ok(h) = host.try_lock() else {
        return;
    };
    let fd = h.listener_fd();
    drop(h);
    if let Some(pair) = add_read_source(fd) {
        *slot = Some(pair);
    }
}

/// 摘监听源（shutdown：host 关闭/重绑时）。
fn remove_listener_source() {
    if let Ok(mut slot) = LISTENER_SRC.lock()
        && let Some(pair) = slot.take()
    {
        remove_conn_source(&pair);
    }
}

/// 泵入口（timer 回调直调；测试可直调）。
pub fn pump_now() {
    // try_lock：点击握手若正握着同一把锁，嵌套回调不能再阻塞。
    if let Some(host) = take_dispatcher()
        && let Ok(mut h) = host.try_lock()
    {
        h.pump_plugins();
        h.enforce_memory_limits(false);
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
        && let Ok(mut slot) = DISPATCHER.lock()
    {
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

/// 配置监视拍（0.5s，恒在跑）兼任监督器慢拍：全量排干兜底 + mtime
/// 热重载 + 内存上限（1s 冷却）。事件源管即时性，这里管保底与覆盖
/// 未连上 socket 的子进程——正确性不依赖它，延迟兜底靠它。
pub fn watch_plugin_binaries() {
    if let Some(host) = take_dispatcher()
        && let Ok(mut h) = host.try_lock()
    {
        h.pump_plugins();
        h.respawn_stale_plugins();
        h.enforce_memory_limits(false);
    }
}

/// 状态接线：全部插件的状态快照（面板与测试用）。无分发器 → 空表。
pub fn status_snapshot() -> Vec<PluginStatus> {
    match take_dispatcher() {
        Some(host) => host.lock().map(|mut h| h.snapshot()).unwrap_or_default(),
        // 空载（未启用任何插件）：面板仍能看见已装插件（发现只读目录，
        // 不建 socket、不碰空载不变量）。
        None => {
            let cfg = session_cfg();
            let mut names: std::collections::BTreeSet<String> =
                cfg.enabled.iter().cloned().collect();
            names.extend(discover_plugin_names(&cfg));
            names
                .into_iter()
                .map(|name| PluginStatus {
                    enabled: cfg.enabled.iter().any(|n| n == &name),
                    running: false,
                    pid: None,
                    memory_bytes: None,
                    last_error: None,
                    name,
                })
                .collect()
        }
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
/// - off：名字出名单，立即杀进程/收层/断连/撤色板；名单空即整个
///   关掉（shutdown：删 socket，回空载）。
///
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
            resolved
                .exists()
                .then(|| resolved.to_string_lossy().to_string())
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
    Some((row.min(sz.rows as u32 - 1), col.min(sz.columns as u32 - 1)))
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
        view: unsafe { Retained::retain(std::ptr::from_ref(view) as *mut SurfaceHostView) }
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
pub fn key_route(
    view: &SurfaceHostView,
    keycode: u16,
    mods: ghostty_sys::ghostty_input_mods_e,
    chars: Option<String>,
) -> bool {
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
        let msg = Message::InputKey(InputKey::new(
            layer,
            key,
            chars.unwrap_or_default(),
            proto_mods,
        ));
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
        host.lock()
            .ok()
            .and_then(|h| h.hotkey_owner(&key, &proto_mods))
    });
    if let Some(conn) = grant {
        let msg = Message::InputKey(InputKey::new(0, key, chars.unwrap_or_default(), proto_mods));
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
    pub(crate) fn send_message(&mut self, conn_id: u64, msg: &Message) -> std::io::Result<()> {
        let frame = encode_frame(msg).map_err(|e| std::io::Error::other(format!("encode: {e}")))?;
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
        assert_eq!(
            got.as_ref().map(|(t, s)| (t.as_str(), *s)),
            Some(("src/main.rs:42:13", 2))
        );
        // 点在路径中间也拿整个 token。
        let got = line_token_at(line, 6);
        assert_eq!(
            got.as_ref().map(|(t, s)| (t.as_str(), *s)),
            Some(("src/main.rs:42:13", 2))
        );
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
        assert!((64.0..=195.0 + 0.01).contains(&h), "至多半屏");
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
        ThemeSet::new(
            "t", "#101010", "#202020", "#303030", "#404040", 128, "#505050", ansi,
        )
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
        assert!(
            resolve_plugin_binary_in("good", &PluginsConfig::default(), Some(&user_dir)).is_some()
        );
        // 不存在 / 名字带斜杠 / 空 → None。
        assert!(resolve_plugin_binary_in("nope", &cfg, Some(&user_dir)).is_none());
        assert!(resolve_plugin_binary_in("a/b", &cfg, Some(&user_dir)).is_none());
        assert!(resolve_plugin_binary_in("", &cfg, Some(&user_dir)).is_none());
        assert!(resolve_plugin_binary_in("good", &PluginsConfig::default(), None).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn memory_limit_kills_runaway_plugin() {
        // /usr/bin/yes：向 null stdout 狂写，常驻、footprint 远超 64KB。
        let dir = sandbox("memlimit");
        let mut cfg = PluginsConfig {
            memory_limit_bytes: 64 * 1024,
            ..Default::default()
        };
        cfg.enabled.push("hog".into());
        cfg.paths.insert("hog".into(), "/usr/bin/yes".into());
        let mut host = PluginHost::bind(dir.join("m.sock"), cfg).expect("bind");
        host.spawn_enabled_now();
        let pid = host
            .children
            .iter()
            .find(|(n, _)| n == "hog")
            .map(|(_, c)| c.id())
            .expect("hog 应已拉起");
        // 等 footprint 可采样（进程刚起时内核账本可能还没就位）。
        for _ in 0..50 {
            if footprint_bytes(pid).is_some_and(|u| u > 0) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        host.enforce_memory_limits(true);
        assert!(
            !host.children.iter().any(|(n, _)| n == "hog"),
            "超限子进程应被收割"
        );
        let err = host.spawn_errors.get("hog").cloned().unwrap_or_default();
        assert!(err.contains("超内存上限"), "面板应能看到原因：{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn memory_limit_zero_means_unlimited() {
        let dir = sandbox("memunlim");
        let mut cfg = PluginsConfig {
            memory_limit_bytes: 0,
            ..Default::default()
        };
        cfg.enabled.push("hog".into());
        cfg.paths.insert("hog".into(), "/usr/bin/yes".into());
        let mut host = PluginHost::bind(dir.join("m.sock"), cfg).expect("bind");
        host.spawn_enabled_now();
        std::thread::sleep(Duration::from_millis(300));
        host.enforce_memory_limits(true);
        assert!(
            host.children.iter().any(|(n, _)| n == "hog"),
            "limit=0 不限：子进程应存活"
        );
        // 清场（enforce 不会碰它）。
        while let Some(i) = host.children.iter().position(|(n, _)| n == "hog") {
            let (_, mut c) = host.children.remove(i);
            let _ = c.kill();
            let _ = c.wait();
        }
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
            memory_limit_bytes: 0,
        };
        let mut host = PluginHost::bind(sock.clone(), cfg.clone()).expect("bind");
        // 首击冷启动：无连接 → 兜底拉起 → claim priority 7。
        let hit = Hit::new(
            1,
            HitKind::Path,
            "/tmp/a.rs",
            "",
            3,
            2,
            1,
            vec![Modifier::Cmd],
        );
        let out = host.dispatch_hit_with_timeout(
            &hit,
            Duration::from_millis(3000),
            None, // 无 GUI：跳过层握手
        );
        assert_eq!(
            out,
            DispatchOutcome::Claimed { priority: 7 },
            "claim 必须仲裁出来"
        );
        // 子进程活着 + 快照可见。
        let snap = host.snapshot();
        assert!(
            snap.iter()
                .any(|s| s.name == "fake" && s.running && s.enabled),
            "{snap:?}"
        );
        // ignore 模式：再造一个 ignore 插件（第二个 host 段）。
        drop(host);
        // SAFETY: 同上。
        unsafe { std::env::set_var("NINJA_FAKE_MODE", "ignore") };
        let script2 = fake_plugin(&dir, "ig");
        cfg.paths
            .insert("fake".into(), script2.to_string_lossy().to_string());
        let mut host = PluginHost::bind(sock.clone(), cfg).expect("bind2");
        let hit = Hit::new(
            1,
            HitKind::Path,
            "/tmp/a.rs",
            "",
            3,
            2,
            1,
            vec![Modifier::Cmd],
        );
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
            memory_limit_bytes: 0,
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
