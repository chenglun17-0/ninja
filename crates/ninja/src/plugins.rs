//! p3：宿主侧 ADE 插件门（Unix socket，默认关）；p4：命中分发。
//!
//! 空载门禁：`[plugins] enabled` 为空（默认）时**不创建 socket 文件、
//! 不拉任何插件进程**——[`PluginHost::start`] 直接返回 `None`，宿主
//! 进程里没有任何插件运行时（验证：`cargo tree -p ninja` 无
//! wasmtime/tokio；默认配置启动后 socket 路径不存在，见
//! `tests/idle_no_plugins.rs` 的运行时取证）。
//!
//! 启用时：绑定 [`socket_path`] 约定的路径并 listen；拉插件进程/
//! 握手是 p5 的事。p4 只做**命中分发**：点击时把 [`Hit`] 广播给已
//! 连上的插件（连接由插件自己连进来，分发时按需非阻塞 accept），
//! 收集 `hit.claim` / `hit.ignore` 回执——全 ignore / 静默 / 断连
//! 一律视为不认领，走系统默认打开。消息编解码类型来自
//! `ninja-protocol`（协议仍只经 socket 交换字节，双方不共享地址空间）。
//!
//! 超时策略：**同步短超时**（[`HIT_REPLY_TIMEOUT`]，默认 500ms）——只
//! 在 Cmd+点击的手势路径上发生一次，不新增任何常驻线程；超预算即
//! 降级为 ignore，绝不卡死主 runloop。

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use ninja_protocol::frame::{FrameDecoder, encode_frame};
use ninja_protocol::{Hit, Message};

/// `[plugins]` 配置（ninja.toml）。默认空 = 插件全关（空载门禁）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginsConfig {
    /// 启用的插件名列表。空 = 关。p5 才真正按名字拉起插件进程。
    pub enabled: Vec<String>,
}

/// 已绑定的 ADE socket 句柄。Drop 时删除 socket 文件（不留残骸）。
#[derive(Debug)]
pub struct PluginHost {
    listener: UnixListener,
    path: PathBuf,
    /// p4：已连上的插件连接（分发时按需 accept 进来）。每条连接各带
    /// 一个帧解码器（半帧状态跨读保留）。
    conns: Vec<Conn>,
    /// hit id 发号器（回执配对用；从 1 起，0 留给「未知」）。
    next_hit_id: u64,
}

#[derive(Debug)]
struct Conn {
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

/// socket 路径约定：`${TMPDIR:-/tmp}/ninja-ade-{pid}.sock`。
pub fn socket_path() -> PathBuf {
    let pid = std::process::id();
    std::env::temp_dir().join(format!("ninja-ade-{pid}.sock"))
}

/// 实际生效路径：`NINJA_ADE_SOCK` 覆盖（测试钩子；p5 拉插件进程时
/// 也经同名环境变量告知路径）。
fn effective_socket_path() -> PathBuf {
    match std::env::var_os("NINJA_ADE_SOCK") {
        Some(p) => PathBuf::from(p),
        None => socket_path(),
    }
}

impl PluginHost {
    /// 唯一入口：按配置决定绑不绑 socket。
    ///
    /// - `enabled` 为空 → `None`：**不建 socket、不碰文件系统**（空载
    ///   不变量）。
    /// - 非空 → 绑定 + listen（非阻塞：p3 不 accept，内核排队）；
    ///   绑定失败不炸终端：stderr 警告 + `None`（同配置模块的降级哲学）。
    pub fn start(cfg: &PluginsConfig) -> Option<PluginHost> {
        if cfg.enabled.is_empty() {
            return None;
        }
        Self::bind(effective_socket_path())
    }

