//! p3：宿主侧 ADE 插件门（Unix socket，默认关）；p4：命中分发；
//! p5：插件监督器 + 层状态机（open→ready→present/close）+ 层前台输入
//! 路由；p6：**关掉即轻**——插件死亡收层（[`layer::close_by_conn`]）、
//! 同会话禁用（[`PluginHost::shutdown`]，[`Drop`] 复用同一实现）、陈旧
//! socket 清扫（[`sweep_stale_sockets`]）。禁用/退出/崩溃之后：无插件
//! 进程、无 socket、无层，内存回空载。面板 v2（2026-08-29 用户产品
//! 决策）：单一 spawn 策略——**启用即拉起**（宿主启动/面板 on 即时
//! spawn；面板 off 走 p6 同一条幂等生命周期），状态可见
//! （[`PluginHost::snapshot`]：名/启用/在跑/pid/内存/最后错误）。
//!
//! 空载门禁：`[plugins] enabled` 为空（默认）时**不创建 socket 文件、
//! 不拉任何插件进程**——[`PluginHost::start`] 直接返回 `None`，宿主
//! 进程里没有任何插件运行时（验证：`cargo tree -p ninja` 无
//! wasmtime/tokio；默认配置启动后 socket 路径不存在，见
//! `tests/idle_no_plugins.rs` 的运行时取证）。
//!
//! 启用时：绑定 [`socket_path`] 约定的路径并 listen。**启用即拉起**
//! （2026-08-29 用户产品决策，覆盖早期「启用≠常驻」条款）：宿主启动
//! （runloop 就绪后，[`spawn_startup_plugins`]）与面板开关「开」
//! （[`toggle_plugin`]）都立即按名解析二进制（`[plugins] paths` →
//! `$NINJA_PLUGIN_DIR/<name>` → `~/.config/ninja/plugins/<name>`（p7
//! 分发缺省安装位）→ 宿主二进制同目录（开发布局回退）），spawn 并以
//! `NINJA_ADE_SOCK` 告知 socket 路径；解析失败/拉不起 = 该插件降级为
//! 不存在（stderr 一行警告，绝不弹 UI）。
//!
//! 命中分发（p4）：点击时把 [`Hit`] 广播给已连插件（连接由插件连进来，
//! 分发时按需非阻塞 accept），收集 `hit.claim` / `hit.ignore` 回执——
//! 全 ignore / 静默 / 断连一律视为不认领，走系统默认打开。
//!
//! 层状态机（p5）：claim 后继续读认领方连接——`layer.open` → 建
//! IOSurface（跨进程共享，插件往里写像素）→ 回 `layer.ready`；
//! `layer.present` → 合成（layer 注册表 + 渲染器层 pass）；
//! `layer.close`（双向）→ 摘层还焦点。层打开期间主 runloop 上挂一个
//! 150ms 轮询 timer 消化插件异步消息（pump；无层时不存在，空载零开销）。
//!
//! T2 主题原语：插件可推 `theme.set` 换全色板（协议 theme 类，2026-08
//! 用户产品决策）。宿主侧运行时覆盖点在 [`crate::theme`]（渲染/vt/容器
//! 全读「当前生效色板」）；应用即全屏重画（vt 强制 Full 脏，跳帧不吃）；
//! 拥有者连接死亡/禁用 = 回退内置 One Dark Pro 基线（与 p6 收层同语义，
//! 见 [`PluginHost::drop_conn`]/[`PluginHost::shutdown`]）。覆盖生效期间
//! 泵 timer 不停（盯连接死亡，无层也盯）。
//!
//! 超时策略：**同步短超时**——claim 汇集 [`HIT_REPLY_TIMEOUT`]（500ms），
//! 层握手 [`LAYER_HANDSHAKE_TIMEOUT`]（1.5s，只在有插件认领时进入）；
//! 都发生在点击手势路径上的一次性开销，不新增常驻线程；超预算即降级，
//! 绝不卡死主 runloop。

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ninja_protocol::frame::{FrameDecoder, encode_frame};
use ninja_protocol::{Hit, InputKey, LayerClose, Message, Modifier, ThemeSet};

use crate::layer::{self, LayerGeom};

/// 插件拉起时机（2026-08-29 用户产品决策修订）：**单一策略，不分
/// spawn 模式**——enabled 名单里的插件宿主启动即拉起；运行中启用
/// （面板 on）立即拉起；禁用（面板 off）走 p6 幂等 shutdown（杀进程 +
/// revoke 主题/层 + 删连接）。「启用≠常驻」旧条款由本决策废止（见
/// PRODUCT.md）；空载门禁不受影响——默认零插件时依然零 socket 零进程。

/// `[plugins]` 配置（ninja.toml）。默认空 = 插件全关（空载门禁）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginsConfig {
    /// 启用的插件名列表。空 = 关。启用即拉起（宿主启动/面板 on 即时
    /// spawn，见 [`PluginHost::spawn_enabled_now`]）。
    pub enabled: Vec<String>,
    /// 插件名 → 二进制路径（拉起用）。缺省时按名在
    /// `$NINJA_PLUGIN_DIR/<name>` / `~/.config/ninja/plugins/<name>` /
    /// 宿主二进制同目录解析。
    pub paths: std::collections::HashMap<String, String>,
}

/// 一个插件在面板/测试眼里的状态快照（[`PluginHost::snapshot`]）。
/// 「运行中」按宿主拉起的子进程判（try_wait 未退出）；内存是子进程
/// 真实物理足迹（`proc_pid_rusage` 的 ri_phys_footprint，与 footprint
/// 工具同源）。
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

/// 已绑定的 ADE socket 句柄。Drop = [`PluginHost::shutdown`]（幂等）：
/// 收层、断连接、收割子进程、删 socket 文件——正常退出与同会话禁用
/// 走同一通路（p6）。
#[derive(Debug)]
pub struct PluginHost {
    listener: UnixListener,
    path: PathBuf,
    /// p4：已连上的插件连接（分发时按需 accept 进来）。每条连接各带
    /// 一个帧解码器（半帧状态跨读保留）。
    conns: Vec<Conn>,
    /// hit id 发号器（回执配对用；从 1 起，0 留给「未知」）。
    next_hit_id: u64,
    /// conn id 发号器（层条目回程路由用）。
    next_conn_id: u64,
    /// 监督器：已拉起（或已放弃）的插件名。「别再试」语义——外部
    /// 死亡/拉起失败不自动重拉（拉不起/挂死的插件不该拖住空转红线）；
    /// 面板再启用时显式清除重试（[`PluginHost::session_enable`]）。
    spawned: std::collections::BTreeSet<String>,
    /// 拉起的插件进程（带名：面板/状态快照按名对应 pid/内存；Drop
    /// 收割；宿主退出时它们也会因 socket EOF 自退）。
    children: Vec<(String, std::process::Child)>,
    /// 拉起失败的最后原因（按名；面板「最后错误」列）。
    spawn_errors: std::collections::BTreeMap<String, String>,
    /// 配置快照（按名解析二进制用；同会话再启用换新 host 也用它）。
    cfg: PluginsConfig,
    /// 已禁用（同会话禁用钩子/退出路径）。置位后分发/泵/accept 全部
    /// 空转，行为等同未启用（NoPlugins）——p6「关掉即轻」。
    disabled: bool,
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

/// 冷启动（spawn→connect）预算：与回执预算解耦——首次点击才发生，
/// 只约束「等插件进程连上」。release 二进制 spawn+connect <50ms；debug
/// 构建/系统繁忙时可达数百毫秒，太紧会让首击随机降级（E2E 实测）。
/// 超预算 = 本次 NoPlugins 降级（下次点击时插件已连上，正常分发）。
const COLD_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// claim 后层握手（open→ready→present）的同步预算。只在认领方要层的
/// 路径上花；预算耗尽 = 放弃等 present（层仍开着，靠 pump timer 兜）。
pub const LAYER_HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(1500);

/// 层打开期间插件连接的轮询周期（主 runloop timer；无层时不存在）。
const PUMP_INTERVAL: f64 = 0.15;

/// socket 路径约定：`${TMPDIR:-/tmp}/ninja-ade-{pid}.sock`。
pub fn socket_path() -> PathBuf {
    let pid = std::process::id();
    std::env::temp_dir().join(format!("ninja-ade-{pid}.sock"))
}

/// 陈旧 socket 清扫（p6）：宿主 SIGKILL/崩溃时 [`Drop`] 不跑，约定
/// 目录下会留下 `ninja-ade-<pid>.sock` 尸体（bind 只清同路径文件）。
/// 规则：文件名里的 pid 已死（`kill(pid,0)`=ESRCH）才删；活 pid 一律
/// 不动（并行实例，或 pid 被复用——保守不动）。只在启用插件启动时
/// 扫（[`PluginHost::start`]）：空载路径零改动。
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

/// 实际生效路径：`NINJA_ADE_SOCK` 覆盖（拉起插件进程时经同名环境变量
/// 告知路径；测试钩子同途）。
fn effective_socket_path() -> PathBuf {
    match std::env::var_os("NINJA_ADE_SOCK") {
        Some(p) => PathBuf::from(p),
        None => socket_path(),
    }
}

/// 用户级插件目录（p7 分发的缺省安装位）：`~/.config/ninja/plugins`。
/// `HOME` 缺失（异常环境）→ None：该搜索段整体跳过（其余段照常）。
pub fn user_plugin_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/ninja/plugins"))
}

/// 按名解析插件二进制：`[plugins.paths]` 显式路径 → `$NINJA_PLUGIN_DIR/
/// <name>` → `~/.config/ninja/plugins/<name>`（p7 用户缺省安装位）→
/// 宿主二进制同目录 `<name>`。都不存在 → None（调用方降级）。
pub fn resolve_plugin_binary(name: &str, cfg: &PluginsConfig) -> Option<PathBuf> {
    resolve_plugin_binary_in(name, cfg, user_plugin_dir().as_deref())
}

