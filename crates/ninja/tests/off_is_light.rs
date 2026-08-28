//! p6 **关掉即轻门禁**的 E2E 取证（`NINJA_E2E=1` 门控，同 layer_preview
//! 惯例）：「启用 preview → 真实用一次 → 禁用 / 被杀 / 正常退出」之后，
//! 宿主必须回到 p2 空载形态——**无插件进程、无 socket、无层、内存回
//! p2 基线**。失败说明插件泄漏进了宿主（PLAN p6「过」的定义）。
//!
//! 三个场景：
//! 1. **用一次→Esc→禁用钩子**（`NINJA_P6_PLUGIN_FILE` 文件触发，同
//!    NINJA_* 惯例）：socket 消失、pgrep 插件空、层探针目录空、
//!    footprint 回 p2 单窗基线（36MB，NOTES.md）+ 容差；随后
//!    **再启用→重绑 socket→再禁用**（同会话禁用/再启用语义）。
//! 2. **SIGKILL 宿主**：preview 因 socket EOF 自退（无残留）；SIGKILL
//!    不跑 Drop → 约定路径留下 socket 尸体 → **下一个启用插件的宿主
//!    启动时清扫**（`sweep_stale_sockets`，pid 已死才删）。
//! 3. **正常退出**（钩子 "quit" → terminate:）：层关过、插件连着的
//!    状态下退出 → 宿主退出、插件无残留、socket 文件被 Drop 清掉。
//!    （CGEventPostToPid 的 ⌘Q 到不了后台应用的菜单系统，实证；
//!    钩子驱动的是同一条产品退出路径。）
//!
//! 运行前提：先 `cargo build -p ninja-preview`（CARGO_BIN_EXE_ninja
//! 同目录解析插件二进制）；Esc/⌘Q 取证需 tools/verify/synth_input.swift
//!（Xcode 工具链；DEVELOPER_DIR 覆盖被显式剥掉，见 post_esc）。

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// p2 空载基线（NOTES.md：单窗 footprint 36MB，Apple Silicon M4）。
const P2_BASELINE_MB: u64 = 36;
/// 容差：IOSurface/Metal 分配器高水位等不可完全归还的碎片。实测校准
///（NOTES.md p6 对照表）：用一次→禁用后 footprint 回到 37MB（p2 基线
/// 36MB，即实测增量 +1MB）；留到 +4MB 保留机器噪声余量，同时仍能
/// 抓住单个 IOSurface 级（~2MB）的泄漏。
const P2_TOLERANCE_MB: u64 = 4;

/// 宿主/插件进程收割器：drop（含 panic 展开）即 kill 整个进程组
///（宿主 + 它拉起的插件 + fakesh）不留孤儿。
struct Reaper(Vec<Child>);