    /// 在给定路径上绑定（start 的实现核心；测试用隔离目录直调）。
    fn bind(path: PathBuf) -> Option<PluginHost> {
        // 极端场景：同 pid 复用留下陈旧文件。先清再绑。
        let _ = std::fs::remove_file(&path);
        match UnixListener::bind(&path) {
            Ok(listener) => {
                // p3 不 accept：非阻塞，避免任何路径卡 runloop；连接在
                // 内核 backlog 排队，等 p5 的监督器接管。
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

    /// 监听器引用（p5 监督器接管 accept 用）。
    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }

    // ------------------------------------------------------------------
    // p4 命中分发
    // ------------------------------------------------------------------

    /// 发下一个 hit id（回执配对用）。点击路径主线程调用。
    pub fn next_hit_id(&mut self) -> u64 {
        self.next_hit_id = self.next_hit_id.saturating_add(1);
        self.next_hit_id
    }

    /// 把 hit 广播给所有已连插件，收集 claim/ignore，仲裁出结果。
    /// 超时用 [`HIT_REPLY_TIMEOUT`]（生产入口；单测用带超时参数的
    /// [`PluginHost::dispatch_hit_with_timeout`]）。
    pub fn dispatch_hit(&mut self, hit: &Hit) -> DispatchOutcome {
        self.dispatch_hit_with_timeout(hit, HIT_REPLY_TIMEOUT)
    }

    /// 按需非阻塞 accept：把内核 backlog 里排队的插件连接收进来。
    /// 不新增线程、不拉进程——没连接就是空操作（p5 才有监督器）。
    fn pump_accept(&mut self) {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    // 分发路径用阻塞读 + 读超时（收口在超时预算内）。
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(HIT_REPLY_TIMEOUT));
                    self.conns.push(Conn {
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
    /// 流程：accept 排队连接 → 广播 hit 帧 → 逐连接收回执（共享
    /// deadline；静默/断连/坏消息部视为 ignore，坏协议断开连接）。
    /// 仲裁：claim 的 priority 最大者胜，平局先连者胜。
    pub(crate) fn dispatch_hit_with_timeout(
        &mut self,
        hit: &Hit,
        timeout: Duration,
    ) -> DispatchOutcome {
        self.pump_accept();
        if self.conns.is_empty() {
            return DispatchOutcome::NoPlugins;
        }

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
            self.conns.remove(*i);
        }
        if self.conns.is_empty() {
            return DispatchOutcome::AllIgnored; // 广播全失败 = 无认领
        }

        // 收阶段：共享 deadline，逐连接收；responded 后不再读它。
        let deadline = Instant::now() + timeout;
        let mut best: Option<u32> = None;
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
                                        if best.is_none_or(|pr| m.priority > pr) {
                                            best = Some(m.priority);
                                        }
                                        responded[i] = true;
                                    }
                                    Ok(Message::HitIgnore(m)) if m.id == hit.id => {
                                        responded[i] = true;
                                    }
                                    Ok(_) => {} // 其余消息/别的 id：忽略（p5 消化）
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
            self.conns.remove(*i);
        }
        match best {
            Some(priority) => DispatchOutcome::Claimed { priority },
            None => DispatchOutcome::AllIgnored,
        }
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
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

/// 点击路径一站式入口：广播 hit 并仲裁。无分发器/锁坏 → NoPlugins
///（即未启用插件 → 系统默认打开）。
pub fn dispatch_hit(hit: &Hit) -> DispatchOutcome {
    match take_dispatcher() {
        Some(host) => host
            .lock()
            .map(|mut h| h.dispatch_hit(hit))
            .unwrap_or(DispatchOutcome::NoPlugins),
        None => DispatchOutcome::NoPlugins,
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
        // 因此任何路径上都不会出现 socket 文件/监听。
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
            let host = PluginHost::bind(sock.clone()).expect("显式绑定应成功");
            assert_eq!(host.path(), sock.as_path());
            assert!(sock.exists(), "绑定后 socket 文件应在");
            // listen 已生效：客户端能连上（内核排队，p3 不 accept）。
            UnixStream::connect(&sock).expect("启用后可连接（排队，不 accept）");
        } // host drop → 文件清除
        assert!(!sock.exists(), "drop 后 socket 文件应删除");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enabled_via_start_uses_convention_path() {
        // 走真实 start()（含 env 覆盖逻辑）：启用非空 → 绑生效路径
        //（NINJA_ADE_SOCK 设置时用它，否则约定路径）。
        let cfg = PluginsConfig {
            enabled: vec!["preview".into()],
        };
        let expected = match std::env::var_os("NINJA_ADE_SOCK") {
            Some(p) => PathBuf::from(p),
            None => socket_path(),
        };
        {
            let host = PluginHost::start(&cfg).expect("启用即绑");
            assert_eq!(host.path(), expected.as_path());
            assert!(expected.exists());
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
        assert!(PluginHost::bind(bad).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------------
    // p4 命中分发（进程内 UnixStream 对端；不拉任何真实进程）
    // ------------------------------------------------------------------

    use ninja_protocol::{HitClaim, HitIgnore, HitKind, Modifier};
    use std::io::{Read, Write};
    use std::thread::{self, JoinHandle};

    fn sample_hit(id: u64) -> Hit {
        Hit::new(id, HitKind::Path, "/tmp/x.rs:1:2", 3, 5, 9, vec![Modifier::Cmd])
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
        let host = PluginHost::bind(sock.clone()).expect("bind");
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
        let mut host = PluginHost::bind(dir.join("a.sock")).unwrap();
        let hit = sample_hit(host.next_hit_id());
        assert_eq!(
            host.dispatch_hit_with_timeout(&hit, Duration::from_millis(10)),
            DispatchOutcome::NoPlugins
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dispatch_peer_ignore_falls_back_and_hit_fields_complete() {
        let (mut host, handles) = host_with_peers("dispign", vec![PeerReply::Ignore]);
        let hit = sample_hit(host.next_hit_id());
        assert_eq!(
            host.dispatch_hit_with_timeout(&hit, Duration::from_millis(300)),
            DispatchOutcome::AllIgnored
        );
        let payload = handles.into_iter().next().unwrap().join().unwrap();
        // 插件收到的必须是完整 hit 字段（够预览认领，对齐 golden 样例）。
        match Message::decode_host(&payload).unwrap() {
            Message::Hit(h) => {
                assert_eq!(h.kind, HitKind::Path);
                assert_eq!(h.text, "/tmp/x.rs:1:2");
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
        // 单 claim。
        let (mut host, handles) = host_with_peers("dispc1", vec![PeerReply::Claim(7)]);
        let hit = sample_hit(host.next_hit_id());
        assert_eq!(
            host.dispatch_hit_with_timeout(&hit, Duration::from_millis(300)),
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
            host.dispatch_hit_with_timeout(&hit, Duration::from_millis(300)),
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
            host.dispatch_hit_with_timeout(&hit, Duration::from_millis(300)),
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
            host.dispatch_hit_with_timeout(&hit, Duration::from_millis(80)),
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
            host.dispatch_hit_with_timeout(&hit, Duration::from_millis(300)),
            DispatchOutcome::AllIgnored
        );
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn dispatcher_weak_dies_with_host_and_free_entry_works() {
        // Weak 通路：登记 → 可取；宿主释放 → 自动失效（退出时 drop 删
        // socket 的生命周期不变量）。
        let dir = sandbox("dispwk");
        let arc = Arc::new(Mutex::new(
            PluginHost::bind(dir.join("a.sock")).unwrap(),
        ));
        install_dispatcher(&arc);
        assert!(take_dispatcher().is_some());
        assert!(next_hit_id() >= 1);
        drop(arc);
        assert!(take_dispatcher().is_none());
        // 失效后走自由函数：NoPlugins（即系统默认），不 panic。
        assert_eq!(
            dispatch_hit(&sample_hit(1)),
            DispatchOutcome::NoPlugins
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
