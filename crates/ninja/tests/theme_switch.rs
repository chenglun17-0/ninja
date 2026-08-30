//! T2 主题插件原语 E2E 取证（`NINJA_E2E=1` 门控，同 one_dark_startup /
//! off_is_light 惯例）：**插件可换主题**全链路（真实 ninja 二进制 + 真实
//! ninja-theme 插件进程 + 真实 ADE socket）。
//!
//! 前提：先 `cargo build -p ninja-theme`（宿主 bin 同目录解析插件二进制，
//! 同 off_is_light 的 ninja-preview 惯例）。
//!
//! 面板 v2 单一策略（2026-08-29 决策：启用即拉起）：
//!
//! 场景 1（`e2e_theme_zero_click_switch_and_revert_on_kill`）：启用
//! ninja-theme（`NINJA_THEME=solarized-dark`，经 `[plugins.paths]` 指到
//! target/ninja-theme）→ **零点击**：宿主启动即拉起插件（runloop 就绪
//! 后）→ 插件连接后即推 `theme.set` →
//! 1. **像素探针**（`NINJA_DUMP_DRAWABLE`）看到背景 #282C34 →
//!    **#002B36**（solarized-dark base03）——全屏换色没被跳帧吃掉；
//! 2. **OSC 10/11 变化**：fakesh（python）轮询发 OSC 10/11 颜色查询，
//!    应答落盘——前景 rgb:8383/9494/a6a6、背景 rgb:0000/2b2b/3636；
//! 3. **无命中**：open_probe 恒空（一次点击都没发；主题不需命中事件）；
//! 4. **杀插件** → 宿主泵摘连接 → 回 ODP 基线：背景像素回 #282C34、
//!    OSC 11 应答回 rgb:2828/2c2c/3434（p6 收层同语义的色板版）。
//!
//! 场景 2（`e2e_theme_disable_hook_reverts_to_baseline`）：生效后用
//! p6 禁用钩子（`NINJA_P6_PLUGIN_FILE` 写 "off"）→ 同样回 ODP +
//! socket 消失（禁用 = 全链回收：连接、进程、色板覆盖）。
//!
//! 场景 3（`e2e_panel_toggle_writes_back_toml_and_recycles`）：面板
//! 开关（`NINJA_PANEL_PLUGIN_FILE` 编程触发，与面板 checkbox 同一条
//! toggle 路径；"open" 先真实开一次面板窗口）：
//! - "theme off" → 回 ODP + socket 消失 + **ninja.toml 写回正确**
//!   （enabled = []，paths/注释保留）；
//! - "theme on" → 从零重拉（socket 重现 + 重新推色板 + toml 回
//!   enabled = ["theme"]）。
//!
//! X2 补充：标题栏区域（窗口 chrome，非 Metal drawable）同样采样断言
//! ——theme.set 生效时 ≈ #002B36，回退时 ≈ #282C34（window_probe 共用
//! 件 + tools/verify/shot_window.swift 窗口截图探针）。

mod window_probe;

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// ODP 基线背景（T1 钉死）与 solarized-dark 背景（官方 base03）。
const ODP_BG: (u8, u8, u8) = (0x28, 0x2C, 0x34);
const SOLARIZED_BG: (u8, u8, u8) = (0x00, 0x2B, 0x36);
/// OSC 应答里的 hex（每通道重复两遍，vt 钉死格式）。
const ODP_OSC11: &str = "rgb:2828/2c2c/3434";
const SOLARIZED_OSC10: &str = "rgb:8383/9494/9696";
const SOLARIZED_OSC11: &str = "rgb:0000/2b2b/3636";

/// 宿主/插件进程收割器：drop（含 panic 展开）即 kill 整个进程组
/// （宿主 + 它拉起的插件 + fakesh），不留孤儿。
struct Reaper(Vec<Child>);