/// [`resolve_plugin_binary`] 的实现核心：用户插件目录可注入（单测用
/// 隔离 HOME，不碰真实 `~/.config`）。段次序见外层文档。
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
    // p7 用户缺省段：分发文档钉的装/卸位（拷进目录 + enabled 即装，
    // 移出 enabled + 删文件即卸——p6 shutdown 保证删文件时无残留）。
    if let Some(dir) = user_dir {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    // 宿主二进制同目录（p5 起的开发布局回退：cargo 把 ninja 与
    // ninja-preview 放同一 target 目录）。分发场景该目录是 .app 的
    // Contents/MacOS/——**在已签名 bundle 里增删文件会破坏签名封条**，
    // 分发链路不承诺此段（文档只教 ~/.config/ninja/plugins）；保留
    // 仅为本地开发布局的无害读取（只探测存在性，不写）。
    if let Ok(exe) = std::env::current_exe() {
        let p = exe.parent()?.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// env 门控的调度调试（stderr 一行一步；取证用，不设不打印）。
fn ade_debug(msg: &str) {
    if std::env::var_os("NINJA_ADE_DEBUG").is_some() {
        eprintln!("ninja[ade]: {msg}");
    }
}

/// 子进程真实物理足迹（字节）：`proc_pid_rusage` 的 ri_phys_footprint
/// （与 footprint 工具同源；libSystem 自带，无新增链接面）。面板内存
/// 列与快照用。进程不在/拒绝 → None。
fn footprint_bytes(pid: u32) -> Option<u64> {
    // rusage_info 公共前缀（SDK sys/resource.h）：uuid[16] + user/system/
    // idle_wkups/interrupt_wkups/pageins/wired/resident（7×u64），
    // ri_phys_footprint 是第 9 个字段（偏移 72；v0..v6 同前缀）。内核
    // 按 flavor 的完整结构体写入（v6 = 16 + 31×u64）——缓冲必须给足，
    // 短了会被内核写穿（实测 SIGBUS）。
    const RI_PHYS_FOOTPRINT_OFF: usize = 16 + 7 * 8;
    // rusage_info_v6（RUSAGE_INFO_CURRENT）：16B uuid + 31×u64。
    let mut info = [0u8; 16 + 31 * 8];
    unsafe extern "C" {
        fn proc_pid_rusage(
            pid: i32,
            flavor: i32,
            buffer: *mut std::ffi::c_void,
        ) -> i32;
    }
    // RUSAGE_INFO_V4 = 4；只读前缀字段，偏移由 ABI 钉死。
    let r = unsafe {
        proc_pid_rusage(pid as i32, 4, info.as_mut_ptr() as *mut std::ffi::c_void)
    };
    (r == 0).then(|| {
        u64::from_le_bytes(
            info[RI_PHYS_FOOTPRINT_OFF..RI_PHYS_FOOTPRINT_OFF + 8]
                .try_into()
                .expect("常量切片恰 8 字节"),
        )
    })
}

impl PluginHost {
    /// 唯一入口：按配置决定绑不绑 socket。
    ///
    /// - `enabled` 为空 → `None`：**不建 socket、不碰文件系统、不拉
    ///   进程**（空载不变量；也不扫陈旧 socket——空载路径零改动）。
    /// - 非空 → 清扫 $TMPDIR 里死了进程的陈旧 socket（见
    ///   [`sweep_stale_sockets`]）→ 绑定 + listen（非阻塞）；绑定失败
    ///   不炸终端：stderr 警告 + `None`（同配置模块的降级哲学）。
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

    /// 配置快照（同会话再启用换新 host 用，见 [`host_set_enabled`]）。
    pub fn cfg(&self) -> &PluginsConfig {
        &self.cfg
    }

    /// 监听器引用（监督器接管 accept 用；p5 分发/泵路径直接经本结构）。
    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }

    // ------------------------------------------------------------------
    // 监督器（单一策略：启用即拉起；面板 v2 2026-08-29）
    // ------------------------------------------------------------------

    /// 拉起单个插件（一切拉起都从这里走：解析二进制 → spawn → 登记
    /// 子进程/错误）。幂等性由调用方（spawned 集）保证。
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
                self.children.push((name.to_string(), child));
            }
            Err(e) => {
                eprintln!("ninja: 插件 {name:?}（{}）拉起失败：{e}", bin.display());
                self.spawn_errors
                    .insert(name.to_string(), format!("拉起失败：{e}"));
            }
        }
    }

    /// **启用即拉起**（单一策略，2026-08-29 决策）：拉起全部 enabled
    /// 且尚未尝试过的插件。宿主启动（runloop 就绪后，app 的
    /// applicationDidFinishLaunching 调 [`spawn_startup_plugins`]）、p6
    /// 钩子再启用（[`host_set_enabled`]）、面板开（[
    /// PluginHost::session_enable`] 走单插件变体）都汇聚到这里。拉起后
    /// 开一个「等首个连接」窗口（[`SPAWN_CONNECT_WINDOW`]）钉住泵
    /// timer：插件 connect + 连接即推的 theme.set 靠泵消化（无层无
    /// 覆盖时泵本会自停）。
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

    /// 面板开关「开」的宿主侧半边：名字进会话 enabled 名单 + 立即拉起
    ///（同一套 spawn 路径；显式清除「别再试」标记 → 之前拉不起/被杀的
    /// 插件可以重试）。名字卫生同 [`resolve_plugin_binary`]（只收裸名）。
    /// 返回 false = 已禁用/名字非法（面板回弹开关）。
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

    /// 面板开关「关」的宿主侧半边：名字出会话 enabled 名单 + 立即杀它
    /// 名下的子进程 + 排干 EOF（收层/回退色板与 p6 插件死亡同一条通路：
    /// pump 摄连接 EOF → [`PluginHost::drop_conn`]）。名单清空 = 整个
    /// 插件面关掉（[`PluginHost::shutdown`]：删 socket，回到空载形态）。
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

    /// 状态快照（面板/测试）：enabled 名单 ∪ 有子进程 ∪ 有错误记录的
    /// 名字，逐名报告 启用/在跑/pid/内存/最后错误。顺带收割已退出的
    /// 子进程（try_wait）并把异常退出记进 last_error。
    pub fn snapshot(&mut self) -> Vec<PluginStatus> {
        // 收割退出者：正常退出（EOF 自退/被禁用杀）不记错，异常退出
        //（非零码/信号）记「已退出」，面板可见。
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
    // p4 命中分发（p5 扩展：冷启动等待 + claim 后层握手）
    // ------------------------------------------------------------------

    /// 发下一个 hit id（回执配对用）。点击路径主线程调用。
    pub fn next_hit_id(&mut self) -> u64 {
        self.next_hit_id = self.next_hit_id.saturating_add(1);
        self.next_hit_id
    }

    /// 把 hit 广播给所有已连插件，收集 claim/ignore，仲裁出结果；
    /// 有人认领且给了 `geom` 时继续层握手（open→ready→present）。
    /// 超时用 [`HIT_REPLY_TIMEOUT`]（生产入口；单测用带超时参数的
    /// [`PluginHost::dispatch_hit_with_timeout`]）。
    pub fn dispatch_hit(&mut self, hit: &Hit, geom: Option<&LayerGeom>) -> DispatchOutcome {
        self.dispatch_hit_with_timeout(hit, HIT_REPLY_TIMEOUT, geom)
    }

    /// 按需非阻塞 accept：把内核 backlog 里排队的插件连接收进来。
    /// 不新增线程；没连接就是空操作。已禁用时不再收新连接（行为同
    /// 未启用）。收到任何连接即关掉「等首个连接」窗口（[
    /// spawn_pending_disarm]）——泵的存活交回常规规则（层/色板覆盖）。
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
                    self.conns.push(Conn {
                        id: self.next_conn_id,
                        stream,
                        decoder: FrameDecoder::new(),
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break, // 监听器异常：本轮不再收，下次分发再试
            }
        }
        if !self.conns.is_empty() {
            spawn_pending_disarm();
        }
    }

    /// dispatch_hit 的实现核心（超时可注入，单测用短预算）。
    ///
    /// 流程：accept 排队连接 →（无连接时）首次拉起插件 + 等冷启动
    /// connect → 广播 hit 帧 → 逐连接收回执（共享 deadline；静默/断连/
    /// 坏消息一律 ignore，坏协议断开连接）→ 仲裁（claim 的 priority
    /// 最大者胜，平局先连者胜）→ 认领方层握手。
    pub(crate) fn dispatch_hit_with_timeout(
        &mut self,
        hit: &Hit,
        timeout: Duration,
        geom: Option<&LayerGeom>,
    ) -> DispatchOutcome {
        if self.disabled {
            // 已禁用（同会话禁用钩子）：等同未启用 → 系统默认打开。
            return DispatchOutcome::NoPlugins;
        }
        self.pump_accept();
        if self.conns.is_empty() {
            // 兜底冷启动（常规路径已不依赖：宿主启动/面板开就拉过）；
            // 只有「启用后从未成功拉起、且未试过」的插件会走到这。等
            // connect 的预算独立于回执预算（拉起只付一次，回执仍钉
            // HIT_REPLY_TIMEOUT）；全部插件都试过（拉不起/已死）就即刻
            // 降级，不再等。
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
            Err(_) => return DispatchOutcome::AllIgnored, // 不可能：new() 钓 v
        };
        let mut broken = Vec::new();
        for (i, c) in self.conns.iter_mut().enumerate() {
            if c.stream.write_all(&frame).is_err() {
                broken.push(i);
            }
        }
        for i in broken.iter().rev() {
            self.drop_conn(*i); // p6：断连 = 无主层一并回收
        }
        if self.conns.is_empty() {
            return DispatchOutcome::AllIgnored; // 广播全失败 = 无认领
        }

        // 收阶段：共享 deadline，逐连接收；responded 后不再读它。
        // 认领者按**连接 id** 记（下方会摘除断连，数组下标不稳）。
        let mut best: Option<(u32, u64)> = None; // (priority, conn id)
        let mut responded = vec![false; self.conns.len()];
        let mut dead: Vec<usize> = Vec::new();
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
                        // 对端关连接：不认领。
                        dead.push(i);
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
                                    // 帧级违规（超限/空载荷）：断开，视为 ignore。
                                    dead.push(i);
                                    responded[i] = true;
                                }
                                Ok(p) => match Message::decode_host(&p) {
                                    Ok(Message::HitClaim(m)) if m.id == hit.id => {
                                        if best.is_none_or(|(pr, _)| m.priority > pr) {
                                            best = Some((m.priority, c.id));
                                        }
                                        responded[i] = true;
                                    }
                                    Ok(Message::HitIgnore(m)) if m.id == hit.id => {
                                        responded[i] = true;
                                    }
                                    Ok(Message::ThemeSet(m)) => {
                                        // T2：连接后插件随时可推 theme.set
                                        //（官方 ninja-theme 连上即推；冷
                                        // 启动窗口内常在 hit 回执前到）。
                                        Self::handle_theme_set(&m, c.id);
                                    }
                                    Ok(_) => {} // 其余消息/别的 id：先记下，握手阶段消化
                                    Err(_) => {
                                        // 坏协议（版本/JSON）：断开，视为 ignore
                                        //（p3 契约：宿主断连）。
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
                        // 没回执但有字节：继续读（回执可能分批到）。
                    }
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        // 静默：预算内没等到回执 → ignore（连接保留，下次再试）。
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
            self.drop_conn(*i); // p6：断连/坏协议 = 无主层一并回收
        }
        let Some((priority, claim_conn)) = best else {
            ade_debug("dispatch: 全 ignore/静默");
            return DispatchOutcome::AllIgnored;
        };
        ade_debug(&format!("dispatch: claim priority={priority} conn={claim_conn}"));
        // 层握手（p5）：认领方在同一连接上要层。geom 为 None（如取证钩子
        // 无渲染上下文）时跳过——认领仍然成立，只是宿主不处理层。
        if let Some(geom) = geom
            && let Some(idx) = self.conns.iter().position(|c| c.id == claim_conn) {
                self.layer_handshake(idx, geom, LAYER_HANDSHAKE_TIMEOUT);
            }
        DispatchOutcome::Claimed { priority }
    }

    /// claim 后的层握手：读认领方连接直到 present/close/断连/预算尽。
    /// `layer.open` → 建 IOSurface 回 `layer.ready`；`layer.present` →
    /// 注册表标记呈现 + 触发重画 + 起泵 timer；`layer.close` → 摘层。
    fn layer_handshake(&mut self, conn_idx: usize, geom: &LayerGeom, budget: Duration) {
        let deadline = Instant::now() + budget;
        let conn_id = self.conns[conn_idx].id;
        let mut buf = [0u8; 8192];
        loop {
            // 1) 先消化解码器里**已缓冲**的帧——claim 与 layer.open 常在同
            //    一个读块到达（分发阶段只弹到回执就停），不先弹会在等新
            //    字节上白耗整个预算（E2E 实测过的竞态）。
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
                break; // 预算尽：层可能仍开着（等 present），pump 兜底
            };
            if self.conns[conn_idx].stream.set_read_timeout(Some(rem)).is_err() {
                self.drop_conn(conn_idx);
                return;
            }
            let n = match self.conns[conn_idx].stream.read(&mut buf) {
                Ok(0) => {
                    self.drop_conn(conn_idx); // 插件退了：收它的层（p6）
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

    /// 握手期单帧处置。返回是否继续等下一帧。
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
        match Message::decode_host(&payload) {
            Ok(Message::LayerOpen(m)) => {
                let geom = LayerGeom { conn: conn_id, ..geom_clone(geom) };
                match layer::open(&geom, m.anchor_row, m.anchor_col) {
                    Some(mut ready) => {
                        ready.id = m.id; // 回执 = layer.open 的 id
                        let f =
                            encode_frame(&Message::LayerReady(ready)).expect("LayerReady 编码");
                        if self.conns[conn_idx].stream.write_all(&f).is_err() {
                            return HandshakeStep::Dead;
                        }
                    }
                    None => {
                        eprintln!("ninja: 层分配失败（IOSurface/Metal），拒层");
                        let f = encode_frame(&Message::LayerClose(LayerClose::new(0)))
                            .expect("LayerClose 编码");
                        let _ = self.conns[conn_idx].stream.write_all(&f);
                    }
                }
                HandshakeStep::Continue
            }
            Ok(Message::LayerPresent(m)) => {
                layer::present(m.layer);
                ensure_pump_timer();
                HandshakeStep::Presented
            }
            Ok(Message::LayerClose(m)) => {
                let _ = layer::close(m.layer);
                stop_pump_timer_if_idle();
                HandshakeStep::Continue
            }
            Ok(Message::ThemeSet(m)) => {
                // 插件在层握手期间也可推色板（认领型插件顺带换色）。
                Self::handle_theme_set(&m, conn_id);
                HandshakeStep::Continue
            }
            Ok(_) => HandshakeStep::Continue, // 别的 id / 别的消息：握手期忽略
            Err(_) => HandshakeStep::Dead, // 坏协议：断（p3 契约）
        }
    }

    /// 泵：层打开期间轮询所有连接，消化插件异步消息（present 重合成 /
    /// close 摘层）。主 runloop timer 调用（见 [`ensure_pump_timer`]）。
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
                    self.drop_conn(i); // p6：插件退了，收它的层
                    continue;
                }
                Ok(n) => {
                    if conn.decoder.extend(&buf[..n]).is_err() {
                        self.drop_conn(i);
                        continue;
                    }
                    let mut dead = false;
                    while let Some(payload) = conn.decoder.pop() {
                        match payload {
                            Err(_) => dead = true,
                            Ok(p) => match Message::decode_host(&p) {
                                Ok(Message::LayerPresent(m)) => {
                                    layer::present(m.layer);
                                }
                                Ok(Message::LayerClose(m)) => {
                                    for _ in layer::close(m.layer) {}
                                    stop_pump_timer_if_idle();
                                }
                                Ok(Message::ThemeSet(m)) => {
                                    // T2：覆盖生效期间插件可再推色板换色。
                                    Self::handle_theme_set(&m, conn_id);
                                }
                                Ok(_) => {}
                                Err(_) => dead = true,
                            },
                        }
                        if dead {
                            break;
                        }
                    }
                    if dead {
                        self.drop_conn(i); // p6：坏协议断连，收它的层
                        continue;
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
                    self.drop_conn(i); // p6：IO 错断连，收它的层
                    continue;
                }
            }
        }
        if !layer::any_layers() {
            stop_pump_timer_if_idle();
        }
    }

    /// 连接死亡收口（EOF / IO 错 / 坏协议，p6 监督器）：摘连接 +
    /// **收掉该连接拥有的全部层**（插件死了它的层就是无主陈旧 overlay：
    /// 不摘则层永久残留且 `any_layers()` 恒真 → 泉 timer 永不停转，
    /// 只能靠用户 Esc 兑底）+ 无层时停泵。`layer::close_by_conn`
    /// 内部对受影响 pane 重画（无主层不再有别的重画时机）。
    /// T2：该连接若拥有色板覆盖 → 回退内置 ODP 基线并重画（与 p6
    /// 收层同语义：插件死了它的视觉贡献不能残留）。
    fn drop_conn(&mut self, idx: usize) {
        let Some(c) = self.conns.get(idx) else {
            return;
        };
        let conn_id = c.id;
        self.conns.remove(idx);
        if !layer::close_by_conn(conn_id).is_empty() {
            ade_debug(&format!("conn {conn_id} 死亡：已回收其全部层"));
        }
        if crate::theme::revoke_owner(conn_id) {
            eprintln!("ninja: 主题插件连接 {conn_id} 死亡，色板回退内置 One Dark Pro 基线");
            crate::view::apply_theme_all();
        }
        stop_pump_timer_if_idle();
    }

    /// T2：theme.set 处置。色值语义坏（格式/alpha 越界）→ 警告 + 整条
    /// 忽略（不断连：坏的是值不是协议）；有效 → 全局覆盖点落地 +
    /// 全部终端面重钉色板重画（vt 侧强制 Full 脏，跳帧不吃全屏换色）
    /// + 起泵（覆盖生效期间必须盯该连接，死亡即回退基线）。
    fn handle_theme_set(m: &ThemeSet, conn_id: u64) {
        match crate::theme::palette_from_wire(m) {
            Some(p) => {
                if crate::theme::apply_plugin(p, conn_id) {
                    eprintln!("ninja: 主题插件已换色板 {:?}（conn {conn_id}）", m.name);
                    crate::view::apply_theme_all();
                    ensure_pump_timer();
                }
            }
            None => eprintln!(
                "ninja: theme.set 色板无效（conn {conn_id}，name={:?}），整条忽略",
                m.name
            ),
        }
    }

    /// 按连接 id 发消息（input.key / layer.close 回程）。找不到连接
    ///（已断）→ Err。
    fn send_to_conn(&mut self, conn_id: u64, msg: &Message) -> std::io::Result<()> {
        let frame =
            encode_frame(msg).map_err(|e| std::io::Error::other(format!("encode: {e}")))?;
        let c = self
            .conns
            .iter_mut()
            .find(|c| c.id == conn_id)
            .ok_or_else(|| std::io::Error::other("plugin conn gone"))?;
        c.stream.write_all(&frame)
    }
}

