//! D-A **⌘W 关整窗** 的 E2E 回归（`NINJA_E2E=1` 门控，真实 GUI 会话）。
//!
//! 修复前可复现缺陷：多 pane（分屏）窗按 ⌘W（菜单 Close=performClose:）
//! 直接整窗关闭——**所有** pane 的 shell 一起 SIGHUP 陪葬，正是用户
//! 「多标签时 ⌘W 实际关了整个窗口」的日报（多面同窗在用户口径里都算
//! 「标签」；NSWindow 原生 tab 的 ⌘W 本就只关当前 tab，实测 12/12 场景
//! 在修复前后都正确，这里一并钉死语义，防回归）。
//!
//! 修复（shell.rs `should_close_whole_window` + AppDelegate
//! `windowShouldClose:`）：裸 ⌘W 只关「当前面」——多 pane 窗关焦点
//! pane（其余 pane 各自 PTY 独立、shell 绝不陪葬），单 pane 放行原生
//! 语义（关当前 tab，最后一个 tab 才关窗）。非 ⌘W 路径（红绿灯、
//! ⇧⌘W Close Window/Close Pane、⌥⌘W、EOF 级联、selftest）不受影响。
//!
//! 四个场景（全部真实二进制 + 真实 CGEvent 前台投递，⌘W 必须走菜单
//! 系统才等价于用户按键——p6 实证 CGEventPostToPid 到不了菜单系统）：
//! 1. **3 分屏 ⌘W×3**：每按一次恰好关一个面（shell 3→2→1，窗在、
//!    进程活），最后一次才关窗退出。修复前：第一下就整窗关 + 进程退。
//! 2. **3 标签 ⌘W**：只关当前 tab（shell 3→2、窗在、进程活）——
//!    钉死 macOS 原生 tab 语义不被本修复破坏。
//! 3. **红绿灯（鼠标路径）**：分屏窗点红绿灯 = 整窗关（原生），进程退。
//! 4. **⇧⌘W（Close Pane 键）**：分屏窗关一个 pane，窗在、进程活。
//!
//! 运行前提同 layer_preview：Xcode 工具链（DEVELOPER_DIR 覆盖被显式
//! 剥掉）、可交互 GUI 会话（前台激活 + 全局 CGEvent）。

use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// 前台互斥锁：4 个场景都要 activate 自己的宿主（⌘W 走菜单系统必须
/// 前台），并行会互抢前台。flock 全局锁串行化（零依赖，Drop 释放）。
struct GuiLock(File);

impl GuiLock {
    fn acquire() -> GuiLock {
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(std::env::temp_dir().join("ninja_da_e2e_gui.lock"))
            .expect("open gui lock");
        // SAFETY: flock 阻塞等待锁；fd 属于我们。
        unsafe {
            libc::flock(f.as_raw_fd(), libc::LOCK_EX);
        }
        GuiLock(f)
    }
}

/// 宿主收割器：drop（含 panic 展开）即 kill 整个进程组，fakesh 的
/// `sleep` 不留孤儿（同 off_is_light 惯例）。
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

/// fakesh：输出一行即挂起（占住 PTY，存活可从宿主子进程数读出）。
fn fakesh(dir: &Path) -> std::path::PathBuf {
    let p = dir.join("fakesh.sh");
    std::fs::write(&p, "#!/bin/bash\nsleep 10000\n").unwrap();
    make_executable(&p);
    p
}

fn make_executable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(p).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(p, perm).unwrap();
}

/// 拉起宿主（selftest 序列在启动 0.8s 后执行；process_group(0) 供收割）。
fn spawn_host(reaper: &mut Reaper, dir: &Path, selftest: &str, shell: &Path) -> u32 {
    let child = Command::new(env!("CARGO_BIN_EXE_ninja"))
        .env("NINJA_P2_SELFTEST", selftest)
        .env("SHELL", shell)
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(dir.join("host_err.txt")).unwrap())
        .spawn()
        .expect("spawn ninja");
    let pid = child.id();
    reaper.0.push(child);
    pid
}