impl Drop for Reaper {
    fn drop(&mut self) {
        for c in self.0.iter_mut() {
            // 每个宿主自己一组（process_group(0)）：kill(-pid) 连插件一起收。
            unsafe {
                libc::killpg(c.id() as i32, libc::SIGKILL);
            }
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn sandbox(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ninja_p6_e2e_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 点击目标目录：路径必须短（80 列网格不折行，同 layer_preview）。
fn short_target_dir(tag: &str) -> PathBuf {
    let dir = PathBuf::from("/tmp").join(format!("np6e2e_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// ADE socket 覆盖路径（场景 1/3 用；场景 2 刻意用约定路径验清扫）。
fn short_sock(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("np6_{tag}_{}.sock", std::process::id()))
}

/// ninja-preview 二进制：宿主 bin 同目录。缺失 = E2E 前提不满足，
/// 直接失败（不静默跳过：门禁）。
fn preview_bin() -> PathBuf {
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_ninja"))
        .parent()
        .expect("target dir")
        .join("ninja-preview");
    assert!(
        bin.is_file(),
        "缺 {bin:?}：先 cargo build -p ninja-preview（E2E 门禁前提）"
    );
    bin
}

/// fakesh：一行可点路径 + 阻塞等宿主退出。
fn fakesh(dir: &Path, line: &str) -> PathBuf {
    let f = dir.join("fakesh.sh");
    std::fs::write(
        &f,
        format!("#!/bin/bash\nprintf '  {line}\\n'\nread _x\n"),
    )
    .unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
    f
}

/// 一个场景的全部句柄（宿主进程本体在 Reaper 里收割；这里只留 pid
/// + 取证路径）。
struct Session {
    host_pid: u32,
    sock: PathBuf,
    probe_dir: PathBuf,
    open_probe: PathBuf,
    state_file: Option<PathBuf>,
    host_err: PathBuf,
    #[allow(dead_code)]
    dir: PathBuf,
}

/// 起一个「启用 preview + 真实用一次」的宿主会话：
/// - `conv_sock`：true = 不设 NINJA_ADE_SOCK（用约定路径，场景 2 验清扫）；
/// - `state_file`：Some = 设 NINJA_P6_PLUGIN_FILE 禁用钩子（场景 1）。
fn launch_used(
    reaper: &mut Reaper,
    tag: &str,
    conv_sock: bool,
    state_file: bool,
) -> Session {
    let dir = sandbox(tag);
    let probe_dir = dir.join("layers");
    std::fs::create_dir_all(&probe_dir).unwrap();
    let open_probe = dir.join("open_probe.txt");
    let sock = if conv_sock {
        // 约定路径（pid 启动后才知，先占位；真实路径由测试自己拼）。
        std::env::temp_dir().join("placeholder.sock")
    } else {
        short_sock(tag)
    };

    // 预览目标：短目录 + 多行真实内容（80 列不折行）。
    let target_dir = short_target_dir(tag);
    let target = target_dir.join("target.rs");
    let mut content = String::new();
    for i in 1..=60 {
        content.push_str(&format!("// line {i} MARKER-P6-E2E off-is-light content\n"));
    }
    std::fs::write(&target, content).unwrap();

    let shell = fakesh(&dir, &format!("{}:7:1", target.display()));
    let cfg = dir.join("cfg.toml");
    std::fs::write(
        &cfg,
        format!(
            "[plugins]\nenabled = [\"preview\"]\n\n[plugins.paths]\npreview = {:?}\n",
            preview_bin()
        ),
    )
    .unwrap();

    let state_path = state_file.then(|| dir.join("plugin_state"));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ninja"));
    cmd.env("NINJA_CONFIG", &cfg)
        .env("SHELL", &shell)
        .env("NINJA_LAYER_PROBE", &probe_dir)
        .env("NINJA_OPEN_PROBE", &open_probe)
        .env("NINJA_P4_HIT", "2,0")
        .env("NINJA_ADE_DEBUG", "1")
        .process_group(0) // 收割时 killpg 连插件一起收（reaper）
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(dir.join("host_err.txt")).unwrap());
    if !conv_sock {
        cmd.env("NINJA_ADE_SOCK", &sock);
    }
    if let Some(p) = &state_path {
        cmd.env("NINJA_P6_PLUGIN_FILE", p);
    }
    let host = cmd.spawn().expect("spawn ninja");
    let host_pid = host.id();
    let real_sock = if conv_sock {
        std::env::temp_dir().join(format!("ninja-ade-{host_pid}.sock"))
    } else {
        sock
    };
    reaper.0.push(host);
    Session {
        host_pid,
        sock: real_sock,
        probe_dir,
        open_probe,
        state_file: state_path,
        host_err: dir.join("host_err.txt"),
        dir,
    }
}

// ---------------------------------------------------------------------------
// 探针/取证小件（同 layer_preview）
// ---------------------------------------------------------------------------

/// PPM（P6）解析：统计「亮」像素数（文本墨迹）。
fn ppm_bright_ink(data: &[u8]) -> Option<usize> {
    let mut pos = 0usize;
    let mut lines: Vec<&[u8]> = Vec::new();
    for _ in 0..3 {
        let nl = data[pos..].iter().position(|&b| b == b'\n')? + pos;
        lines.push(&data[pos..nl]);
        pos = nl + 1;
    }
    if lines[0] != b"P6" || lines[2] != b"255" {
        return None;
    }
    let body = &data[pos..];
    Some(
        body.chunks_exact(3)
            .filter(|px| px[0] > 100 && px[1] > 100 && px[2] > 100)
            .count(),
    )
}

/// 等目录里出现任一 *.ppm（present 取证）。
fn wait_ppm(dir: &Path, total: Duration) -> Option<PathBuf> {
    wait_until(total, || {
        std::fs::read_dir(dir).ok().and_then(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .find(|p| p.extension().is_some_and(|x| x == "ppm"))
        })
    })
}

/// 等文件消失。
fn wait_gone(p: &Path, total: Duration) -> bool {
    wait_until(total, || (!p.exists()).then_some(true)).unwrap_or(false)
}

/// 等文件出现。
fn wait_exists(p: &Path, total: Duration) -> bool {
    wait_until(total, || p.exists().then_some(true))
        .unwrap_or(false)
}

fn wait_until<T>(total: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + total;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn pid_alive(pid: u32) -> bool {
    // SAFETY: kill(pid,0) 只做存在性检查，不发信号。
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn wait_pid_gone(pid: u32, total: Duration) -> bool {
    wait_until(total, || (!pid_alive(pid)).then_some(true)).unwrap_or(false)
}

/// 等待**本测试的子进程**完全退出并收尸（僵尸对 kill(pid,0) 仍
/// “活”：退出后必须 waitpid 收尸才算 gone；宿主 Child 在 Reaper 里，
/// 这里直接 waitpid）。
fn wait_child_reaped(pid: u32, total: Duration) -> bool {
    wait_until(total, || {
        let mut st = 0;
        // SAFETY: waitpid 只碰本地栈变量；WNOHANG 不阻塞。
        let r = unsafe { libc::waitpid(pid as i32, &mut st, libc::WNOHANG) };
        (r == pid as i32).then_some(true)
    })
    .unwrap_or(false)
}

/// host_pid 的名为 name 的直接子进程 pid（pgrep 取证；等它出现）。
fn wait_child(host_pid: u32, name: &str, total: Duration) -> Option<u32> {
    wait_until(total, || {
        let out = Command::new("pgrep")
            .args(["-P", &host_pid.to_string(), "-x", name])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.trim().parse::<u32>().ok()
    })
}

/// host_pid 的全部子进程命令行（残留进程取证）。
fn child_lines(host_pid: u32) -> Vec<String> {
    let out = Command::new("ps")
        .args(["-axo", "ppid=,comm="])
        .output()
        .expect("ps 取证");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let (ppid, comm) = l.split_once(' ')?;
            (ppid.trim() == host_pid.to_string()).then(|| comm.trim().to_string())
        })
        .collect()
}

/// footprint（字节）：取 header 行 `... Footprint: 36.1 MB ...`。
fn footprint_of(pid: u32) -> Option<u64> {
    let out = Command::new("footprint").arg(pid.to_string()).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = text.lines().find(|l| l.contains("Footprint:"))?;
    let rest = line.split("Footprint:").nth(1)?.trim();
    let mut it = rest.split_whitespace();
    let val: f64 = it.next()?.parse().ok()?;
    let unit = it.next().unwrap_or("B");
    let mult = match unit {
        "B" => 1.0,
        "KB" => 1024.0,
        "MB" => 1024.0 * 1024.0,
        "GB" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((val * mult) as u64)
}

/// footprint 稳态值：间隔采样取最小（malloc 高水位抖动去噪）。
fn footprint_settled(pid: u32) -> u64 {
    std::thread::sleep(Duration::from_secs(2));
    let mut best = u64::MAX;
    for _ in 0..3 {
        if let Some(b) = footprint_of(pid) {
            best = best.min(b);
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    best
}

/// 合成键定向投递到宿主进程（tools/verify/synth_input.swift；Esc=53）。
fn post_key(host_pid: u32, code: &str, cmd: bool) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/verify/synth_input.swift");
    let sub = if cmd { "keypidcmd" } else { "keypid" };
    let out = Command::new("swift")
        .arg(&script)
        .args([sub, code, &host_pid.to_string()])
        // 剥掉仓内 .cargo/config.toml 的 DEVELOPER_DIR 覆盖（CLT swift
        // 与自身 SDK 不配套；合成输入要用 xcode-select 的 Xcode）。
        .env_remove("DEVELOPER_DIR")
        .output()
        .expect("swift synth_input");
    eprintln!(
        "key({sub} {code}) post: {} out={:?} err={:?}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "合成键失败（见上行 swift 输出）");
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

/// 合成 Esc 关层（带重试：CGEvent 首键常丢——NOTES p2 实录；层关掉
/// 之后多余的 Esc 落进 PTY 无害，fakesh 的 `read` 只认换行）。返回层
/// 探针文件是否已消失。
fn esc_close_layer(host_pid: u32, ppm: &Path, total: Duration) -> bool {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        post_key(host_pid, "53", false);
        if wait_gone(ppm, Duration::from_secs(2)) {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// 场景
// ---------------------------------------------------------------------------

/// 场景 1（门禁核心）：启用 preview → 真实用一次 → Esc 关层 → 同会话
/// 禁用钩子 → 无插件进程 / socket 消失 / 层探针目录空 / footprint 回
/// p2 基线；再启用→重绑→再禁用（同会话语义）。
#[test]
fn e2e_use_then_disable_no_residue_and_footprint_back_to_p2() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip: 拉真实窗口进程，设 NINJA_E2E=1 启用");
        return;
    }
    let mut reaper = Reaper(Vec::new());
    let s = launch_used(&mut reaper, "off", false, true);

    // —— 真实用一次：层出现且有文本墨迹。
    let ppm = wait_ppm(&s.probe_dir, Duration::from_secs(20))
        .expect("层探针未出现：插件未认领/未 present（看 host_err.txt）");
    let ink = ppm_bright_ink(&std::fs::read(&ppm).expect("读层探针 PPM"))
        .expect("解析层探针 PPM");
    assert!(ink > 2000, "层内文本墨迹不足（{ink}px）：层可能只画了背景");
    let preview_pid = wait_child(s.host_pid, "ninja-preview", Duration::from_secs(10))
        .expect("用一次后预览进程应在跑");
    assert!(s.sock.exists(), "启用期 socket 文件应在");
    assert!(read(&s.open_probe).is_empty(), "插件认领后不应走系统默认打开");

    // —— Esc 关层（真实键路径；层探针文件被删）。CGEvent 首键常丢，
    // 带重试（见 esc_close_layer）。
    assert!(
        esc_close_layer(s.host_pid, &ppm, Duration::from_secs(15)),
        "Esc 后层探针未被删除（关层路径未跑或未摘层）"
    );

    // —— 禁用钩子（文件触发）：socket 消失 = 禁用完成信号。
    let state = s.state_file.as_ref().expect("场景 1 应带禁用钩子");
    std::fs::write(state, "off\n").unwrap();
    assert!(
        wait_gone(&s.sock, Duration::from_secs(10)),
        "禁用后 socket 文件未消失（禁用通路没跑完）"
    );
    // 无残留插件进程（kill+wait 收割完成才删 socket，此时必已退出）。
    assert!(
        wait_pid_gone(preview_pid, Duration::from_secs(10)),
        "禁用后插件进程未退出（宿主泄漏了子进程）"
    );
    assert!(
        !child_lines(s.host_pid)
            .iter()
            .any(|c| c.contains("preview")),
        "禁用后宿主不应再有 preview 子进程：{:?}",
        child_lines(s.host_pid)
    );
    // 层探针目录空（无陈旧层/无隐藏窗口）。
    let leftover: Vec<_> = std::fs::read_dir(&s.probe_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(leftover.is_empty(), "层探针目录非空：{leftover:?}");
    // 宿主 stderr 取证：拉起过 + 禁用过（同一实现的两端）。
    let host_err = read(&s.host_err);
    assert!(host_err.contains("已拉起插件"), "应有拉起日志：{host_err}");
    assert!(host_err.contains("插件已禁用"), "应有禁用日志：{host_err}");

    // —— 内存回 p2 空载（门禁）。
    let fp = footprint_settled(s.host_pid);
    let fp_mb = fp / (1024 * 1024);
    eprintln!("p6 用一次→禁用后 footprint：{fp_mb}MB（p2 基线 {P2_BASELINE_MB}MB + 容差 {P2_TOLERANCE_MB}MB）");
    assert!(
        fp_mb <= P2_BASELINE_MB + P2_TOLERANCE_MB,
        "禁用后未回 p2 空载基线：{fp_mb}MB > {}MB（插件泄漏进了宿主？）",
        P2_BASELINE_MB + P2_TOLERANCE_MB
    );

    // —— 再启用 → 同路径重绑（同会话禁用/再启用语义：spawned 集重置）。
    std::fs::write(state, "on\n").unwrap();
    assert!(
        wait_exists(&s.sock, Duration::from_secs(10)),
        "再启用后 socket 应重绑出现"
    );
    assert!(read(&s.host_err).contains("已再启用"), "应有再启用日志");
    // 再启用≠常驻：没有新分发就不拉进程。
    assert!(
        !child_lines(s.host_pid)
            .iter()
            .any(|c| c.contains("preview")),
        "再启用后无分发不应拉起插件进程"
    );
    // —— 再关一次（往返完整性）。
    std::fs::write(state, "off\n").unwrap();
    assert!(
        wait_gone(&s.sock, Duration::from_secs(10)),
        "再禁用后 socket 文件未消失"
    );
    assert!(
        !child_lines(s.host_pid)
            .iter()
            .any(|c| c.contains("preview")),
        "再禁用后不应有插件进程"
    );
}

/// 场景 2：SIGKILL 宿主 → 插件因 EOF 自退；socket 尸体由下一个启用
/// 插件的宿主启动时清扫（pid 已死才删）。
#[test]
fn e2e_sigkill_host_plugin_self_exits_and_stale_socket_swept() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip: 拉真实窗口进程，设 NINJA_E2E=1 启用");
        return;
    }
    let mut reaper = Reaper(Vec::new());
    // 约定路径（不覆盖 NINJA_ADE_SOCK）：清扫只认 ninja-ade-<pid>.sock。
    let s = launch_used(&mut reaper, "kill", true, false);

    let _ppm = wait_ppm(&s.probe_dir, Duration::from_secs(20))
        .expect("层探针未出现（看 host_err.txt）");
    let preview_pid = wait_child(s.host_pid, "ninja-preview", Duration::from_secs(10))
        .expect("用一次后预览进程应在跑");
    assert!(s.sock.exists(), "约定路径 socket 应在：{:?}", s.sock);

    // SIGKILL 宿主（Drop 不跑——这正是陈旧 socket 的来源）。随后收尸：
    // 僵尸 pid 对 kill(pid,0) 仍返回“活”，清扫会误判保留。
    unsafe { libc::kill(s.host_pid as i32, libc::SIGKILL) };
    {
        let mut st = 0;
        unsafe { libc::waitpid(s.host_pid as i32, &mut st, 0) };
    }

    // 插件因 socket EOF 自退（宿主死后无强杀者）。
    assert!(
        wait_pid_gone(preview_pid, Duration::from_secs(10)),
        "宿主被 SIGKILL 后插件应因 EOF 自退（残留 = 生命周期泄漏）"
    );
    // 尸体在（SIGKILL 不跑 Drop）——清扫的目标场景。
    assert!(
        s.sock.exists(),
        "SIGKILL 后约定路径应留下 socket 尸体：{:?}",
        s.sock
    );

    // 下一个启用插件的宿主启动 → start() 清扫死 pid 的尸体。
    let dir2 = sandbox("kill2");
    let cfg2 = dir2.join("cfg.toml");
    std::fs::write(
        &cfg2,
        format!(
            "[plugins]\nenabled = [\"preview\"]\n\n[plugins.paths]\npreview = {:?}\n",
            preview_bin()
        ),
    )
    .unwrap();
    let host2 = Command::new(env!("CARGO_BIN_EXE_ninja"))
        .env("NINJA_CONFIG", &cfg2)
        .env("SHELL", fakesh(&dir2, "stale"))
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(dir2.join("host_err.txt")).unwrap())
        .spawn()
        .expect("spawn ninja #2");
    let host2_pid = host2.id();
    reaper.0.push(host2);
    let sock2 = std::env::temp_dir().join(format!("ninja-ade-{host2_pid}.sock"));
    assert!(
        wait_exists(&sock2, Duration::from_secs(5)),
        "第二个宿主应绑出自己的 socket"
    );
    assert!(
        wait_gone(&s.sock, Duration::from_secs(10)),
        "死 pid 的陈旧 socket 应被下一个启用宿主清扫：{:?}",
        s.sock
    );
    assert!(
        read(&dir2.join("host_err.txt")).contains("清扫陈旧"),
        "应有清扫日志：{}",
        read(&dir2.join("host_err.txt"))
    );
    // 收尾：第二宿主同样 SIGKILL（drop 不跑），尸体手工清（同
    // idle_no_plugins 惯例）。
    unsafe { libc::kill(host2_pid as i32, libc::SIGKILL) };
    wait_pid_gone(host2_pid, Duration::from_secs(10));
    let _ = std::fs::remove_file(&sock2);
}

/// 场景 3：层开着、插件连着 → ⌘Q 正常退出：宿主 exit 0、插件无残留、
/// socket 被 Drop 清掉、层探针被收层路径删除。
#[test]
fn e2e_normal_quit_no_residue() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip: 拉真实窗口进程，设 NINJA_E2E=1 启用");
        return;
    }
    let mut reaper = Reaper(Vec::new());
    // 带钩子（"quit" 动作驱动正常退出）。
    let s = launch_used(&mut reaper, "quit", false, true);

    let ppm = wait_ppm(&s.probe_dir, Duration::from_secs(20))
        .expect("层探针未出现（看 host_err.txt）");
    let preview_pid = wait_child(s.host_pid, "ninja-preview", Duration::from_secs(10))
        .expect("用一次后预览进程应在跑");

    // 先 Esc 关层（层前台时除 Esc 外的键都路由给插件——⌘Q 会被层吃
    // 掉，这是 p5 钉死的语义；层关后键盘回终端才能退出）。CGEvent 首
    // 键常丢，带重试。
    assert!(
        esc_close_layer(s.host_pid, &ppm, Duration::from_secs(15)),
        "Esc 后层探针未被删除（关层路径未跑）"
    );
    // 插件仍在跑（连接还在）：正常退出（钩子 "quit" → terminate: →
    // run() 返回 → 栈 drop → shutdown：断连 + 收割子进程 + 删 socket）。
    // CGEventPostToPid 的 ⌘Q 到不了后台应用的菜单系统（实证），用钩子
    // 驱动同一条产品退出路径。
    let state = s.state_file.as_ref().expect("场景 3 应带钩子");
    std::fs::write(state, "quit\n").unwrap();
    assert!(
        wait_child_reaped(s.host_pid, Duration::from_secs(10)),
        "正常退出钩子后宿主应退出（terminate: → run() 返回）"
    );
    // 插件无残留（EOF 自退 / shutdown kill+wait 收割）。
    assert!(
        wait_pid_gone(preview_pid, Duration::from_secs(10)),
        "正常退出后插件进程未退出（宿主泄漏了子进程）"
    );
    // socket 文件被 Drop 清掉。
    assert!(
        wait_gone(&s.sock, Duration::from_secs(5)),
        "正常退出后 socket 文件未删除（Drop 未跑 shutdown？）"
    );
}