/// 握手循环的单步结果。
enum HandshakeStep {
    Continue,
    Presented,
    Dead,
}

/// LayerGeom 的浅拷贝（Retained 字段 clone；主线程）。
fn geom_clone(g: &LayerGeom) -> LayerGeom {
    LayerGeom {
        pane: g.pane,
        cell_px: g.cell_px,
        view_px: g.view_px,
        scale: g.scale,
        device: g.device.clone(),
        view: g.view,
        conn: g.conn,
    }
}

impl PluginHost {
    /// 幂等关闭（p6 同会话禁用；[`Drop`] 复用同一实现）。顺序敏感：
    /// 1. 摘全部层并尽力通知还连着的拥有者 `layer.close`（插件好清
    ///    状态；已死连接的层一并回收）；
    /// 2. 无层即停泵 timer；
    /// 3. 断全部连接（插件侧读到 EOF 自退——正常路径零强杀）；
    /// 4. kill + wait 子进程（EOF 没退的兑底 + 收尸防僵尸）；
    /// 5. 删 socket 文件（文件消失 = 禁用完成的可观测信号）。
    /// 之后 host 处于 disabled 态：分发/泵/accept 全部空转，行为等同
    /// 未启用（NoPlugins），直到被换上新 host 再启用（见
    /// [`host_set_enabled`]）。
    pub fn shutdown(&mut self) {
        if self.disabled {
            return; // 幂等
        }
        self.disabled = true;
        // T2：禁用 = 插件能力全回收，色板覆盖一并回退内置基线（与
        // p6 收层同一语义；再启用后由插件重新推 theme.set 才会再覆盖）。
        if crate::theme::revoke_all() {
            eprintln!("ninja: 插件禁用，色板回退内置 One Dark Pro 基线");
            crate::view::apply_theme_all();
        }
        for (handle, conn, _pane) in layer::close_all() {
            let _ = self.send_to_conn(conn, &Message::LayerClose(LayerClose::new(handle)));
        }
        stop_pump_timer_if_idle();
        self.conns.clear();
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

impl Drop for PluginHost {
    fn drop(&mut self) {
        self.shutdown(); // 同一实现；已禁用则幂等空转
    }
}

// ---------------------------------------------------------------------------
// 泵 timer（层打开期间存在；主 runloop）
// ---------------------------------------------------------------------------

/// CFRunLoopTimer 的存储（CF 类型不自动 Send；只在主线程碰，手工标注
/// 满足 static 要求——纪律同 layer::Registry）。
struct TimerSlot(Option<objc2_core_foundation::CFRetained<objc2_core_foundation::CFRunLoopTimer>>);
unsafe impl Send for TimerSlot {}

static PUMP_TIMER: Mutex<TimerSlot> = Mutex::new(TimerSlot(None));

/// 拉起后「等首个连接」的窗口（单一策略，2026-08-29 决策）：插件被
/// 拉起后，它的 connect + 连接即推的 theme.set 要靠泵消化；但此时
/// 可能既无层也无色板覆盖（泵的常规启停条件都不满足），泵会自停 →
/// 连接永远没人 accept。窗口内泵不自停；首个连接进来（或窗口过期，
/// 拉不起/挂死的插件不该拖住空转红线）即恢复常规规则。之后插件推
/// theme.set 生效 → `override_active()` 接手钉住泵（T2 机制不变）。
const SPAWN_CONNECT_WINDOW: Duration = Duration::from_secs(5);

static SPAWN_PENDING: Mutex<Option<Instant>> = Mutex::new(None);

/// 开窗（拉起后）。
fn spawn_pending_arm() {
    if let Ok(mut s) = SPAWN_PENDING.lock() {
        *s = Some(Instant::now() + SPAWN_CONNECT_WINDOW);
    }
}

/// 关窗（连接已到 / 宿主禁用）。
fn spawn_pending_disarm() {
    if let Ok(mut s) = SPAWN_PENDING.lock() {
        *s = None;
    }
}

/// 窗口是否在等（钉住泵不自停）。
fn spawn_pending_active() -> bool {
    SPAWN_PENDING
        .lock()
        .map(|s| s.map(|dl| Instant::now() < dl).unwrap_or(false))
        .unwrap_or(false)
}

/// 泵回调（CFRunLoopTimer callout，主线程）：无分发器 = 宿主在退出，
/// 停表。
unsafe extern "C-unwind" fn pump_tick(
    _timer: *mut objc2_core_foundation::CFRunLoopTimer,
    _info: *mut std::ffi::c_void,
) {
    pump_now();
}

/// 起泵（幂等）：首个层打开后由 layer_handshake 调用。
fn ensure_pump_timer() {
    let main = match objc2_core_foundation::CFRunLoop::main() {
        Some(rl) => rl,
        None => return,
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

/// 停泵（幂等）：最后一个层关闭后由 pump/close 路径调用。
fn stop_pump_timer_if_idle() {
    if layer::any_layers()
        || crate::theme::override_active()
        || spawn_pending_active()
    {
        return; // 还有层要合成 / 还有色板覆盖要盯 / 还在等拉起的插件连上
    }
    if let Ok(mut slot) = PUMP_TIMER.lock()
        && let Some(t) = slot.0.take()
            && let Some(main) = objc2_core_foundation::CFRunLoop::main() {
                // SAFETY: t 曾加入主 runloop。
                unsafe {
                    main.remove_timer(
                        Some(&t),
                        objc2_core_foundation::kCFRunLoopCommonModes,
                    )
                };
            }
}

/// 泵入口（timer 回调直调；测试可直调）。
pub fn pump_now() {
    if !layer::any_layers() && !crate::theme::override_active() && !spawn_pending_active() {
        stop_pump_timer_if_idle();
        return;
    }
    match take_dispatcher() {
        Some(host) => {
            if let Ok(mut h) = host.lock() {
                h.pump_plugins();
            }
        }
        None => stop_pump_timer_if_idle(),
    }
}

// ---------------------------------------------------------------------------
// 全局分发器：view（Cmd+点击）/ 面板 / 取证钩子 → PluginHost 的通路
// ---------------------------------------------------------------------------
//
// PluginHost 住在本静态槽的 Arc 里（生命周期 = 进程；面板 v2 起
// 运行中把插件从零拉起需要随时可造新 host，栈上 Option 不再够用）。
// 退出收口不靠静态槽的 Drop（静态不 drop）：`applicationWillTerminate`
// → [`host_shutdown`] 显式幂等关（与 [`Drop`] 同一实现）；崩溃/
// SIGKILL 的 socket 尸体由 [`sweep_stale_sockets`] 清扫（p6）。只在主
// 线程读写（点击/面板/钩子本就主线程），Mutex 只为满足 static 要求。

static DISPATCHER: Mutex<Option<Arc<Mutex<PluginHost>>>> = Mutex::new(None);

/// 启动配置快照（会话真值的回退源：host 还没进（空 enabled）时，
/// 面板开关用这里的 paths 解析插件）。app::run 装一次。
static SESSION_CFG: Mutex<Option<PluginsConfig>> = Mutex::new(None);

/// 登记全局分发器 + 启动配置快照（app::run 调；启用非空时）。
pub fn install_dispatcher(host: Arc<Mutex<PluginHost>>, startup_cfg: PluginsConfig) {
    if let Ok(mut slot) = DISPATCHER.lock() {
        *slot = Some(host);
    }
    if let Ok(mut slot) = SESSION_CFG.lock() {
        *slot = Some(startup_cfg);
    }
}

/// 只装启动配置快照（空载路径：enabled 空，无 host 可装；面板首次
/// 开时仍需 paths 来发现/拉起插件）。
pub fn install_session_cfg(cfg: PluginsConfig) {
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
        Some(host) => host
            .lock()
            .map(|h| h.cfg().clone())
            .unwrap_or_default(),
        None => SESSION_CFG
            .lock()
            .ok()
            .and_then(|s| s.clone())
            .unwrap_or_default(),
    }
}

/// 点击路径一站式入口：给 hit 发号（无分发器 → 0）。
pub fn next_hit_id() -> u64 {
    match take_dispatcher() {
        Some(host) => host.lock().map(|mut h| h.next_hit_id()).unwrap_or(0),
        None => 0,
    }
}

/// 点击路径一站式入口：广播 hit 并仲裁；认领且能开层时走层握手。
/// 无分发器/锁坏 → NoPlugins（即未启用插件 → 系统默认打开）。
pub fn dispatch_hit(hit: &Hit, geom: Option<&LayerGeom>) -> DispatchOutcome {
    match take_dispatcher() {
        Some(host) => host
            .lock()
            .map(|mut h| h.dispatch_hit(hit, geom))
            .unwrap_or(DispatchOutcome::NoPlugins),
        None => DispatchOutcome::NoPlugins,
    }
}

/// 层前台键盘：把按键转成 `input.key` 发给拥有该层的插件连接。
/// 返回 false = 无层/连接已断（调用方回落普通终端路径）。
pub fn forward_input_key(
    pane: u32,
    key: &str,
    text: &str,
    modifiers: Vec<Modifier>,
) -> bool {
    let Some((layer, conn)) = layer::foreground(pane) else {
        return false;
    };
    let msg = Message::InputKey(InputKey::new(layer, key, text, modifiers));
    match take_dispatcher() {
        Some(host) => host
            .lock()
            .map(|mut h| h.send_to_conn(conn, &msg).is_ok())
            .unwrap_or(false),
        None => false,
    }
}

/// 宿主关层（Esc 兜底 / resize / pane 关闭）：摘层 + 通知插件
/// `layer.close` + 重画。PRODUCT：「任何插件层都能立刻关掉」。
pub fn host_close_layers_of_pane(pane: u32) {
    for (handle, conn, _) in layer::close_pane(pane) {
        if let Some(host) = take_dispatcher()
            && let Ok(mut h) = host.lock() {
                let _ = h.send_to_conn(conn, &Message::LayerClose(LayerClose::new(handle)));
            }
    }
    stop_pump_timer_if_idle();
}

/// 宿主退出收口（p6）：`NSApplication terminate:` 直接 `exit(0)`，
/// `app.run()` 不返回、Rust 栈展开不发生——静态槽不 drop，
/// `PluginHost::Drop` 在⌘Q/关最后窗的正常退出路径上不会跑（E2E 实测：
/// socket 尸体不只是 SIGKILL 的产物）。`applicationWillTerminate` 里
/// 显式调本函数（幂等；与 Drop 同一实现）。
pub fn host_shutdown() {
    if let Some(host) = take_dispatcher()
        && let Ok(mut h) = host.lock() {
            h.shutdown();
        }
}

/// **启用即拉起**的宿主启动半边（app 的 applicationDidFinishLaunching
/// 调；runloop 就绪后）。空载（无分发器）= 无操作——门禁不变。
pub fn spawn_startup_plugins() {
    if let Some(host) = take_dispatcher()
        && let Ok(mut h) = host.lock() {
            h.spawn_enabled_now();
        }
}

/// 状态接线：全部插件的状态快照（面板与测试用；见
/// [`PluginHost::snapshot`]）。无分发器 → 空表。
pub fn status_snapshot() -> Vec<PluginStatus> {
    match take_dispatcher() {
        Some(host) => host.lock().map(|mut h| h.snapshot()).unwrap_or_default(),
        None => Vec::new(),
    }
}

/// **面板开关的宿主侧入口**（2026-08-29 决策：启用即拉起/禁用即回收；
/// 与 `NINJA_P6_PLUGIN_FILE` 同一条幂等生命周期路径）。面板 UI 与
/// E2E 钩子都调这里；写回 ninja.toml 由调用方（`panel::toggle`）做。
/// - on：名字进会话 enabled 名单 + 立即拉起；host 不在/已禁用时先
///   重绑（从零拉起用启动快照的 paths）。
/// - off：名字出名单 + 立即杀进程/收层/断连；名单空 → 整个关掉
///   （shutdown：删 socket，回空载）。
/// 返回 false = 开且拉不起 host（绑定失败）；关恒 true。
pub fn toggle_plugin(name: &str, on: bool) -> bool {
    if !on {
        if let Some(host) = take_dispatcher() {
            if let Ok(mut h) = host.lock() {
                h.session_disable(name);
            }
        } else {
            // host 不在（空载）：从启动快照名单里剔除（下次启动生效）。
            if let Ok(mut slot) = SESSION_CFG.lock()
                && let Some(cfg) = slot.as_mut() {
                    cfg.enabled.retain(|n| n != name);
                }
        }
        return true;
    }
    match take_dispatcher() {
        Some(host) => {
            let Ok(mut h) = host.lock() else {
                return false;
            };
            if h.disabled {
                // 整面被 p6 钩子关过：重绑（同 host_set_enabled(true) 的
                // 再启用路径）。名字先进名单，新 host 一次拉起全部
                // enabled（含本次要开的）——单一策略不并出「重绑后其它
                // enabled 插件没人拉」的缺口。
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
                Some(h) => {
                    let arc = Arc::new(Mutex::new(h));
                    if let Ok(mut slot) = DISPATCHER.lock() {
                        *slot = Some(arc.clone());
                    }
                    match arc.lock() {
                        Ok(mut h) => {
                            h.spawn_enabled_now();
                            true
                        }
                        Err(_) => false,
                    }
                }
                None => false,
            }
        }
    }
}

/// p6 同会话禁用 / 再启用（取证钩子 `NINJA_P6_PLUGIN_FILE` 驱动；
/// 产品面 = 面板逐插件开关，见 [`toggle_plugin`]）。
/// - 禁用 = 现任 host [`PluginHost::shutdown`]（幂等：收层/断连接/
///   收割子进程/删 socket）；
/// - 再启用 = 新绑一个 host 换进分发器同一槽位——`spawned` 集随新
///   对象重置，并**立即拉起**全部 enabled（启用即拉起）；旧 host 的
///   [`Drop`] 是幂等空转。
/// 返回 false = 无分发器（未启用插件/宿主在退出）/ 再启用绑定失败。
pub fn host_set_enabled(on: bool) -> bool {
    let Some(host) = take_dispatcher() else {
        return false;
    };
    let Ok(mut h) = host.lock() else {
        return false;
    };
    if !on {
        h.shutdown();
        return true;
    }
    if h.cfg().enabled.is_empty() {
        return false; // 配置本就未启用：没有可再启用的东西
    }
    // 重绑在原任 host 自己的路径上（生产 = effective_socket_path；
    // 显式绑定的测试路径也随之保留）。
    let path = h.path().to_path_buf();
    match PluginHost::bind(path, h.cfg().clone()) {
        Some(nh) => {
            let bound = nh.path().to_path_buf();
            *h = nh;
            eprintln!(
                "ninja: 插件已再启用（socket {bound:?} 已重绑，enabled 已拉起）"
            );
            h.spawn_enabled_now();
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    /// 本测试进程独有的临时目录（不碰全局 TMPDIR 约定路径，避免并行
    /// 测试互踩）。
    fn sandbox(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ninja_plugins_test_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 协议样例里的 theme.set（solarized-dark，与 golden 同值）。
    fn sample_theme_set() -> ninja_protocol::ThemeSet {
        Message::sample_messages()
            .into_iter()
            .find_map(|m| match m {
                Message::ThemeSet(t) => Some(t),
                _ => None,
            })
            .expect("sample 集含 theme.set")
    }

    #[test]
    fn default_config_starts_nothing() {
        // 空载门禁的核心：默认（空）配置 → None。bind 永不发生，
        // 因此任何路径上都不会出现 socket 文件/监听/子进程。
        let cfg = PluginsConfig::default();
        assert!(cfg.enabled.is_empty());
        assert!(
            PluginHost::start(&cfg).is_none(),
            "空载配置绝不起 PluginHost"
        );
    }

    #[test]
    fn bind_listens_and_drop_cleans() {
        let dir = sandbox("bind");
        let sock = dir.join("ade.sock");
        {
            let host = PluginHost::bind(sock.clone(), PluginsConfig::default())
                .expect("显式绑定应成功");
            assert_eq!(host.path(), sock.as_path());
            assert!(sock.exists(), "绑定后 socket 文件应在");
            // listen 已生效：客户端能连上（内核排队）。
            UnixStream::connect(&sock).expect("启用后可连接（排队，不 accept）");
        } // host drop → 文件清除
        assert!(!sock.exists(), "drop 后 socket 文件应删除");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enabled_via_start_uses_convention_path() {
        // 走真实 start()（含 env 覆盖逻辑）：启用非空 → 绑生效路径
        //（NINJA_ADE_SOCK 设置时用它，否则约定路径）。start 只绑 socket
        //不拉进程——拉起由 spawn_enabled_now 显式触发（宿主启动/面板开）。
        // 与会改 NINJA_ADE_SOCK 的 toggle 测试串行（env 是进程级的）。
        let _g = DISPATCHER_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let cfg = PluginsConfig {
            enabled: vec!["preview".into()],
            ..PluginsConfig::default()
        };
        let expected = match std::env::var_os("NINJA_ADE_SOCK") {
            Some(p) => PathBuf::from(p),
            None => socket_path(),
        };
        {
            let host = PluginHost::start(&cfg).expect("启用即绑");
            assert_eq!(host.path(), expected.as_path());
            assert!(expected.exists());
            assert!(
                host.children.is_empty(),
                "start 只绑 socket；拉起在 spawn_enabled_now（本测试不调）"
            );
        }
        if std::env::var_os("NINJA_ADE_SOCK").is_none() {
            assert!(!expected.exists(), "drop 后约定路径应删除");
        }
    }

    #[test]
    fn socket_path_convention_contains_pid() {
        // 约定钉死：${TMPDIR}/ninja-ade-{pid}.sock。
        let p = socket_path();
        assert_eq!(
            p.file_name().unwrap().to_string_lossy(),
            format!("ninja-ade-{}.sock", std::process::id())
        );
        assert_eq!(p.parent(), Some(std::env::temp_dir().as_path()));
    }

    #[test]
    fn bind_failure_degrades_to_none() {
        // 路径不可达（父目录不存在）→ None，不 panic。
        let dir = sandbox("nope");
        let bad = dir.join("missing-dir/ade.sock");
        assert!(PluginHost::bind(bad, PluginsConfig::default()).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn binary_resolution_order() {
        // 名字卫生：路径注入拒绝。
        assert!(resolve_plugin_binary("../evil", &PluginsConfig::default()).is_none());
        assert!(resolve_plugin_binary("", &PluginsConfig::default()).is_none());

        // paths 表优先于 env/同目录。
        let dir = sandbox("resolve");
        let explicit = dir.join("plug-explicit");
        std::fs::write(&explicit, b"#!/bin/sh\n").unwrap();
        let cfg = PluginsConfig {
            enabled: vec!["preview".into()],
            paths: std::collections::HashMap::from([(
                "preview".to_string(),
                explicit.to_string_lossy().into_owned(),
            )]),
        };
        assert_eq!(resolve_plugin_binary("preview", &cfg), Some(explicit.clone()));

        // 名字不在任何来源 → None。
        assert!(resolve_plugin_binary("ghost", &cfg).is_none());

        // paths 指向不存在的文件 → 落到 env/同目录（都没有 → None）。
        let cfg2 = PluginsConfig {
            enabled: vec!["preview".into()],
            paths: std::collections::HashMap::from([(
                "preview".to_string(),
                dir.join("nope").to_string_lossy().into_owned(),
            )]),
        };
        assert!(resolve_plugin_binary("preview", &cfg2).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn binary_resolution_user_plugin_dir() {
        // p7 用户级缺省段：~/.config/ninja/plugins/<name>（注入隔离 HOME，
        // 不碰真实 ~/.config）。
        let dir = sandbox("resolve_user");
        let plugdir = dir.join("home/.config/ninja/plugins");
        std::fs::create_dir_all(&plugdir).unwrap();
        let bin = plugdir.join("preview");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();

        let cfg = PluginsConfig {
            enabled: vec!["preview".into()],
            ..PluginsConfig::default()
        };
        // 命中：paths/env 都没给时用户目录生效。
        assert_eq!(
            resolve_plugin_binary_in("preview", &cfg, Some(plugdir.as_path())),
            Some(bin.clone())
        );

        // 显式 paths 优先于用户目录。
        let explicit = dir.join("plug-explicit");
        std::fs::write(&explicit, b"#!/bin/sh\n").unwrap();
        let cfg2 = PluginsConfig {
            enabled: vec!["preview".into()],
            paths: std::collections::HashMap::from([(
                "preview".to_string(),
                explicit.to_string_lossy().into_owned(),
            )]),
        };
        assert_eq!(
            resolve_plugin_binary_in("preview", &cfg2, Some(plugdir.as_path())),
            Some(explicit)
        );

        // 未命中：目录在但名字不在 → None；用户目录段缺席（HOME 异常）
        //  → 整段跳过不报错。
        assert!(resolve_plugin_binary_in("ghost", &cfg, Some(plugdir.as_path())).is_none());
        assert!(resolve_plugin_binary_in("preview", &cfg, None).is_none());

        // 目录还没创建（装了但没拷文件）→ 同样安静跳过。
        let unmade = dir.join("home2/.config/ninja/plugins");
        assert!(resolve_plugin_binary_in("preview", &cfg, Some(unmade.as_path())).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------------
    // p4 命中分发（进程内 UnixStream 对端；不拉任何真实进程）
    // ------------------------------------------------------------------

    use ninja_protocol::{HitClaim, HitIgnore, HitKind};
    use std::io::{Read, Write};
    use std::thread::{self, JoinHandle};

    fn sample_hit(id: u64) -> Hit {
        Hit::new(id, HitKind::Path, "/tmp/x.rs:1:2", "/tmp", 3, 5, 9, vec![Modifier::Cmd])
    }

    /// 对端脚本：读完 hit 帧后怎么回。
    enum PeerReply {
        Claim(u32),
        Ignore,
        /// 收到但不回（静默，吃掉超时预算）。
        Silent,
        /// 收到后立即断开。
        Disconnect,
    }

    /// 起一个进程内对端：连接 → 读一帧 hit → 按脚本回执。
    /// 返回收到的 hit 帧载荷（供断言字段完整性）。
    fn spawn_peer(sock: PathBuf, reply: PeerReply) -> JoinHandle<Vec<u8>> {
        thread::spawn(move || {
            let mut s = UnixStream::connect(&sock).expect("peer connect");
            let mut len_buf = [0u8; 4];
            s.read_exact(&mut len_buf).expect("peer read len");
            let len = u32::from_le_bytes(len_buf) as usize;
            let mut payload = vec![0u8; len];
            s.read_exact(&mut payload).expect("peer read payload");
            let id = match Message::decode_host(&payload) {
                Ok(Message::Hit(h)) => h.id,
                _ => panic!("peer 应收到 hit 帧"),
            };
            match reply {
                PeerReply::Claim(p) => {
                    let f = encode_frame(&Message::HitClaim(HitClaim::new(id, p))).unwrap();
                    s.write_all(&f).unwrap();
                    // 保连接开着，等主线程读完。
                    thread::sleep(Duration::from_millis(300));
                }
                PeerReply::Ignore => {
                    let f = encode_frame(&Message::HitIgnore(HitIgnore::new(id))).unwrap();
                    s.write_all(&f).unwrap();
                    thread::sleep(Duration::from_millis(300));
                }
                PeerReply::Silent => thread::sleep(Duration::from_millis(400)),
                PeerReply::Disconnect => drop(s),
            }
            payload
        })
    }

    /// 绑一个独立 host，等对端连进 backlog。
    fn host_with_peers(tag: &str, peers: Vec<PeerReply>) -> (PluginHost, Vec<JoinHandle<Vec<u8>>>) {
        let dir = sandbox(tag);
        let sock = dir.join("ade.sock");
        let host =
            PluginHost::bind(sock.clone(), PluginsConfig::default()).expect("bind");
        let handles = peers
            .into_iter()
            .map(|r| spawn_peer(sock.clone(), r))
            .collect();
        // 等对端 connect 完成（进内核 backlog；宿主此刻还没 accept）。
        thread::sleep(Duration::from_millis(150));
        (host, handles)
    }

    #[test]
    fn dispatch_without_peers_is_no_plugins() {
        let dir = sandbox("disp0");
        let mut host = PluginHost::bind(dir.join("a.sock"), PluginsConfig::default()).unwrap();
        let hit = sample_hit(host.next_hit_id());
        // 短预算：无对端且无可拉插件（enabled 空 → 不 spawn）→ NoPlugins。
        assert_eq!(
            host.dispatch_hit_with_timeout(&hit, Duration::from_millis(10), None),
            DispatchOutcome::NoPlugins
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dispatch_peer_ignore_falls_back_and_hit_fields_complete() {
        let (mut host, handles) = host_with_peers("dispign", vec![PeerReply::Ignore]);
        let hit = sample_hit(host.next_hit_id());
        assert_eq!(
            host.dispatch_hit_with_timeout(&hit, Duration::from_millis(300), None),
            DispatchOutcome::AllIgnored
        );
        let payload = handles.into_iter().next().unwrap().join().unwrap();
        // 插件收到的必须是完整 hit 字段（够预览认领，对齐 golden 样例）。
        match Message::decode_host(&payload).unwrap() {
            Message::Hit(h) => {
                assert_eq!(h.kind, HitKind::Path);
                assert_eq!(h.text, "/tmp/x.rs:1:2");
                assert_eq!(h.cwd, "/tmp");
                assert_eq!(h.row, 3);
                assert_eq!(h.col, 5);
                assert_eq!(h.pane, 9);
                assert_eq!(h.modifiers, vec![Modifier::Cmd]);
                assert_eq!(h.id, hit.id);
            }
            other => panic!("应收到 hit，得到 {other:?}"),
        }
    }

    #[test]
    fn dispatch_claim_wins_by_priority() {
        // 单 claim（无 geom：不进层握手）。
        let (mut host, handles) = host_with_peers("dispc1", vec![PeerReply::Claim(7)]);
        let hit = sample_hit(host.next_hit_id());
        assert_eq!(
            host.dispatch_hit_with_timeout(&hit, Duration::from_millis(300), None),
            DispatchOutcome::Claimed { priority: 7 }
        );
        for h in handles {
            h.join().unwrap();
        }

        // 双 claim：priority 大者胜（与连接先后无关）。
        let (mut host, handles) =
            host_with_peers("dispc2", vec![PeerReply::Claim(3), PeerReply::Claim(9)]);
        let hit = sample_hit(host.next_hit_id());
        assert_eq!(
            host.dispatch_hit_with_timeout(&hit, Duration::from_millis(300), None),
            DispatchOutcome::Claimed { priority: 9 }
        );
        for h in handles {
            h.join().unwrap();
        }

        // ignore + claim 并存 → claim 胜。
        let (mut host, handles) =
            host_with_peers("dispc3", vec![PeerReply::Ignore, PeerReply::Claim(4)]);
        let hit = sample_hit(host.next_hit_id());
        assert_eq!(
            host.dispatch_hit_with_timeout(&hit, Duration::from_millis(300), None),
            DispatchOutcome::Claimed { priority: 4 }
        );
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn dispatch_silent_peer_times_out_to_ignore() {
        // 静默对端：预算耗尽 → ignore 降级（不卡死；短预算 80ms 控制测试时长）。
        let (mut host, handles) = host_with_peers("dispsil", vec![PeerReply::Silent]);
        let hit = sample_hit(host.next_hit_id());
        let t0 = Instant::now();
        assert_eq!(
            host.dispatch_hit_with_timeout(&hit, Duration::from_millis(80), None),
            DispatchOutcome::AllIgnored
        );
        assert!(t0.elapsed() >= Duration::from_millis(70), "应等满预算再降级");
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn dispatch_disconnected_peer_is_ignore() {
        let (mut host, handles) = host_with_peers("dispdead", vec![PeerReply::Disconnect]);
        let hit = sample_hit(host.next_hit_id());
        assert_eq!(
            host.dispatch_hit_with_timeout(&hit, Duration::from_millis(300), None),
            DispatchOutcome::AllIgnored
        );
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn dispatcher_strong_slot_and_free_entry_works() {
        // 强槽通路（面板 v2 起静态槽持 Arc）：登记 → 可取 → hit 发号；
        // host_shutdown 后行为同未启用（NoPlugins）。槽全局：与其它
        // 装槽的测试串行（见 DISPATCHER_TEST_LOCK）。
        let _g = DISPATCHER_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = sandbox("dispwk");
        let sock = dir.join("a.sock");
        let arc = Arc::new(Mutex::new(
            PluginHost::bind(sock.clone(), PluginsConfig::default()).unwrap(),
        ));
        install_dispatcher(arc, PluginsConfig::default());
        assert!(take_dispatcher().is_some());
        assert!(sock.exists());
        assert!(next_hit_id() >= 1);
        // 退出收口：shutdown（幂等；与 applicationWillTerminate 同一
        // 通路）→ socket 消失，分发 NoPlugins，不 panic。
        host_shutdown();
        assert!(!sock.exists(), "shutdown 后 socket 应删除");
        assert_eq!(
            dispatch_hit(&sample_hit(1), None),
            DispatchOutcome::NoPlugins
        );
        // 清槽，不污染后续装槽的测试（生产永不卸槽）。
        if let Ok(mut slot) = DISPATCHER.lock() {
            *slot = None;
        }
        assert_eq!(
            dispatch_hit(&sample_hit(2), None),
            DispatchOutcome::NoPlugins
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forward_input_key_without_layer_is_false() {
        // 无层：键盘路由回落普通终端路径。
        assert!(!forward_input_key(4242, "a", "", vec![]));
    }

    // ------------------------------------------------------------------
    // p6 关掉即轻：插件死亡收层 / 同会话禁用 / 陈旧 socket 清扫
    // ------------------------------------------------------------------

    use ninja_protocol::{LayerOpen, LayerPresent, Placement};

    /// 层生命周期测试几何：真 Metal 设备 + 假 view（0 = 重画跳过，
    /// 见 layer::repaint_view）。headless 无设备 → None（跳过，同
    /// renderer 测试惯例）。
    fn test_geom() -> Option<LayerGeom> {
        let device = objc2_metal::MTLCreateSystemDefaultDevice()?;
        Some(LayerGeom {
            pane: 41061, // 独立 pane id：不与其它测试/全局状态互踩
            cell_px: (8.0, 16.0),
            view_px: (640.0, 480.0),
            scale: 2.0,
            device,
            view: 0,
            conn: 0,
        })
    }

    /// 对端脚本（层生命周期）：claim + layer.open → 等 layer.ready →
    /// （present=true 时发 present 并稍等宿主消化）→ 断开连接（=插件死亡）。
    fn spawn_layer_peer(sock: PathBuf, present: bool) -> JoinHandle<()> {
        thread::spawn(move || {
            let mut s = UnixStream::connect(&sock).expect("peer connect");
            let mut len_buf = [0u8; 4];
            s.read_exact(&mut len_buf).unwrap();
            let len = u32::from_le_bytes(len_buf) as usize;
            let mut payload = vec![0u8; len];
            s.read_exact(&mut payload).unwrap();
            let id = match Message::decode_host(&payload) {
                Ok(Message::Hit(h)) => h.id,
                other => panic!("peer 应收到 hit，得到 {other:?}"),
            };
            s.write_all(&encode_frame(&Message::HitClaim(HitClaim::new(id, 100))).unwrap())
                .unwrap();
            s.write_all(
                &encode_frame(&Message::LayerOpen(LayerOpen::new(
                    id,
                    Placement::Overlay,
                    2,
                    0,
                )))
                .unwrap(),
            )
            .unwrap();
            // 等 layer.ready（拿到层句柄）。
            s.read_exact(&mut len_buf).unwrap();
            let len = u32::from_le_bytes(len_buf) as usize;
            let mut p = vec![0u8; len];
            s.read_exact(&mut p).unwrap();
            let handle = match Message::decode_plugin(&p) {
                Ok(Message::LayerReady(r)) => r.layer,
                other => panic!("peer 应收到 layer.ready，得到 {other:?}"),
            };
            if present {
                s.write_all(&encode_frame(&Message::LayerPresent(LayerPresent::new(handle))).unwrap())
                    .unwrap();
                thread::sleep(Duration::from_millis(150));
            }
            drop(s); // 插件死亡（EOF）
        })
    }

    #[test]
    fn conn_death_reclaims_layers_and_stops_pump() {
        // p6 监督器缺口：插件死亡（断连）必须收层——不摘则陈旧 overlay
        // 永久残留 + any_layers 恒真 + 泵 timer 永不停转。
        let Some(geom) = test_geom() else {
            eprintln!("skip: 无 Metal 设备（headless），开层需真设备");
            return;
        };
        // REGISTRY 全局：与 layer::tests 的空表断言互斥。
        let _g = crate::layer::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // 阶段一：握手期对端死亡（layer_handshake 的 EOF 分支）。
        {
            let dir = sandbox("connd1");
            let sock = dir.join("a.sock");
            let mut host =
                PluginHost::bind(sock.clone(), PluginsConfig::default()).expect("bind");
            let peer = spawn_layer_peer(sock, false);
            // 等对端 connect 进 backlog（同 host_with_peers 的节奏）。
            thread::sleep(Duration::from_millis(150));
            let hit = sample_hit(host.next_hit_id());
            let out = host.dispatch_hit_with_timeout(&hit, Duration::from_secs(2), Some(&geom));
            assert_eq!(out, DispatchOutcome::Claimed { priority: 100 });
            peer.join().unwrap();
            assert!(
                !layer::any_layers(),
                "握手期对端死亡：它开的层应被回收（无主陈旧 overlay）"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        // 阶段二：present 后在泵路径死亡（pump_plugins 的 EOF 分支）。
        {
            let dir = sandbox("connd2");
            let sock = dir.join("a.sock");
            let mut host =
                PluginHost::bind(sock.clone(), PluginsConfig::default()).expect("bind");
            let peer = spawn_layer_peer(sock, true);
            // 等对端 connect 进 backlog（同 host_with_peers 的节奏）。
            thread::sleep(Duration::from_millis(150));
            let hit = sample_hit(host.next_hit_id());
            let out = host.dispatch_hit_with_timeout(&hit, Duration::from_secs(2), Some(&geom));
            assert_eq!(out, DispatchOutcome::Claimed { priority: 100 });
            assert!(layer::any_layers(), "present 后层应在");
            peer.join().unwrap();
            // 泵消化对端 EOF：层被回收（多泵几拍兑调度抖动）。
            let mut reclaimed = false;
            for _ in 0..40 {
                host.pump_plugins();
                if !layer::any_layers() {
                    reclaimed = true;
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
            assert!(reclaimed, "泵期对端死亡：它开的层应被回收");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn shutdown_is_idempotent_cleans_and_rebind_works() {
        // 同会话禁用通路（Drop 复用同一实现）：socket 消失、对端读到
        // EOF、幂等、禁用后行为同未启用（NoPlugins）、同路径可重绑
        // （再启用语义）。
        let dir = sandbox("shut");
        let sock = dir.join("a.sock");
        let cfg = PluginsConfig {
            enabled: vec!["ghost".into()], // 无对应二进制：不会真的拉起
            ..PluginsConfig::default()
        };
        let mut host = PluginHost::bind(sock.clone(), cfg).expect("bind");
        assert!(sock.exists());

        // 对端连上（pump_plugins 顺带 accept），读到 EOF（=shutdown 断连）。
        let peer_sock = sock.clone();
        let peer = thread::spawn(move || {
            let mut s = UnixStream::connect(&peer_sock).expect("peer");
            let mut b = [0u8; 16];
            let _ = s.read(&mut b); // 阻塞到 EOF
        });
        thread::sleep(Duration::from_millis(100));
        host.pump_plugins(); // 收进对端（无层：空转）

        host.shutdown();
        assert!(!sock.exists(), "shutdown 后 socket 文件应删除");
        peer.join().unwrap();

        // 幂等：再关一次不 panic、不重复动作。
        host.shutdown();
        // 禁用后行为同未启用：分发直接 NoPlugins（不重试拉插件）。
        let hit = sample_hit(host.next_hit_id());
        assert_eq!(
            host.dispatch_hit_with_timeout(&hit, Duration::from_millis(10), None),
            DispatchOutcome::NoPlugins
        );

        // 再启用语义：同一路径重绑新 host（spawned 集随新对象重置）。
        let cfg = host.cfg().clone();
        let mut again = PluginHost::bind(sock.clone(), cfg).expect("rebind");
        assert!(sock.exists(), "再启用应重绑同一路径");
        again.shutdown();
        assert!(!sock.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// T2：theme.set 的宿主处置链（进程内对端，不拉真实插件进程）：
    /// 1) 插件连接推 theme.set → 覆盖生效（current() = 插件色板）；
    /// 2) 坏色板（格式错）→ 整条忽略且**不断连**；
    /// 3) 对端死亡（EOF）→ 泵摘连接 + 色板回退 ODP 基线（p6 同语义）。
    /// （视图层重钉/重画由 view::apply_theme_all 完成，本测试进程无
    /// 窗口 view = 空转；vt 链路已由 theme.rs 单测覆盖。）
    #[test]
    fn theme_set_applies_and_reverts_on_conn_death() {
        let _g = crate::theme::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = sandbox("themeset");
        let sock = dir.join("a.sock");
        let mut host = PluginHost::bind(sock.clone(), PluginsConfig::default()).expect("bind");

        // 对端：连上 → 先推坏色板（应被忽略）→ 再推好色板 → 等宿主消化
        //（重试对齐宿主泵拍）→ 断开（= 插件死亡）。
        let peer = thread::spawn(move || {
            let mut s = UnixStream::connect(&sock).expect("peer connect");
            let mut bad = sample_theme_set();
            bad.bg = "#nope!".into();
            s.write_all(&encode_frame(&Message::ThemeSet(bad)).unwrap())
                .unwrap();
            let good = sample_theme_set();
            s.write_all(&encode_frame(&Message::ThemeSet(good)).unwrap())
                .unwrap();
            // 给宿主时间消化（宿主泵由测试直调，这里只保连接活着）。
            thread::sleep(Duration::from_millis(400));
            drop(s); // EOF = 插件死亡
        });
        thread::sleep(Duration::from_millis(150));
        host.pump_plugins(); // accept + 消化两帧

        // 好色板生效（solarized-dark bg #002b36）；坏的那条没生效也没断连。
        let cur = crate::theme::current();
        assert_eq!(cur.bg, crate::term::Rgb(0x00, 0x2B, 0x36), "theme.set 应生效");
        assert_eq!(cur.name, "solarized-dark");
        assert!(crate::theme::override_active());
        assert_eq!(host.conns.len(), 1, "坏色板不得断连（忽略的是值不是协议）");

        // 对端死亡 → 泵摘连接 → 回退 ODP。
        peer.join().unwrap();
        let mut reverted = false;
        for _ in 0..40 {
            host.pump_plugins();
            if host.conns.is_empty() {
                reverted = true;
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        assert!(reverted, "泵应摘掉死亡对端连接");
        assert!(!crate::theme::override_active(), "连接死亡必须回退基线");
        assert_eq!(crate::theme::current(), crate::theme::one_dark_pro());

        // shutdown 路径兜底：覆盖存在时禁用 → 回退（幂等）。
        let pal = crate::theme::palette_from_wire(&sample_theme_set()).unwrap();
        assert!(crate::theme::apply_plugin(pal, 42));
        host.shutdown();
        assert!(!crate::theme::override_active(), "禁用必须回收色板覆盖");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_socket_sweep_dead_pid_only() {
        // p6：陈旧 socket 清扫——只删死了进程的；活 pid / 解析不出 pid /
        // 非约定文件一律不动。
        let dir = sandbox("sweep");
        let live = dir.join(format!("ninja-ade-{}.sock", std::process::id()));
        // 死 pid：真拉一个短命进程收尸拿 pid（kill(pid,0)=ESRCH）。
        let dead_pid = {
            let mut c = std::process::Command::new("/usr/bin/true").spawn().unwrap();
            let pid = c.id();
            c.wait().unwrap();
            pid
        };
        let dead = dir.join(format!("ninja-ade-{dead_pid}.sock"));
        let garbage = dir.join("ninja-ade-notapid.sock");
        let unrelated = dir.join("other-thing.sock");
        for p in [&live, &dead, &garbage, &unrelated] {
            std::fs::write(p, b"x").unwrap();
        }
        sweep_stale_sockets_in(&dir);
        assert!(live.exists(), "活 pid 的 socket（并行实例）不能动");
        assert!(!dead.exists(), "死 pid 的陈旧 socket 应被清扫");
        assert!(garbage.exists(), "解析不出 pid 的文件不碰");
        assert!(unrelated.exists(), "非本约定的文件不碰");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 分发器槽是全局的：装它的测试之间串行（含既有
    /// dispatcher_strong_slot… 测试）。收尾卸槽，不污染后续。
    static DISPATCHER_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn host_set_enabled_disable_reenable_cycle() {
        // p6 钩子通路（NINJA_P6_PLUGIN_FILE → host_set_enabled）：
        // 禁用 → socket 消失；再启用 → 同路径重绑（换新 host，启用即
        // 拉起——ghost 无二进制，拉起尝试降级但不影响重绑语义）。
        let _g = DISPATCHER_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = sandbox("p6hook");
        let sock = dir.join("a.sock");
        let cfg = PluginsConfig {
            enabled: vec!["ghost".into()],
            ..PluginsConfig::default()
        };
        let arc = Arc::new(Mutex::new(
            PluginHost::bind(sock.clone(), cfg).expect("bind"),
        ));
        install_dispatcher(arc, PluginsConfig::default());
        assert!(sock.exists());

        assert!(host_set_enabled(false));
        assert!(!sock.exists(), "禁用后 socket 文件应消失");

        assert!(host_set_enabled(true));
        assert!(sock.exists(), "再启用应重绑同一路径");

        // 再关一次 + 收尾（生产槽不卸；这里清槽防污染后续测试）。
        assert!(host_set_enabled(false));
        if let Ok(mut slot) = DISPATCHER.lock() {
            *slot = None;
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------------
    // 面板 v2（单一策略：启用即拉起）
    // ------------------------------------------------------------------

    /// 伪造插件脚本：挂住等宿主杀（真实插件的常驻形态）；x 位可执行。
    fn fake_plugin(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(
            &p,
            "#!/bin/sh\nwhile :; do sleep 0.2; done\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    #[test]
    fn toggle_plugin_single_strategy_lifecycle() {
        // 面板开关全链（与 NINJA_P6_PLUGIN_FILE 同一条幂等生命周期）：
        // 空 enabled（无 host）→ toggle on = 从零拉起（socket 出现、
        // 子进程在跑、快照报告内存）→ toggle off = 杀进程 + 名单空 →
        // shutdown（socket 消失，回空载形态）。
        let _g = DISPATCHER_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = sandbox("toggle");
        let bin = fake_plugin(&dir, "fakeplug");
        let sock = dir.join("ade.sock");
        // SAFETY: 单线程测试内的 env 覆盖（Rust 2024 起 unsafe）。
        unsafe { std::env::set_var("NINJA_ADE_SOCK", &sock) };
        // 起点：空载（enabled 空，无 host）——面板仍可用（靠启动快照）。
        let cfg = PluginsConfig {
            paths: std::collections::HashMap::from([(
                "fakeplug".to_string(),
                bin.to_string_lossy().into_owned(),
            )]),
            ..PluginsConfig::default()
        };
        install_session_cfg(cfg.clone());
        assert!(take_dispatcher().is_none(), "空载：无 host");

        // —— 开：从零拉起。
        assert!(toggle_plugin("fakeplug", true));
        assert!(sock.exists(), "面板开后 socket 应出现");
        let snap = status_snapshot();
        let st = snap.iter().find(|s| s.name == "fakeplug").unwrap();
        assert!(st.enabled && st.running, "启用即拉起：进程应在跑（{st:?}）");
        let pid = st.pid.unwrap();
        assert!(st.memory_bytes.unwrap_or(0) > 0, "真实子进程足迹应 > 0");

        // —— 关：杀进程 + 名单空 → 整面关。
        assert!(toggle_plugin("fakeplug", false));
        assert!(!sock.exists(), "最后一个插件关掉 → socket 删除（空载）");
        let snap = status_snapshot();
        let st = snap.iter().find(|s| s.name == "fakeplug");
        assert!(
            st.is_none() || !st.unwrap().running,
            "关后不应在跑（{st:?}）"
        );
        // 进程真的死了（kill+wait 已收尸：kill(pid,0) 应 ESRCH）。
        let alive = unsafe { libc::kill(pid as i32, 0) == 0 }
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        assert!(!alive, "子进程应被收割（pid {pid} 仍在）");

        // —— 再开再关（幂等往返）。
        assert!(toggle_plugin("fakeplug", true));
        assert!(sock.exists());
        assert!(toggle_plugin("fakeplug", false));
        assert!(!sock.exists());

        // SAFETY: 同上。
        unsafe { std::env::remove_var("NINJA_ADE_SOCK") };
        if let Ok(mut slot) = DISPATCHER.lock() {
            *slot = None;
        }
        if let Ok(mut slot) = SESSION_CFG.lock() {
            *slot = None;
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_enable_off_missing_binary_reports_error() {
        // 拉不起的二进制：开 = 记入 last_error、不在跑；名单空不绑 socket
        // 之外的任何东西；开关往返不炸。
        let _g = DISPATCHER_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = sandbox("nobin");
        let sock = dir.join("ade.sock");
        // SAFETY: 单线程测试内的 env 覆盖。
        unsafe { std::env::set_var("NINJA_ADE_SOCK", &sock) };
        install_session_cfg(PluginsConfig::default());
        assert!(toggle_plugin("ghostplug", true));
        assert!(sock.exists(), "host 仍应绑定（名单非空）");
        let snap = status_snapshot();
        let st = snap.iter().find(|s| s.name == "ghostplug").unwrap();
        assert!(st.enabled && !st.running, "拉不起：不在跑但在名单");
        assert!(st.last_error.is_some(), "应有错误说明（{st:?}）");
        assert!(toggle_plugin("ghostplug", false));
        assert!(!sock.exists(), "关完回空载");
        // SAFETY: 同上。
        unsafe { std::env::remove_var("NINJA_ADE_SOCK") };
        if let Ok(mut slot) = DISPATCHER.lock() {
            *slot = None;
        }
        if let Ok(mut slot) = SESSION_CFG.lock() {
            *slot = None;
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn toggle_on_over_disabled_host_respawns_all_enabled() {
        // p6 整面关掉后（host disabled）再从面板开单个插件：重绑 + **全部
        // enabled 一起拉起**（不只本次开关的那个——「重绑后其它 enabled
        // 插件没人拉」的缺口回归钉）。
        let _g = DISPATCHER_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = sandbox("rebind");
        let b1 = fake_plugin(&dir, "plug_a");
        let b2 = fake_plugin(&dir, "plug_b");
        let sock = dir.join("ade.sock");
        // SAFETY: 单线程测试内的 env 覆盖。
        unsafe { std::env::set_var("NINJA_ADE_SOCK", &sock) };
        let cfg = PluginsConfig {
            enabled: vec!["plug_a".into()],
            paths: std::collections::HashMap::from([
                ("plug_a".to_string(), b1.to_string_lossy().into_owned()),
                ("plug_b".to_string(), b2.to_string_lossy().into_owned()),
            ]),
            ..PluginsConfig::default()
        };
        let arc = Arc::new(Mutex::new(
            PluginHost::bind(sock.clone(), cfg.clone()).expect("bind"),
        ));
        install_dispatcher(arc.clone(), cfg);
        arc.lock().unwrap().spawn_enabled_now();
        assert!(sock.exists());

        // p6 整面关（等同 NINJA_P6_PLUGIN_FILE=off）。
        assert!(host_set_enabled(false));
        assert!(!sock.exists());

        // 面板开 plug_b：重绑 → plug_a（还在 enabled 名单里）与 plug_b
        // 一起拉起。
        assert!(toggle_plugin("plug_b", true));
        assert!(sock.exists(), "重绑后 socket 应回来");
        let snap = status_snapshot();
        let a = snap.iter().find(|s| s.name == "plug_a").unwrap();
        let b = snap.iter().find(|s| s.name == "plug_b").unwrap();
        assert!(a.enabled && a.running, "重绑后既有 enabled 也要拉起：{a:?}");
        assert!(b.enabled && b.running, "本次开关的插件在跑：{b:?}");

        // 收尾。
        assert!(toggle_plugin("plug_a", false));
        assert!(toggle_plugin("plug_b", false));
        assert!(!sock.exists(), "名单空 → socket 删");
        // SAFETY: 同上。
        unsafe { std::env::remove_var("NINJA_ADE_SOCK") };
        if let Ok(mut slot) = DISPATCHER.lock() {
            *slot = None;
        }
        if let Ok(mut slot) = SESSION_CFG.lock() {
            *slot = None;
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spawn_pending_window_pins_pump() {
        // 拉起后的「等首个连接」窗口：开窗 → 泵不许自停（无层无覆盖也
        // 不停）；关窗/过期 → 恢复常规。纯静态槽直测（先起泵再验停不停）。
        assert!(!spawn_pending_active());
        ensure_pump_timer();
        assert!(
            PUMP_TIMER.lock().map(|s| s.0.is_some()).unwrap_or(false),
            "前置：泵已挂起"
        );
        spawn_pending_arm();
        assert!(spawn_pending_active());
        stop_pump_timer_if_idle(); // 窗口内不得真停
        assert!(
            PUMP_TIMER.lock().map(|s| s.0.is_some()).unwrap_or(false),
            "窗口内泵应保持挂起（未移除）"
        );
        spawn_pending_disarm();
        assert!(!spawn_pending_active());
        stop_pump_timer_if_idle(); // 常规规则：无层无覆盖 → 真停
        assert!(
            PUMP_TIMER.lock().map(|s| s.0.is_none()).unwrap_or(true),
            "关窗后无层无覆盖 → 泵应自停"
        );
    }
}