impl Drop for Reaper {
    fn drop(&mut self) {
        for c in self.0.iter_mut() {
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
        "ninja_t2_e2e_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// ADE socket 覆盖路径（必须短：macOS sun_path 上限 104 字节）。
fn short_sock(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("nt2_{tag}_{}.sock", std::process::id()))
}

/// ninja-theme 二进制：宿主 bin 同目录。缺失 = E2E 前提不满足，直接
/// 失败（不静默跳过：门禁，同 off_is_light 的 preview_bin 惯例）。
fn theme_bin() -> PathBuf {
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_ninja"))
        .parent()
        .expect("target dir")
        .join("ninja-theme");
    assert!(
        bin.is_file(),
        "缺 {bin:?}：先 cargo build -p ninja-theme（E2E 门禁前提）"
    );
    bin
}

/// fakesh（python3，经 pty exec；宿主不给 shell 传参，路径烧进脚本）：
/// 1. 打一行可点路径（行首两格空白，NINJA_P4_HIT 点击列 2 落在 '/' 上）；
/// 2. 轮询发 OSC 10/11 颜色查询，把应答落盘（覆盖写）——主题切换的
///    可观测信号（应答随 vt 默认色换新）。
fn fakesh(dir: &Path) -> PathBuf {
    let f = dir.join("fakesh.py");
    std::fs::write(
        &f,
        format!(
            r#"#!/usr/bin/env python3
import os, select, sys, time, termios
out10 = {out10:?}
out11 = {out11:?}
# pty 默认 canonical 模式：OSC 应答不带换行，永远卡在行缓冲里读不到
#（实测 "(none)"）。改非规范输入 + 关 ECHO（应答回显会被 vt 当 OSC
# 设置吃回环；输出侧 OPOST/ONLCR 保留）。
attrs = termios.tcgetattr(0)
attrs[3] &= ~(termios.ICANON | termios.ECHO)
termios.tcsetattr(0, termios.TCSANOW, attrs)
sys.stdout.write("  /tmp/nt2e/target.txt\n"); sys.stdout.flush()
def query(seq):
    sys.stdout.write(seq); sys.stdout.flush()
    r, _, _ = select.select([0], [], [], 1.0)
    if r:
        try:
            return os.read(0, 4096).decode("utf-8", "replace")
        except OSError:
            return "(err)"
    return "(none)"
# 每拍补一个输出字节：vt 标脏 → 重画 → cyclic 3 槽位像素探针持续
# 刷新为「当前生效色板」（防旧主题帧长期占槽，回退断言才不可空洞）。
deadline = time.time() + 60
while time.time() < deadline:
    try:
        sys.stdout.write("."); sys.stdout.flush()
        open(out10, "w").write(query("\x1b]10;?\x1b\\"))
        open(out11, "w").write(query("\x1b]11;?\x1b\\"))
    except OSError:
        pass
    time.sleep(0.25)
"#,
            out10 = dir.join("osc10.txt"),
            out11 = dir.join("osc11.txt"),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
    f
}

/// 一个会话的全部句柄。
struct Session {
    host_pid: u32,
    sock: PathBuf,
    probe_dir: PathBuf,
    osc10: PathBuf,
    osc11: PathBuf,
    open_probe: PathBuf,
    host_err: PathBuf,
    #[allow(dead_code)]
    dir: PathBuf,
}

/// 起一个「启用 ninja-theme（启动即拉起，零点击）」的宿主会话。
/// `p6_file`/`panel_file`：取证钩子（禁用/面板开关）文件路径。
/// 起一个「启用 ninja-theme（启动即拉起，零点击）」的宿主会话。
/// `p6_file`/`panel_file`：取证钩子（p6 禁用/面板开关）文件路径。
/// `cfg_path`：写真实配置文件（面板开关会写回它；场景 3 验内容）。
fn launch_theme_host(
    reaper: &mut Reaper,
    tag: &str,
    p6_file: Option<&Path>,
    panel_file: Option<&Path>,
    cfg_path: &Path,
) -> Session {
    let dir = sandbox(tag);
    let probe_dir = dir.join("drawable");
    std::fs::create_dir_all(&probe_dir).unwrap();
    let sock = short_sock(tag);
    std::fs::write(
        cfg_path,
        format!(
            "# e2e 配置（面板写回取证）\n[plugins]\nenabled = [\"theme\"]\n\n[plugins.paths]\ntheme = {:?}\n",
            theme_bin()
        ),
    )
    .unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ninja"));
    cmd.env("NINJA_CONFIG", cfg_path)
        .env("SHELL", fakesh(&dir))
        .env("NINJA_DUMP_DRAWABLE", &probe_dir)
        .env("NINJA_OPEN_PROBE", dir.join("open_probe.txt"))
        .env("NINJA_ADE_SOCK", &sock)
        .env("NINJA_THEME", "solarized-dark")
        .process_group(0) // 收割时 killpg 连插件一起收
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(dir.join("host_err.txt")).unwrap());
    if let Some(p) = p6_file {
        cmd.env("NINJA_P6_PLUGIN_FILE", p);
    }
    if let Some(p) = panel_file {
        cmd.env("NINJA_PANEL_PLUGIN_FILE", p);
    }
    let host = cmd.spawn().expect("spawn ninja binary");
    let host_pid = host.id();
    reaper.0.push(host);
    Session {
        host_pid,
        sock,
        probe_dir,
        osc10: dir.join("osc10.txt"),
        osc11: dir.join("osc11.txt"),
        open_probe: dir.join("open_probe.txt"),
        host_err: dir.join("host_err.txt"),
        dir,
    }
}

// ---------------------------------------------------------------------------
// 探针小件（同 one_dark_startup / off_is_light 惯例）
// ---------------------------------------------------------------------------

fn wait_until<T>(total: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + total;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// PPM（P6）解析：返回 (宽, 高, 像素)。
fn parse_ppm(data: &[u8]) -> Option<(usize, usize, &[u8])> {
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
    let dims = String::from_utf8(lines[1].to_vec()).ok()?;
    let (w, h) = dims.split_once(' ')?;
    let (w, h): (usize, usize) = (w.parse().ok()?, h.parse().ok()?);
    Some((w, h, &data[pos..]))
}

/// cyclic 3 槽位里是否出现过「右下角 ≈ 目标背景」的帧（远离文本/光标
/// 的纯 clear 色区；容差 ±2/通道）。
fn any_frame_with_bg(dir: &Path, target: (u8, u8, u8)) -> bool {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            let p = e.path();
            let is_frame = p.extension().is_some_and(|x| x == "ppm")
                && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("frame_"));
            is_frame && {
                let Ok(d) = std::fs::read(&p) else { return false };
                let Some((w, h, px)) = parse_ppm(&d) else { return false };
                let mut ok = true;
                'outer: for y in (h.saturating_sub(16)..h).step_by(2) {
                    for x in (w.saturating_sub(16)..w).step_by(2) {
                        let c = &px[(y * w + x) * 3..(y * w + x) * 3 + 3];
                        if (i32::from(c[0]) - i32::from(target.0)).abs() > 2
                            || (i32::from(c[1]) - i32::from(target.1)).abs() > 2
                            || (i32::from(c[2]) - i32::from(target.2)).abs() > 2
                        {
                            ok = false;
                            break 'outer;
                        }
                    }
                }
                ok
            }
        })
}

