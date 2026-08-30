//! X2 **标题栏配色与主题不一致** E2E 回归（`NINJA_E2E=1` 门控，真实
//! GUI 会话）。
//!
//! 修复前可复现缺陷（用户实测）：窗口顶部标题栏还是白色——系统默认
//! 标题栏画浅色材质底，与终端 ODP 深底割裂（探针取证：标题栏区域平均
//! 色 = (246,242,241)，不是 #282C34）。
//!
//! 修复（shell::apply_theme_chrome，make_window 建窗即套 + theme.set
//! 换色板时 view::apply_theme_all → apply_theme_chrome_all 重套全部终端
//! 窗）：
//! - `titlebarAppearsTransparent` + 窗口背景色 = 生效色板 bg（标题栏与
//!   内容统一，本不动终端网格/布局）；
//! - `titlebarSeparatorStyle = None`（系统 hairline 不横穿统一底色）；
//! - 标题文字/红绿灯随底色明暗自动黑白（theme::is_dark →
//!   vibrantDark/vibrantLight；ODP 深底 = 白字，实测标题文字像素
//!   ≈ 亮色、底 ≈ #282C34）。
//!
//! 三个场景：
//! 1. **启动即主题化**：首窗标题栏像素 ≈ #282C34（ODP）——核心回归；
//! 2. **theme.set 运行时切色板同步换**：ninja-theme（solarized-dark）
//!    连接即推 → 标题栏 ≈ #002B36；杀插件回退 → 标题栏回 ≈ #282C34；
//! 3. **多窗口/标签一致**：selftest tab,win → 两个在屏窗口（其一含
//!    共享标题栏的标签条）标题栏都 ≈ #282C34。
//!
//! 运行前提：可交互 GUI 会话 + 终端的屏幕录制授权（screencapture 取
//! 证）；`cargo build -p ninja-theme`（场景 2）。

mod window_probe;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use window_probe::{near, wait_all_titlebars_near, wait_until};

/// ODP / solarized-dark 背景基线（同 theme_switch E2E）。
const ODP_BG: (u8, u8, u8) = (0x28, 0x2C, 0x34);
const SOLARIZED_BG: (u8, u8, u8) = (0x00, 0x2B, 0x36);

/// 前台互斥锁：screencapture 探在屏窗口，并行宿主窗口互相遮挡会引入
/// 偶发抖动；flock 串行（同 cmdw_surface_close 惯例）。
struct GuiLock(#[allow(dead_code)] std::fs::File);

impl GuiLock {
    fn acquire() -> GuiLock {
        use std::os::unix::io::AsRawFd;
        let f = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(std::env::temp_dir().join("ninja_x2_e2e_gui.lock"))
            .expect("open gui lock");
        // SAFETY: flock 阻塞等待锁；fd 属于我们。
        unsafe {
            libc::flock(f.as_raw_fd(), libc::LOCK_EX);
        }
        GuiLock(f)
    }
}

/// 宿主/插件收割器：drop 即 kill 整个进程组（同 D-A 惯例）。
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
        "ninja_x2_e2e_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// fakesh：占住 PTY（窗口/标题栏是静态 chrome，不需要输出内容）。
fn fakesh(dir: &Path) -> PathBuf {
    let p = dir.join("fakesh.sh");
    fs::write(&p, "#!/bin/bash\nsleep 10000\n").unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// ninja-theme 二进制（宿主 bin 同目录；场景 2 门禁前提）。
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

/// 拉宿主（env 附加；process_group(0) 供收割）。返回 pid。
fn spawn_host(reaper: &mut Reaper, envs: &[(&str, &std::ffi::OsStr)], err: &Path) -> u32 {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ninja"));
    cmd.envs(envs.iter().copied())
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(fs::File::create(err).unwrap());
    let child = cmd.spawn().expect("spawn ninja");
    let pid = child.id();
    reaper.0.push(child);
    pid
}

/// host_pid 的名为 name 的直接子进程 pid（等它出现）。
fn wait_child(host_pid: u32, name: &str, total: Duration) -> Option<u32> {
    wait_until(total, || {
        let out = Command::new("pgrep")
            .args(["-P", &host_pid.to_string(), "-x", name])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse::<u32>()
            .ok()
    })
}

/// 场景 1：启动即主题化——首窗标题栏像素 ≈ #282C34。
/// 修复前：标题栏 = 系统浅色材质 (246,242,241)。
#[test]
fn titlebar_pixels_follow_theme_on_startup() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip: 拉真实窗口进程，设 NINJA_E2E=1 启用");
        return;
    }
    let _gui = GuiLock::acquire();
    let mut reaper = Reaper(Vec::new());
    let dir = sandbox("startup");
    let pid = spawn_host(
        &mut reaper,
        &[("SHELL", fakesh(&dir).as_os_str())],
        &dir.join("err.txt"),
    );
    let shots = wait_all_titlebars_near(pid, ODP_BG, Duration::from_secs(15)).unwrap_or_else(|| {
        panic!(
            "[startup] 首窗标题栏未随 ODP 主题（修复前 = 系统浅色 (246,242,241)；host_err：{:?}）",
            fs::read_to_string(dir.join("err.txt")).unwrap_or_default()
        )
    });
    assert!(
        shots.len() == 1,
        "空载启动应恰一个窗口，探到 {} 个",
        shots.len()
    );
}

