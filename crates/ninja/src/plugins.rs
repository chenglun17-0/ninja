//! p3：宿主侧 ADE 插件门（Unix socket，默认关）；p4：命中分发；
//! p5：插件监督器（首次命中拉起进程）+ 层状态机（open→ready→present/
//! close）+ 层前台输入路由；p6：**关掉即轻**——插件死亡收层
//! （[`layer::close_by_conn`]）、同会话禁用（[`PluginHost::shutdown`]，
//! [`Drop`] 复用同一实现）、陈旧 socket 清扫（[`sweep_stale_sockets`]）。
//! 禁用/退出/崩溃之后：无插件进程、无 socket、无层，内存回空载。
//!
//! 空载门禁：`[plugins] enabled` 为空（默认）时**不创建 socket 文件、
//! 不拉任何插件进程**——[`PluginHost::start`] 直接返回 `None`，宿主
//! 进程里没有任何插件运行时（验证：`cargo tree -p ninja` 无
//! wasmtime/tokio；默认配置启动后 socket 路径不存在，见
//! `tests/idle_no_plugins.rs` 的运行时取证）。
//!
//! 启用时：绑定 [`socket_path`] 约定的路径并 listen。**启用 ≠ 常驻**
//! （PRODUCT 规则）：进程拉起发生在**首次命中分发**——按名解析二进制
//! （`[plugins] paths` → `$NINJA_PLUGIN_DIR/<name>` → 宿主二进制同目录），
//! spawn 并以 `NINJA_ADE_SOCK` 告知 socket 路径；解析失败/拉不起 =
//! 该插件降级为不存在（stderr 一行警告，绝不弹 UI）。
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
//! 超时策略：**同步短超时**——claim 汇集 [`HIT_REPLY_TIMEOUT`]（500ms），
//! 层握手 [`LAYER_HANDSHAKE_TIMEOUT`]（1.5s，只在有插件认领时进入）；
//! 都发生在点击手势路径上的一次性开销，不新增常驻线程；超预算即降级，
//! 绝不卡死主 runloop。

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use ninja_protocol::frame::{FrameDecoder, encode_frame};
use ninja_protocol::{Hit, InputKey, LayerClose, Message, Modifier};

use crate::layer::{self, LayerGeom};

/// `[plugins]` 配置（ninja.toml）。默认空 = 插件全关（空载门禁）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginsConfig {
    /// 启用的插件名列表。空 = 关。首次命中分发时按名拉起（启用≠常驻）。
    pub enabled: Vec<String>,
    /// 插件名 → 二进制路径（p5 拉起用）。缺省时按名在
    /// `$NINJA_PLUGIN_DIR/<name>` / 宿主二进制同目录解析。
    pub paths: std::collections::HashMap<String, String>,
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
    /// p5 监督器：已拉起（或已放弃）的插件名。启用≠常驻：真正的 spawn
    /// 发生在首次分发（`ensure_spawned`），这里只记「别再试」。
    spawned: std::collections::BTreeSet<String>,
    /// 拉起的插件进程（Drop 收割；宿主退出时它们也会因 socket EOF 自退）。
    children: Vec<std::process::Child>,
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