/// synth_input.swift 子命令（activate/wincount/key；剥 DEVELOPER_DIR，
/// 同 off_is_light.post_key：CLT swift 与自身 SDK 不配套）。
fn synth(args: &[&str]) -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/verify/synth_input.swift");
    let out = Command::new("swift")
        .arg(&script)
        .args(args)
        .env_remove("DEVELOPER_DIR")
        .output()
        .expect("swift synth_input");
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        out.status.success(),
        "synth_input {args:?} 失败: {} (stderr={:?})",
        stdout,
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn alive(pid: u32) -> bool {
    // kill(pid,0) 只探测存在性；宿主退出但未被 wait 时是 zombie，
    // 也返回 0——用 ps 状态排除（zombie/undead 都算死）。
    // SAFETY: kill(pid,0) 不发信号。
    if unsafe { libc::kill(pid as i32, 0) } != 0 {
        return false;
    }
    let out = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("ps");
    let stat = String::from_utf8_lossy(&out.stdout).trim().to_string();
    !stat.starts_with('Z') && !stat.is_empty()
}

/// 宿主直接子进程数 = 存活 shell 数（fakesh exec 成 sleep）。
fn shell_count(pid: u32) -> usize {
    let out = Command::new("pgrep")
        .arg("-P")
        .arg(pid.to_string())
        .output()
        .expect("pgrep");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count()
}

fn window_count(pid: u32) -> usize {
    synth(&["wincount", &pid.to_string()])
        .parse::<usize>()
        .expect("wincount 输出数字")
}

/// 前台激活 + 全局 ⌘W（W=13）。带重试：CGEvent 首键偶发丢失（NOTES p2
/// 实录）；重试前先查状态，已生效就不再按（多按会多关面）。
fn cmd_w(pid: u32, expect_shells_before: usize, total: Duration) {
    let deadline = Instant::now() + total;
    loop {
        synth(&["activate", &pid.to_string()]);
        synth(&["key", "13", "1"]);
        // 事件派发 + performClose 级联（windowShouldClose → close_leaf）
        // 都在主线程一轮 runloop 内，2s 观察窗足够。
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(200));
            if shell_count(pid) < expect_shells_before {
                return;
            }
            if !alive(pid) {
                return; // 调用方按场景断言生死
            }
        }
        assert!(
            Instant::now() < deadline,
            "⌘W 未生效（shell 数停在 {expect_shells_before}）——事件投递失败或修复缺失"
        );
    }
}

fn wait_stable_shells(pid: u32, n: usize, total: Duration) {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if shell_count(pid) == n && alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!(
        "宿主未就绪：期望 {n} shell，实际 {}，alive={}",
        shell_count(pid),
        alive(pid)
    );
}

/// 等宿主退出（关窗/最后面关闭路径）。
fn wait_exit(pid: u32, total: Duration) {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if !alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("宿主未按预期退出（alive, shells={}）", shell_count(pid));
}

fn sandbox(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ninja_da_e2e_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 场景 1（缺陷本体）：3 分屏 ⌘W 逐面关，最后才关窗。
/// 修复前：第一下 ⌘W 整窗关（3 shell 全灭 + 进程退）。
#[test]
fn cmd_w_closes_one_split_surface_at_a_time() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip（NINJA_E2E 未设）");
        return;
    }
    let dir = sandbox("split");
    let _gui = GuiLock::acquire();
    let shell = fakesh(&dir);
    let mut reaper = Reaper(Vec::new());
    let pid = spawn_host(&mut reaper, &dir, "split,split", &shell);
    wait_stable_shells(pid, 3, Duration::from_secs(15));

    cmd_w(pid, 3, Duration::from_secs(10));
    assert!(alive(pid), "⌘W 多 pane 窗不应整窗关（D-A：只关当前面）");
    assert_eq!(shell_count(pid), 2, "其余 pane 的 shell 必须存活（各自 PTY 独立）");
    assert_eq!(window_count(pid), 1, "窗口必须在");

    cmd_w(pid, 2, Duration::from_secs(10));
    assert!(alive(pid) && shell_count(pid) == 1 && window_count(pid) == 1);

    // 最后一个面：⌘W 关窗 → 最后窗关闭 → 进程退出。
    cmd_w(pid, 1, Duration::from_secs(10));
    wait_exit(pid, Duration::from_secs(5));
}

