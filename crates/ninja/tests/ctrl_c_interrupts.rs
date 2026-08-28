//! D-B **Ctrl+C 无效** 的 E2E 回归（`NINJA_E2E=1` 门控，真实 GUI 会话）。
//!
//! 修复前可复现缺陷：终端里跑交互程序（pi、bash、sleep）按 ^C 无反应。
//! 根因两层（keyDown 非 Cmd 分支走 `interpretKeyEvents`）：
//! 1. **未绑定的 ^c 被吞**：^c 不在 StandardKeyBinding.dict；IME 输入源
//!    （中文等）下 interpretKeyEvents 对 Ctrl+字母**零回调**（insertText:
//!    /doCommandBySelector: 都不进，探针取证）——即使走到 insertText:，
//!    路径上的 `keymap::sanitize_utf8` 也把 C0 剥掉（0x03 → None）。
//! 2. **绑定的 ^a 等被错译**：ASCII 输入源下 ^a → moveToBeginningOf…:
//!    → 方向键序列，终端要的 0x01 永远出不来。
//!
//! 修复（view.rs keyDown + keymap.rs）：Ctrl（非 Cmd）+ 有映射键码整类
//! 绕过 interpretKeyEvents，按 vt 键 + CTRL 修饰直通编码（^C→0x03、
//! ^A→0x01、^Space→0x00、Ctrl+方向键→CSI 修饰序列，编码器级回归见
//! term.rs `ctrl_letter_encodes_c0_byte`）。
//!
//! 两个场景（真实二进制 + 真实 CGEvent 定向投递 `keypidctrl`）：
//! 1. **^A/^C 以字面字节到 shell**：fakesh `stty -isig -icanon` 后
//!    `cat` 记录 stdin 原始字节——0x03/0x01 必须落盘（同时钉死 ^A=0x01，
//!    防键绑定错译回归；修复前：IME 源零字节 / ASCII 源 ^A 变方向键）。
//! 2. **真实 sleep 100 被 ^C 中断**：fakesh `trap ':' INT` 保身，子
//!    `sleep 100` 领 SIGINT（默认处置，exec 后 handler 复位）退出，
//!    marker 落盘证明 sleep 提前死（否则要跑满 100s）。ISIG 由 PTY
//!    默认 termios 提供——宿主只写 0x03，内核翻译信号，正是真终端语义。
//!
//! 运行前提同 cmdw_surface_close：可交互 GUI 会话（CGEventPostToPid
//! 免前台激活，p6 Esc 取证同款）。

use std::fs;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// 宿主收割器：drop（含 panic 展开）即 kill 整个进程组，fakesh 的
/// `sleep`/`cat` 不留孤儿（同 cmdw_surface_close 惯例）。
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

/// ^A/^C 记录 shell：关 ISIG/ICANON（让 0x03 过内核线规程**作为字节**
/// 而非 SIGINT 到达），cat 原样落盘 stdin。
fn record_fakesh(dir: &Path) -> PathBuf {
    let p = dir.join("fakesh_record.sh");
    fs::write(
        &p,
        "#!/bin/bash\nstty -isig -icanon\ncat > \"$NINJA_DB_RECORD\"\n",
    )
    .unwrap();
    make_executable(&p);
    p
}

/// ^C 中断取证 shell：自身装 INT trap 保命（SIG_IGN 会被 exec 继承，
/// 必须用 handler 让 sleep 拿回默认处置），sleep 100 被 SIGINT 打断后
/// marker 落盘、再挂住保 pane。
fn sleep_fakesh(dir: &Path) -> PathBuf {
    let p = dir.join("fakesh_sleep.sh");
    fs::write(
        &p,
        "#!/bin/bash\ntrap ':' INT\nsleep 100\ntouch \"$NINJA_DB_MARKER\"\nsleep 10000\n",
    )
    .unwrap();
    make_executable(&p);
    p
}

fn make_executable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = fs::metadata(p).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(p, perm).unwrap();
}

/// synth_input.swift 子命令（keypidctrl：定向投递 + Ctrl；剥
/// DEVELOPER_DIR，同 cmdw_surface_close.synth：CLT swift 与自身 SDK
/// 不配套）。
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
    // SAFETY: kill(pid,0) 只探测存在性；排除 zombie。
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

/// 宿主直接子进程数（这里单 pane = fakesh bash 数）。
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

/// 等 fakesh 就位：宿主有 1 个稳定子进程（bash 起来了，stty 已跑）。
fn wait_shell_up(pid: u32, deadline: Duration) {
    let end = Instant::now() + deadline;
    let mut stable = 0;
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(200));
        if shell_count(pid) == 1 {
            stable += 1;
            if stable >= 3 {
                return;
            }
        } else {
            stable = 0;
        }
    }
    panic!("fakesh 未稳定起来（shell_count={}）", shell_count(pid));
}