/// host_pid 的名为 name 的直接子进程 pid（等它出现；pgrep 取证）。
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

/// 等文件内容包含 needle。
fn wait_file_contains(p: &Path, needle: &str, total: Duration) -> bool {
    wait_until(total, || {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|s| s.contains(needle).then_some(true))
    })
    .unwrap_or(false)
}

fn host_err_of(s: &Session) -> String {
    std::fs::read_to_string(&s.host_err).unwrap_or_default()
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 场景
// ---------------------------------------------------------------------------

/// 起会话 → 启动即拉起（零点击）→ 等 theme.set 生效（像素 + OSC 双
/// 证据）。返回插件 pid（供杀/禁用后的断言）。
fn wait_theme_applied(s: &Session, tag: &str) -> u32 {
    // 启用即拉起：宿主一启动插件进程就在（不是首击才出现）。
    let plugin_pid =
        wait_child(s.host_pid, "ninja-theme", Duration::from_secs(20)).unwrap_or_else(|| {
            panic!(
                "[{tag}] 宿主启动后 ninja-theme 未被拉起（host_err：{:?}）",
                host_err_of(&s)
            )
        });
    // 像素：cyclic 3 槽位出现 solarized 背景帧（拉起+connect+theme.set+
    // 全屏重画；留 20s 余量给首跑 debug 构建）。
    assert!(
        wait_until(Duration::from_secs(20), || {
            any_frame_with_bg(&s.probe_dir, SOLARIZED_BG).then_some(true)
        })
        .unwrap_or(false),
        "[{tag}] 像素探针未见 solarized 背景 {SOLARIZED_BG:?}（theme.set 未生效/被跳帧吃掉？host_err：{:?}）",
        host_err_of(&s)
    );
    // X2 标题栏采样：窗口 chrome（标题栏区域）随 theme.set 同步换——
    // 非 Metal drawable，drawable 探针盖不到（修复前实测白色
    // (246,242,241)）。面板场景会多一个非终端窗（面板不主题化），取
    // 任一窗口命中。
    let titlebar =
        window_probe::wait_any_titlebar_near(s.host_pid, SOLARIZED_BG, Duration::from_secs(10))
            .unwrap_or_else(|| {
                panic!(
                    "[{tag}] 标题栏未随 theme.set 换 solarized {SOLARIZED_BG:?}（host_err：{:?}）",
                    host_err_of(&s)
                )
            });
    assert!(window_probe::near(titlebar, SOLARIZED_BG));
    // OSC 10/11：应答换新（vt 默认色被重钉）。
    assert!(
        wait_file_contains(&s.osc10, SOLARIZED_OSC10, Duration::from_secs(10)),
        "[{tag}] OSC 10 应答未变 solarized 前景（got：{:?}）",
        std::fs::read_to_string(&s.osc10).unwrap_or_default()
    );
    assert!(
        wait_file_contains(&s.osc11, SOLARIZED_OSC11, Duration::from_secs(10)),
        "[{tag}] OSC 11 应答未变 solarized 背景（got：{:?}）",
        std::fs::read_to_string(&s.osc11).unwrap_or_default()
    );
    // 零点击取证：全程无命中分发（open_probe 恒空 = 系统默认从未触发）。
    assert!(
        !s.open_probe.exists() || read(&s.open_probe).is_empty(),
        "[{tag}] 启用即拉起不应需要任何点击/命中（open_probe：{:?}）",
        read(&s.open_probe)
    );
    plugin_pid
}

/// 等回 ODP 基线（像素 + OSC + X2 标题栏三证据）。
fn wait_baseline_back(s: &Session, tag: &str) {
    assert!(
        wait_until(Duration::from_secs(15), || {
            any_frame_with_bg(&s.probe_dir, ODP_BG).then_some(true)
        })
        .unwrap_or(false),
        "[{tag}] 回退后像素探针未见 ODP 背景 {ODP_BG:?}（host_err：{:?}）",
        host_err_of(&s)
    );
    // X2 标题栏采样：回退同样覆盖标题栏（apply_theme_chrome_all 重套；
    // 面板场景多一个非终端窗，取任一命中）。
    let titlebar =
        window_probe::wait_any_titlebar_near(s.host_pid, ODP_BG, Duration::from_secs(10))
            .unwrap_or_else(|| {
                panic!(
                    "[{tag}] 回退后标题栏未回 ODP {ODP_BG:?}（host_err：{:?}）",
                    host_err_of(&s)
                )
            });
    assert!(window_probe::near(titlebar, ODP_BG));
    assert!(
        wait_file_contains(&s.osc11, ODP_OSC11, Duration::from_secs(10)),
        "[{tag}] OSC 11 应答未回 ODP（got：{:?}）",
        std::fs::read_to_string(&s.osc11).unwrap_or_default()
    );
}

/// 插件进程不在（收割完成）。
fn wait_theme_gone(s: &Session, plugin_pid: u32, tag: &str) {
    let gone = wait_until(Duration::from_secs(10), || {
        let out = Command::new("pgrep")
            .args(["-P", &s.host_pid.to_string(), "-x", "ninja-theme"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        (!text.contains(&plugin_pid.to_string())).then_some(true)
    })
    .unwrap_or(false);
    assert!(gone, "[{tag}] 插件进程未退出");
}

#[test]
fn e2e_theme_zero_click_switch_and_revert_on_kill() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip: 拉真实窗口进程，设 NINJA_E2E=1 启用");
        return;
    }
    let mut reaper = Reaper(Vec::new());
    let dir = sandbox("kill");
    let cfg = dir.join("cfg.toml");
    let s = launch_theme_host(&mut reaper, "kill", None, None, &cfg);

    let plugin_pid = wait_theme_applied(&s, "kill");

    // 杀插件进程（SIGKILL = 连接死亡）：宿主泵摘连接 → 色板回 ODP。
    unsafe {
        libc::kill(plugin_pid as i32, libc::SIGKILL);
    }
    wait_baseline_back(&s, "kill");
    // 插件死了不复活（spawned 集「别再试」语义；面板重开才重试）。
    std::thread::sleep(Duration::from_millis(800));
    let out = Command::new("pgrep")
        .args(["-P", &s.host_pid.to_string(), "-x", "ninja-theme"])
        .output()
        .expect("pgrep");
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "插件死后不应被重新拉起"
    );
    // 杀插件 ≠ 禁用插件面：enabled 仍含 theme，socket 应仍在（面板/钩子
    // 的整面关闭语义在场景 2/3 盖）。
    std::thread::sleep(Duration::from_millis(500));
    assert!(s.sock.exists(), "杀插件后 socket 不应消失（那是禁用的语义）");
}