/// 按名解析插件二进制：`[plugins.paths]` 显式路径 → `$NINJA_PLUGIN_DIR/
/// <name>` → 宿主二进制同目录 `<name>`。都不存在 → None（调用方降级）。
pub fn resolve_plugin_binary(name: &str, cfg: &PluginsConfig) -> Option<PathBuf> {
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
    // p5 监督器：首次分发时拉起启用的插件
    // ------------------------------------------------------------------

    /// 拉起尚未尝试过的启用插件。PRODUCT：启用≠常驻——只在命中分发
    /// 路径调用（首次点击才 spawn）。已有连接在（如外部自连的测试
    /// 插件/插件已拉起）时不重复拉：v0 无握手，宿主无法把连接映射回
    /// 名字，按「有连接就够」处理。
    fn ensure_spawned(&mut self) {
        if !self.conns.is_empty() {
            return;
        }
        for name in self.cfg.enabled.clone() {
            if !self.spawned.insert(name.clone()) {
                continue; // 已试过（成功或失败都不重试本次会话）
            }
            let Some(bin) = resolve_plugin_binary(&name, &self.cfg) else {
                eprintln!(
                    "ninja: 插件 {name:?} 找不到二进制（[plugins.paths] / NINJA_PLUGIN_DIR / 宿主同目录），本次降级为未启用"
                );
                continue;
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
                    self.children.push(child);
                }
                Err(e) => {
                    eprintln!("ninja: 插件 {name:?}（{}）拉起失败：{e}", bin.display());
                }
            }
        }
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
    /// 未启用）。
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
            // p5 冷启动：首次分发才拉插件；等 connect 的预算独立于回执
            // 预算（拉起只付一次，回执仍钉 HIT_REPLY_TIMEOUT）。测试注入
            // 的短 timeout 也约束冷启动等待；全部插件都试过（拉不起/
            // 已死）就即刻降级，不再等。
            let can_spawn = self
                .cfg
                .enabled
                .iter()
                .any(|n| !self.spawned.contains(n));
            if !can_spawn {
                return DispatchOutcome::NoPlugins;
            }
            ade_debug("dispatch: 无连接，冷启动拉插件");
            let t_spawn = Instant::now();
            self.ensure_spawned();
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
        if let Some(geom) = geom {
            if let Some(idx) = self.conns.iter().position(|c| c.id == claim_conn) {
                self.layer_handshake(idx, geom, LAYER_HANDSHAKE_TIMEOUT);
            }
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
    fn drop_conn(&mut self, idx: usize) {
        let Some(c) = self.conns.get(idx) else {
            return;
        };
        let conn_id = c.id;
        self.conns.remove(idx);
        if !layer::close_by_conn(conn_id).is_empty() {
            ade_debug(&format!("conn {conn_id} 死亡：已回收其全部层"));
        }
        stop_pump_timer_if_idle();
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
        for (handle, conn, _pane) in layer::close_all() {
            let _ = self.send_to_conn(conn, &Message::LayerClose(LayerClose::new(handle)));
        }
        stop_pump_timer_if_idle();
        self.conns.clear();
        for c in self.children.iter_mut() {
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
    if layer::any_layers() {
        return;
    }
    if let Ok(mut slot) = PUMP_TIMER.lock() {
        if let Some(t) = slot.0.take() {
            if let Some(main) = objc2_core_foundation::CFRunLoop::main() {
                // SAFETY: t 曾加入主 runloop。
                unsafe {
                    main.remove_timer(
                        Some(&t),
                        objc2_core_foundation::kCFRunLoopCommonModes,
                    )
                };
            }
        }
    }
}

/// 泵入口（timer 回调直调；测试可直调）。
pub fn pump_now() {
    if !layer::any_layers() {
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
// 全局分发器：view（Cmd+点击）→ PluginHost 的通路
// ---------------------------------------------------------------------------
//
// PluginHost 本体住在 app::run() 的栈上（生命周期 = 进程，退出时 drop
// 删 socket 文件）；这里只登记 Weak——宿主退出时栈上的 Arc 先释放，
// 静态槽里的 Weak 自动失效，Drop 照常跑。只在主线程读写（点击路径
// 本就主线程），Mutex 只为满足 static 要求。

static DISPATCHER: Mutex<Option<Weak<Mutex<PluginHost>>>> = Mutex::new(None);

/// 登记全局分发器（app::run 启用插件时调一次；空载不调）。
pub fn install_dispatcher(host: &Arc<Mutex<PluginHost>>) {
    if let Ok(mut slot) = DISPATCHER.lock() {
        *slot = Some(Arc::downgrade(host));
    }
}

/// 取当前分发器（没装 / 已随宿主退出失效 → None）。
pub fn take_dispatcher() -> Option<Arc<Mutex<PluginHost>>> {
    DISPATCHER
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().and_then(Weak::upgrade))
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
        if let Some(host) = take_dispatcher() {
            if let Ok(mut h) = host.lock() {
                let _ = h.send_to_conn(conn, &Message::LayerClose(LayerClose::new(handle)));
            }
        }
    }
    stop_pump_timer_if_idle();
}

/// 宿主退出收口（p6）：`NSApplication terminate:` 直接 `exit(0)`，
/// `app.run()` 不返回、Rust 栈展开不发生——`PluginHost::Drop` 在⌘Q/
/// 关最后窗的正常退出路径上**不会跑**（E2E 实测：socket 尸体不只是
/// SIGKILL 的产物）。`applicationWillTerminate` 里显式调本函数（幂
/// 等；与 Drop 同一实现）。
pub fn host_shutdown() {
    if let Some(host) = take_dispatcher() {
        if let Ok(mut h) = host.lock() {
            h.shutdown();
        }
    }
}

/// p6 同会话禁用 / 再启用（取证钩子 `NINJA_P6_PLUGIN_FILE` 驱动；
/// 产品 UI 归后续阶段）。
/// - 禁用 = 现任 host [`PluginHost::shutdown`]（幂等：收层/断连接/
///   收割子进程/删 socket）；
/// - 再启用 = 新绑一个 host 换进分发器同一槽位——`spawned` 集随新
///   对象重置（下次分发重新拉起）、socket 重绑，即「禁用→再启用」
///   的完整语义；旧 host 的 [`Drop`] 是幂等空转。
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
            eprintln!("ninja: 插件已再启用（socket {bound:?} 已重绑，spawned 集已重置）");
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
        //（NINJA_ADE_SOCK 设置时用它，否则约定路径）。不触发任何 spawn
        //（拉起只在首次分发；本测试不分发）。
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
            assert!(host.children.is_empty(), "启用不等于拉起（首次分发才拉）");
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
    fn dispatcher_weak_dies_with_host_and_free_entry_works() {
        // Weak 通路：登记 → 可取；宿主释放 → 自动失效（退出时 drop 删
        // socket 的生命周期不变量）。分发器槽全局：与其它装槽的测试
        // 串行（见 DISPATCHER_TEST_LOCK）。
        let _g = DISPATCHER_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = sandbox("dispwk");
        let arc = Arc::new(Mutex::new(
            PluginHost::bind(dir.join("a.sock"), PluginsConfig::default()).unwrap(),
        ));
        install_dispatcher(&arc);
        assert!(take_dispatcher().is_some());
        assert!(next_hit_id() >= 1);
        drop(arc);
        assert!(take_dispatcher().is_none());
        // 失效后走自由函数：NoPlugins（即系统默认），不 panic。
        assert_eq!(
            dispatch_hit(&sample_hit(1), None),
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
    /// dispatcher_weak_dies… 测试）。
    static DISPATCHER_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn host_set_enabled_disable_reenable_cycle() {
        // p6 钩子通路（NINJA_P6_PLUGIN_FILE → host_set_enabled）：
        // 禁用 → socket 消失；再启用 → 同路径重绑（换新 host）。
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
        install_dispatcher(&arc);
        assert!(sock.exists());

        assert!(host_set_enabled(false));
        assert!(!sock.exists(), "禁用后 socket 文件应消失");

        assert!(host_set_enabled(true));
        assert!(sock.exists(), "再启用应重绑同一路径");

        // 再关一次 + 宿主退出路径（drop Arc → Drop → 幂等空转）。
        assert!(host_set_enabled(false));
        drop(arc);
        assert!(take_dispatcher().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