/// 场景 2：theme.set 运行时切色板，标题栏同步换 + 插件死亡回退。
/// 修复前：终端内容换 solarized、标题栏仍白（或钉 ODP 后不跟随）。
#[test]
fn titlebar_switches_with_theme_set_and_reverts() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip: 拉真实窗口进程，设 NINJA_E2E=1 启用");
        return;
    }
    let _gui = GuiLock::acquire();
    let mut reaper = Reaper(Vec::new());
    let dir = sandbox("switch");
    let cfg = dir.join("cfg.toml");
    fs::write(
        &cfg,
        format!(
            "[plugins]\nenabled = [\"theme\"]\n\n[plugins.paths]\ntheme = {:?}\n",
            theme_bin()
        ),
    )
    .unwrap();
    let sock = dir.join("x2.sock");
    let pid = spawn_host(
        &mut reaper,
        &[
            ("NINJA_CONFIG", cfg.as_os_str()),
            ("SHELL", fakesh(&dir).as_os_str()),
            ("NINJA_ADE_SOCK", sock.as_os_str()),
            ("NINJA_THEME", "solarized-dark".as_ref()),
        ],
        &dir.join("err.txt"),
    );
    // 启用即拉起：插件连接即推 theme.set → 标题栏应随终端一起换
    // solarized。等插件进程 + 标题栏到位（拉起+connect+推色板+重套）。
    let plugin_pid =
        wait_child(pid, "ninja-theme", Duration::from_secs(20)).expect("ninja-theme 未被拉起");
    let shots =
        wait_all_titlebars_near(pid, SOLARIZED_BG, Duration::from_secs(20)).unwrap_or_else(|| {
            panic!(
                "[switch] theme.set 后标题栏未换 solarized（host_err：{:?}）",
                fs::read_to_string(dir.join("err.txt")).unwrap_or_default()
            )
        });
    assert_eq!(shots.len(), 1);
    // 杀插件（连接死亡）→ 色板回 ODP：标题栏（窗口 chrome，非 Metal
    // drawable）必须同步回退——这正是 X2 的「视觉签名覆盖标题栏区域」。
    unsafe {
        libc::kill(plugin_pid as i32, libc::SIGKILL);
    }
    let back = wait_all_titlebars_near(pid, ODP_BG, Duration::from_secs(15)).unwrap_or_else(|| {
        panic!(
            "[switch] 插件死亡后标题栏未回 ODP（host_err：{:?}）",
            fs::read_to_string(dir.join("err.txt")).unwrap_or_default()
        )
    });
    assert_eq!(back.len(), 1);
}

/// 场景 3：多窗口/标签一致——selftest tab,win → 两个在屏窗口（其一为
/// 标签窗，标题栏区含共享标签条）都 ≈ #282C34。NSWindow tabbing 共享
/// 标题栏也走同一 apply_theme_chrome（每窗重套）。
#[test]
fn titlebar_consistent_across_windows_and_tabs() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip: 拉真实窗口进程，设 NINJA_E2E=1 启用");
        return;
    }
    let _gui = GuiLock::acquire();
    let mut reaper = Reaper(Vec::new());
    let dir = sandbox("multi");
    let pid = spawn_host(
        &mut reaper,
        &[
            ("NINJA_P2_SELFTEST", "tab,win".as_ref()),
            ("SHELL", fakesh(&dir).as_os_str()),
        ],
        &dir.join("err.txt"),
    );
    // selftest 在启动 0.8s 后执行；等「≥2 窗且全部 ≈ ODP」——不能只等
    // 「全部 ≈」：selftest 前只有 1 窗且已主题化，会提前命中。
    let shots = window_probe::wait_until(Duration::from_secs(15), || {
        let s = window_probe::probe_all_titlebars(pid);
        (s.len() >= 2 && s.iter().all(|&c| near(c, ODP_BG))).then_some(s)
    })
    .unwrap_or_else(|| {
        panic!(
            "[multi] 未探到 ≥2 个全部随 ODP 主题的窗口（探到 {:?}；host_err：{:?}）",
            window_probe::probe_all_titlebars(pid),
            fs::read_to_string(dir.join("err.txt")).unwrap_or_default()
        )
    });
    // wait_until 已全窗断言 ≈；这里逐窗再钉一次消息。
    for (i, c) in shots.iter().enumerate() {
        assert!(near(*c, ODP_BG), "窗口 {i} 标题栏 {c:?} != ODP {ODP_BG:?}");
    }
}