/// 场景 2：原生 tab 语义钉死——3 标签 ⌘W 只关当前 tab。
#[test]
fn cmd_w_closes_only_current_tab() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip（NINJA_E2E 未设）");
        return;
    }
    let dir = sandbox("tabs");
    let _gui = GuiLock::acquire();
    let shell = fakesh(&dir);
    let mut reaper = Reaper(Vec::new());
    let pid = spawn_host(&mut reaper, &dir, "tab,tab", &shell);
    wait_stable_shells(pid, 3, Duration::from_secs(15));

    cmd_w(pid, 3, Duration::from_secs(10));
    assert!(alive(pid), "⌘W 关 tab 不应杀进程");
    assert_eq!(shell_count(pid), 2, "其余 tab 的 shell 必须存活");
    assert_eq!(window_count(pid), 1, "tab 组窗口必须在（还剩 2 个 tab）");
}

/// 场景 3：非 ⌘W 路径不回归——红绿灯整窗关（鼠标事件不触发 pane 级）。
#[test]
fn traffic_light_closes_whole_window() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip（NINJA_E2E 未设）");
        return;
    }
    let dir = sandbox("red");
    let _gui = GuiLock::acquire();
    let shell = fakesh(&dir);
    let mut reaper = Reaper(Vec::new());
    let pid = spawn_host(&mut reaper, &dir, "split,split", &shell);
    wait_stable_shells(pid, 3, Duration::from_secs(15));

    // AX 点击红绿灯（System Events；同会话 osascript 取证惯例）。
    let out = Command::new("osascript")
        .arg("-e")
        .arg(&format!(
            "tell application \"System Events\" to tell (first process whose unix id is {pid}) to click button 1 of window 1"
        ))
        .output()
        .expect("osascript click close button");
    assert!(
        out.status.success(),
        "红绿灯点击失败: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    wait_exit(pid, Duration::from_secs(5));
    assert_eq!(
        shell_count(pid),
        0,
        "红绿灯=整窗关：所有 pane 收尾，无 shell 残留"
    );
}

/// 场景 4：⇧⌘W（Close Pane 键）语义不变——关一个 pane，窗在。
#[test]
fn cmd_shift_w_still_closes_one_pane() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip（NINJA_E2E 未设）");
        return;
    }
    let dir = sandbox("csw");
    let _gui = GuiLock::acquire();
    let shell = fakesh(&dir);
    let mut reaper = Reaper(Vec::new());
    let pid = spawn_host(&mut reaper, &dir, "split", &shell);
    wait_stable_shells(pid, 2, Duration::from_secs(15));

    // ⌘⇧W 带 Shift → windowShouldClose 放行整窗关？不——⌘⇧W 是
    // Panes 菜单 ninjaClosePane:（菜单键派发，非 performClose:），
    // 直接关焦点 pane；这里钉它没被 ⌘W 修复波及。
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        synth(&["activate", &pid.to_string()]);
        let out = Command::new("osascript")
            .arg("-e")
            .arg("tell application \"System Events\" to keystroke \"w\" using {command down, shift down}")
            .output()
            .expect("osascript keystroke");
        assert!(out.status.success(), "⇧⌘W keystroke 失败");
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(200));
            if shell_count(pid) < 2 {
                assert!(alive(pid), "⇧⌘W 关 pane 不应杀进程");
                assert_eq!(shell_count(pid), 1, "⇧⌘W 只关一个 pane");
                assert_eq!(window_count(pid), 1, "窗口必须在");
                return;
            }
        }
        assert!(Instant::now() < deadline, "⇧⌘W 未生效");
    }
}