fn sandbox(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ninja_db_e2e_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// 场景 1（缺陷本体，字节级）：^A→0x01、^C→0x03 以字面字节进 shell stdin。
/// 修复前：IME 输入源下零字节（interpretKeyEvents 吞）；ASCII 源下
/// ^C 勉强可达但 ^A 被键绑定错译成方向键序列——本场景两个键一起钉。
#[test]
fn ctrl_keys_reach_shell_as_c0_bytes() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip（NINJA_E2E 未设）");
        return;
    }
    let dir = sandbox("bytes");
    let record = dir.join("record.bin");
    let shell = record_fakesh(&dir);

    // NINJA_DB_RECORD 由 fakesh 的 cat 落盘（SHELL 脚本经 shebang 执行，
    // 环境继承宿主进程）。
    let mut reaper = Reaper(Vec::new());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ninja"));
    cmd.env("SHELL", &shell)
        .env("NINJA_DB_RECORD", &record)
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(fs::File::create(dir.join("host_err.txt")).unwrap());
    let child = cmd.spawn().expect("spawn ninja");
    let pid = child.id();
    reaper.0.push(child);

    wait_shell_up(pid, Duration::from_secs(15));
    // stty 生效余量：fakesh 起稳后 stty -isig 已跑完（几百 ms 量级），
    // 再让渡 1.5s，避免 ^C 抢跑在 ISIG 关闭前变成 SIGINT 杀掉 bash。
    std::thread::sleep(Duration::from_millis(1500));

    // ^A（keyCode 0x00）与 ^C（keyCode 0x08）各一记。
    synth(&["keypidctrl", "0", &pid.to_string()]);
    std::thread::sleep(Duration::from_millis(300));
    synth(&["keypidctrl", "8", &pid.to_string()]);
    std::thread::sleep(Duration::from_millis(300));

    let end = Instant::now() + Duration::from_secs(8);
    loop {
        let got = fs::read(&record).unwrap_or_default();
        assert!(
            got.iter().all(|b| *b == 0x01 || *b == 0x03),
            "stdin 里只该有 ^A/^C，却收到 {:02x?}",
            got
        );
        if got.contains(&0x01) && got.contains(&0x03) {
            assert!(alive(pid), "宿主必须还在（^C 只是输入字节，不是关窗）");
            return;
        }
        if Instant::now() >= end {
            panic!(
                "shell 未收到 ^A=0x01/^C=0x03 字节（record={:02x?}）——\
                 Ctrl 组合被 interpretKeyEvents 吞/错译（D-B 回归）",
                got
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// 场景 2（缺陷本体，语义级）：真实 `sleep 100` 被 ^C（SIGINT）打断。
/// 修复前：^C 无人发字节 → 无 SIGINT → sleep 跑满 100s → marker 永不落盘。
#[test]
fn ctrl_c_interrupts_real_sleep() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip（NINJA_E2E 未设）");
        return;
    }
    let dir = sandbox("sigint");
    let marker = dir.join("interrupted.marker");
    let shell = sleep_fakesh(&dir);

    let mut reaper = Reaper(Vec::new());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ninja"));
    cmd.env("SHELL", &shell)
        .env("NINJA_DB_MARKER", &marker)
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(fs::File::create(dir.join("host_err.txt")).unwrap());
    let child = cmd.spawn().expect("spawn ninja");
    let pid = child.id();
    reaper.0.push(child);

    // 等 fakesh 与它的 sleep 100 都在（sleep 没起就投 ^C 会白打）。
    wait_shell_up(pid, Duration::from_secs(15));
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let out = Command::new("pgrep")
            .args(["-P", &pid.to_string()])
            .output()
            .expect("pgrep bash");
        let bash_pid = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .and_then(|l| l.trim().parse::<u32>().ok());
        if let Some(bp) = bash_pid {
            let so = Command::new("pgrep")
                .args(["-P", &bp.to_string()])
                .output()
                .expect("pgrep sleep");
            if !String::from_utf8_lossy(&so.stdout).trim().is_empty() {
                break;
            }
        }
        assert!(Instant::now() < end, "fakesh 的 sleep 100 未起来");
        std::thread::sleep(Duration::from_millis(200));
    }

    // ^C 一记 → PTY 线规程（默认 ISIG）→ SIGINT → 前台进程组
    //（bash 有 trap 保命，sleep 默认处置领死）→ marker 落盘。
    synth(&["keypidctrl", "8", &pid.to_string()]);

    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if marker.exists() {
            assert!(alive(pid), "宿主必须还在（fakesh 后续挂住保 pane）");
            assert_eq!(shell_count(pid), 1, "fakesh（bash）必须存活");
            return;
        }
        assert!(
            Instant::now() < end,
            "sleep 100 未被 ^C 打断（marker 未落盘）——0x03 没进 PTY（D-B 回归）"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}