#[test]
fn e2e_theme_disable_hook_reverts_to_baseline() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip: 拉真实窗口进程，设 NINJA_E2E=1 启用");
        return;
    }
    let mut reaper = Reaper(Vec::new());
    let dir = sandbox("off");
    let state = dir.join("plugin_state");
    let cfg = dir.join("cfg.toml");
    let s = launch_theme_host(&mut reaper, "off", Some(&state), None, &cfg);

    let plugin_pid = wait_theme_applied(&s, "off");

    // p6 禁用钩子（文件触发）：shutdown = 收层/断连接/收割进程/删
    // socket + **色板覆盖回基线**（T2 与 p6 同语义）。
    std::fs::write(&state, "off\n").unwrap();
    // socket 消失 = 禁用完成（p6 可观测信号）。
    assert!(
        wait_until(Duration::from_secs(10), || {
            (!s.sock.exists()).then_some(true)
        })
        .unwrap_or(false),
        "[off] 禁用后 socket 未消失（host_err：{:?}）",
        host_err_of(&s)
    );
    wait_baseline_back(&s, "off");
    wait_theme_gone(&s, plugin_pid, "off");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 面板开关全链（编程触发，同 checkbox 的 toggle 路径；不依赖合成
/// CGEvent）：
/// 1. "open" 真开一次面板窗口（构建/显示路径不炸，宿主仍活）；
/// 2. "theme off" → 杀进程 + 回 ODP + socket 消失 + ninja.toml 写回
///    enabled = []（paths 段与首行注释保留）；
/// 3. "theme on" → 从零重拉：socket 重现、色板重新生效、toml 回
///    enabled = ["theme"]。
#[test]
fn e2e_panel_toggle_writes_back_toml_and_recycles() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip: 拉真实窗口进程，设 NINJA_E2E=1 启用");
        return;
    }
    let mut reaper = Reaper(Vec::new());
    let dir = sandbox("panel");
    let state = dir.join("panel_cmds");
    let cfg = dir.join("cfg.toml");
    let s = launch_theme_host(&mut reaper, "panel", None, Some(&state), &cfg);

    let plugin_pid = wait_theme_applied(&s, "panel");
    let theme_path = theme_bin();

    // —— 面板窗口先真开一次（窗口构建/1s 刷新 timer 不炸）。钩子去抖：
    //    每写一条等宿主消化一拍以上。
    std::fs::write(&state, "open\n").unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        unsafe { libc::kill(s.host_pid as i32, 0) == 0 },
        "面板打开后宿主不应崩溃"
    );

    // —— 面板 off：立即杀 + 回收 + 写回 toml。
    std::fs::write(&state, "theme off\n").unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || {
            (!s.sock.exists()).then_some(true)
        })
        .unwrap_or(false),
        "[panel] 面板 off 后 socket 未消失（host_err：{:?}）",
        host_err_of(&s)
    );
    wait_baseline_back(&s, "panel");
    wait_theme_gone(&s, plugin_pid, "panel");
    // ninja.toml 写回：enabled = []，其余字节（注释/paths）保留。
    let toml = wait_until(Duration::from_secs(5), || {
        let t = read(&cfg);
        t.contains("enabled = []").then_some(t)
    })
    .unwrap_or_else(|| read(&cfg));
    assert!(
        toml.contains("enabled = []"),
        "[panel] toml 应写回 enabled = []，得到：{toml:?}"
    );
    assert!(
        toml.contains(&format!("theme = {theme_path:?}")),
        "[panel] 写回不得丢 paths 段：{toml:?}"
    );
    assert!(
        toml.contains("# e2e 配置"),
        "[panel] 写回不得抹掉注释：{toml:?}"
    );

    // —— 面板 on：从零重拉（socket 重现 + 色板重新生效 + toml 回写）。
    std::fs::write(&state, "theme on\n").unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || {
            s.sock.exists().then_some(true)
        })
        .unwrap_or(false),
        "[panel] 面板 on 后 socket 应重绑出现"
    );
    // 色板重新生效（新的插件进程重新推 theme.set）。
    assert!(
        wait_until(Duration::from_secs(15), || {
            any_frame_with_bg(&s.probe_dir, SOLARIZED_BG).then_some(true)
        })
        .unwrap_or(false),
        "[panel] 面板 on 后色板应重新生效（像素探针未见 solarized）"
    );
    let toml = wait_until(Duration::from_secs(5), || {
        let t = read(&cfg);
        t.contains("enabled = [\"theme\"]").then_some(t)
    })
    .unwrap_or_else(|| read(&cfg));
    assert!(
        toml.contains("enabled = [\"theme\"]"),
        "[panel] toml 应写回 enabled = [\"theme\"]，得到：{toml:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
