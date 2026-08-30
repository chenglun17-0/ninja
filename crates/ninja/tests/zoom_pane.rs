//! X3 **⌘⇧Enter 放大（toggle_zoom）** 的 E2E 回归（`NINJA_E2E=1` 门控，
//! 同 cmdw_surface_close 惯例）。
//!
//! Ghostty 语义钉死：多 pane 时 ⌘⇧Enter 放大焦点 pane 临时占满窗口，
//! 再按还原布局；无分屏时等价窗口 zoom（最大化非全屏）。放大期间其余
//! pane **暂隐藏但不销毁**——PTY/vt 全保留、数据继续喂 vt 不丢，布局
//! 还原即正确显示，滚动位置不动（隐藏面不 setFrame：网格尺寸冻结在
//! 分屏态）。
//!
//! 三个场景（真实二进制 + 真实 PTY shell）：
//! 1. **分屏 → zoom → 数据不丢 → 还原**（核心）：ticking fakesh 双 pane
//!    每 0.25s 打一行 `tick N` 且写 `tick.$$.txt`。放大焦点面后断言：
//!    两 shell 都活；隐藏面继续写文件；还原后布局/网格尺寸回原值且
//!    **隐藏面的 vt 内容包含放大期间收到的 tick**（数据没丢的铁证）。
//! 2. **无分屏 = 窗口 zoom**：单 pane 窗 ⌘⇧Enter → isZoomed=true、
//!    变宽、**非全屏**（styleMask 无 FullScreen）；再按回原尺寸。
//! 3. **真实 ⌘⇧Enter 键**（菜单键等价物）：osascript keystroke 过
//!    AppKit 菜单系统触发 ninjaToggleZoom:（与场景 1 的文件钩子同一条
//!    toggle_zoom 路径；证明默认绑定真的接上了菜单）。
//!
//! 驱动：`NINJA_ZOOM_FILE`（动作文件钩子：toggle/zoom/unzoom/dump）+
//! `NINJA_ZOOM_DUMP`（态快照 JSON），与 NINJA_P6/PANEL 钩子同惯例。

use std::fs::File;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// 前台互斥锁：场景 3 要激活宿主（⌘⇧Enter 走菜单系统必须前台），
/// 并行会互抢前台。flock 全局锁串行化（同 cmdw 惯例）。
struct GuiLock(File);

impl GuiLock {
    fn acquire() -> GuiLock {
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(std::env::temp_dir().join("ninja_x3_e2e_gui.lock"))
            .expect("open gui lock");
        // SAFETY: flock 阻塞等待锁；fd 属于我们。
        unsafe {
            libc::flock(f.as_raw_fd(), libc::LOCK_EX);
        }
        GuiLock(f)
    }
}

/// 宿主收割器：drop（含 panic 展开）即 kill 整个进程组（fakesh 不留孤儿）。
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
        "ninja_x3_e2e_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// ticking fakesh：每 0.25s 打一行 `tick N` 且把 N 写进
/// `$TICK_DIR/tick.$$.txt`（宿主 env 透传；$$ 区分两个 pane 的 shell）。
/// 阻塞面（场景 2/3 用）：打一行提示符后挂起。
fn fakesh(dir: &Path, ticking: bool) -> PathBuf {
    let p = dir.join("fakesh.sh");
    let script = if ticking {
        concat!(
            "#!/bin/bash\n",
            "i=0\n",
            "while true; do\n",
            "  i=$((i+1))\n",
            "  printf 'tick %d\\n' \"$i\"\n",
            "  echo \"$i\" > \"$TICK_DIR/tick.$$.txt\"\n",
            "  sleep 0.25\n",
            "done\n",
        )
    } else {
        concat!(
            "#!/bin/bash\n",
            "printf 'idle%%\\n'\n",
            "sleep 10000\n",
        )
    };
    std::fs::write(&p, script).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// 拉起宿主（process_group(0) 供收割；NINJA_ZOOM_FILE/DUMP 是 X3 钩子）。
fn spawn_host(
    reaper: &mut Reaper,
    dir: &Path,
    shell: &Path,
    tick_dir: Option<&Path>,
) -> (u32, PathBuf, PathBuf) {
    let cmd_file = dir.join("zoom_cmd.txt");
    let dump_file = dir.join("zoom_dump.json");
    let _ = std::fs::remove_file(&dump_file);
    let mut c = Command::new(env!("CARGO_BIN_EXE_ninja"));
    c.env("SHELL", shell)
        .env("NINJA_ZOOM_FILE", &cmd_file)
        .env("NINJA_ZOOM_DUMP", &dump_file)
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(dir.join("host_err.txt")).unwrap());
    if let Some(t) = tick_dir {
        c.env("TICK_DIR", t);
    }
    let child = c.spawn().expect("spawn ninja");
    let pid = child.id();
    reaper.0.push(child);
    (pid, cmd_file, dump_file)
}

fn alive(pid: u32) -> bool {
    // SAFETY: kill(pid,0) 不发信号，只探测存在性。
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

/// 宿主直接子进程数 = 存活 shell 数。
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

fn wait_shells(pid: u32, n: usize, total: Duration) {
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

/// 写一条钩子动作并确保被宿主读到（拍间隔 0.2s；后续动作要等这一拍，
/// 否则文件被覆盖而丢失）。
fn write_cmd_and_wait_tick(cmd_file: &Path, action: &str) {
    std::fs::write(cmd_file, action).unwrap();
    std::thread::sleep(Duration::from_millis(450));
}

// ---------------------------------------------------------------------------
// zoom 态快照解析（{"zoomed":B,"zoomed_pane":N,"leaves":[{...}],"window":{...}}）
// ---------------------------------------------------------------------------

struct Leaf {
    pane: u32,
    hidden: bool,
    w: f64,
    cols: u64,
    last: String,
}

/// 取顶层标量（zoomed / zoomed_pane）。顶层键不与叶子键混（叶子在
/// "leaves":[...] 内），按 `"key":` 到下一个 `,`/`}` 取值。
fn top_scalar(json: &str, key: &str) -> String {
    let pat = format!("\"{key}\":");
    let i = json.find(&pat).expect("key in json") + pat.len();
    let rest = &json[i..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    rest[..end].trim().to_string()
}

fn leaves(json: &str) -> Vec<Leaf> {
    let mut out = Vec::new();
    let mut rest = json;
    while let Some(i) = rest.find("{\"pane\":") {
        let obj = &rest[i..];
        let end = obj.find('}').expect("leaf obj end") + 1;
        let obj = &obj[..end];
        let get = |k: &str| -> String {
            let pat = format!("\"{k}\":");
            let j = obj.find(&pat).expect("leaf key") + pat.len();
            let tail = &obj[j..];
            let e = tail.find([',', '}']).unwrap_or(tail.len());
            tail[..e].trim().to_string()
        };
        let last_raw = get("last");
        out.push(Leaf {
            pane: get("pane").parse().expect("pane num"),
            hidden: get("hidden") == "true",
            w: get("w").parse().expect("w num"),
            cols: get("cols").parse().expect("cols num"),
            last: last_raw.trim_matches('"').to_string(),
        });
        rest = &rest[i + end..];
    }
    out
}

fn window_zoomed(json: &str) -> Option<bool> {
    let win = json.split("\"window\":").nth(1)?;
    let pat = "\"zoomed\":";
    let i = win.find(pat)? + pat.len();
    let tail = &win[i..];
    let e = tail.find([',', '}']).unwrap_or(tail.len());
    Some(tail[..e].trim() == "true")
}

fn window_fullscreen(json: &str) -> Option<bool> {
    let win = json.split("\"window\":").nth(1)?;
    let pat = "\"fullscreen\":";
    let i = win.find(pat)? + pat.len();
    let tail = &win[i..];
    let e = tail.find([',', '}']).unwrap_or(tail.len());
    Some(tail[..e].trim() == "true")
}

fn window_w(json: &str) -> Option<f64> {
    let win = json.split("\"window\":").nth(1)?;
    let pat = "\"w\":";
    let i = win.find(pat)? + pat.len();
    let tail = &win[i..];
    let e = tail.find([',', '}']).unwrap_or(tail.len());
    tail[..e].trim().parse().ok()
}

/// tick 行 `"tick 42"` → 42（vt 内容取证）。
fn tick_num(last: &str) -> Option<u64> {
    last.trim()
        .strip_prefix("tick ")
        .and_then(|n| n.trim().parse().ok())
}

/// 当前两个 tick 文件里的最小 tick（两 pane 同节奏，min ≈ 慢的那个）。
fn min_tick_file(tick_dir: &Path) -> Option<u64> {
    let mut mins = Vec::new();
    for e in std::fs::read_dir(tick_dir).ok()? {
        let p = e.ok()?.path();
        let n = std::fs::read_to_string(&p)
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()?;
        mins.push(n);
    }
    mins.into_iter().min()
}

/// X3 钩子驱动：写动作（递增 dump 序号绕钩子去抖）→ 等快照落盘。
fn dump(nth: &mut u64, cmd_file: &Path, dump_file: &Path) -> String {
    *nth += 1;
    let _ = std::fs::remove_file(dump_file);
    std::fs::write(cmd_file, format!("dump{nth}")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(s) = std::fs::read_to_string(dump_file) {
            return s;
        }
        if Instant::now() > deadline {
            panic!("zoom 快照未落盘 {dump_file:?}（钩子未跑？NINJA_ZOOM_FILE 设置对了吗）");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 写一次 toggle，然后轮询快照直到 `pred` 成立（返回成立时的快照）。
fn toggle_and_wait(
    nth: &mut u64,
    cmd_file: &Path,
    dump_file: &Path,
    total: Duration,
    pred: impl Fn(&str) -> bool,
) -> String {
    std::fs::write(cmd_file, "toggle").unwrap();
    // 钩子拍间隔 0.2s：先等一拍确保 "toggle" 被读到，再开始轮询 dump
    //（否则 dump 序列会覆盖掉还没被读的 toggle）。
    std::thread::sleep(Duration::from_millis(450));
    let deadline = Instant::now() + total;
    loop {
        let snap = dump(nth, cmd_file, dump_file);
        if pred(&snap) {
            return snap;
        }
        assert!(
            Instant::now() < deadline,
            "toggle 后态未达预期，最后快照：{snap}"
        );
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// synth_input.swift activate（场景 3 前台激活）。
fn synth_activate(pid: u32) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/verify/synth_input.swift");
    let out = Command::new("swift")
        .arg(&script)
        .args(["activate", &pid.to_string()])
        .env_remove("DEVELOPER_DIR")
        .output()
        .expect("swift synth_input");
    assert!(
        out.status.success(),
        "synth activate 失败: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// 场景 1（核心）：分屏 → zoom → 隐藏面数据不丢 → 还原 → 布局与内容恢复。
#[test]
fn zoom_split_keeps_hidden_pane_data_and_restores_layout() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip（NINJA_E2E 未设）");
        return;
    }
    let dir = sandbox("split");
    let _gui = GuiLock::acquire(); // 宿主串行（后台宿主 keyWindow 会空，
    // 同 cmdw 惯例；selftest 的 split 步也依赖 key window）
    let tick_dir = dir.join("ticks");
    std::fs::create_dir_all(&tick_dir).unwrap();
    let shell = fakesh(&dir, true);
    let mut reaper = Reaper(Vec::new());
    let (pid, cmd_file, dump_file) =
        spawn_host(&mut reaper, &dir, &shell, Some(&tick_dir));
    wait_shells(pid, 1, Duration::from_secs(15));
    // 双 pane 布置走钩子 split（不依赖 selftest 的 key window）。
    write_cmd_and_wait_tick(&cmd_file, "split");
    wait_shells(pid, 2, Duration::from_secs(10));
    let mut nth = 0u64;

    // 等双面 tick 流稳定（两份 tick 文件都在写且 ≥3）。
    {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(m) = min_tick_file(&tick_dir)
                && m >= 3
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    // 基线：未放大，两叶子都可见，记录分屏几何。
    let base = dump(&mut nth, &cmd_file, &dump_file);
    assert_eq!(top_scalar(&base, "zoomed"), "false", "基线不应处于放大态");
    let base_leaves = leaves(&base);
    assert_eq!(base_leaves.len(), 2, "分屏后应有两个叶子：{base}");
    // pane id 发号确定性：首窗面 = 1，split 新面 = 2（后续断言依赖）。
    assert_eq!((base_leaves[0].pane, base_leaves[1].pane), (1, 2), "{base}");
    assert!(base_leaves.iter().all(|l| !l.hidden));
    let (w1, w2) = (base_leaves[0].w, base_leaves[1].w);
    let (cols1, cols2) = (base_leaves[0].cols, base_leaves[1].cols);
    assert!(w1 > 50.0 && w2 > 50.0, "分屏面宽度异常：{base}");
    assert_eq!(
        window_zoomed(&base),
        Some(false),
        "多 pane 的 toggle 不应碰窗口 zoom"
    );
    let t_before = min_tick_file(&tick_dir).expect("tick 文件就绪");

    // zoom（焦点面 = split 夺焦的新 pane，pane id 2）。
    let zoomed = toggle_and_wait(
        &mut nth,
        &cmd_file,
        &dump_file,
        Duration::from_secs(5),
        |s| top_scalar(s, "zoomed") == "true",
    );
    assert_eq!(
        top_scalar(&zoomed, "zoomed_pane"),
        "2",
        "应放大焦点面（split 夺焦的新 pane）：{zoomed}"
    );
    let z_leaves = leaves(&zoomed);
    assert!(z_leaves[0].hidden, "非放大面应隐藏（不销毁）：{zoomed}");
    assert!(!z_leaves[1].hidden, "放大面应可见：{zoomed}");
    assert!(
        z_leaves[1].w >= w1 + w2 + 1.0,
        "放大面应占满整窗（期望 ≥ {:+.1}，得到 {:.1}）：{zoomed}",
        w1 + w2 + 5.0,
        z_leaves[1].w
    );
    assert!(
        z_leaves[1].cols > z_leaves[0].cols + 10,
        "放大面网格应变宽（全窗列数）：{zoomed}"
    );
    assert_eq!(
        window_zoomed(&zoomed),
        Some(false),
        "pane zoom 与窗口 zoom 是两回事：{zoomed}"
    );

    // 放大期间：两 shell 都活、隐藏面的 shell 继续写文件（数据继续流）。
    std::thread::sleep(Duration::from_millis(1600));
    assert!(alive(pid), "放大不应杀宿主");
    assert_eq!(shell_count(pid), 2, "两 pane 的 shell 必须都活着（隐藏 ≠ 销毁）");
    let t_hidden = min_tick_file(&tick_dir).expect("tick 文件仍在写");
    assert!(
        t_hidden >= t_before + 4,
        "隐藏面（与可见面）的 shell 应持续写文件：before={t_before} hidden={t_hidden}"
    );

    // 还原：布局/网格尺寸回基线，隐藏面 vt 里有放大期间收到的 tick。
    let restored = toggle_and_wait(
        &mut nth,
        &cmd_file,
        &dump_file,
        Duration::from_secs(5),
        |s| top_scalar(s, "zoomed") == "false",
    );
    assert_eq!(
        top_scalar(&restored, "zoomed_pane"),
        "null",
        "还原后无放大面：{restored}"
    );
    let r_leaves = leaves(&restored);
    assert_eq!(r_leaves.len(), 2, "还原后仍是两个 pane（没销毁）：{restored}");
    assert!(r_leaves.iter().all(|l| !l.hidden), "还原后全部可见：{restored}");
    assert!(
        (r_leaves[0].w - w1).abs() < 2.0 && (r_leaves[1].w - w2).abs() < 2.0,
        "还原后面宽应回基线（{w1:.1}/{w2:.1}）：{restored}"
    );
    assert!(
        r_leaves[0].cols.abs_diff(cols1) <= 2 && r_leaves[1].cols.abs_diff(cols2) <= 2,
        "还原后网格列数应回基线（{cols1}/{cols2}）：{restored}"
    );
    // 数据没丢的铁证：隐藏面 vt 的最后 tick ≥ 放大期间已写的 tick。
    let hidden_last = tick_num(&r_leaves[0].last)
        .or_else(|| tick_num(&r_leaves.iter().map(|l| l.last.clone()).collect::<Vec<_>>().join(" ")));
    let Some(n) = hidden_last else {
        panic!("隐藏面（pane 1）应有 tick 内容：{restored}");
    };
    assert!(
        n + 2 >= t_hidden,
        "隐藏面 vt 丢了放大期间的数据：vt tick={n}，shell 已写 tick={t_hidden}"
    );
    assert!(
        r_leaves.iter().any(|l| tick_num(&l.last).is_some()),
        "两面内容都应在：{restored}"
    );

    // 收尾态：shell 仍 2 个（还原不杀 shell）。
    assert_eq!(shell_count(pid), 2, "还原后两 shell 仍活");
}

/// 场景 2：无分屏 = 窗口 zoom（最大化**非全屏**），再按还原。
#[test]
fn no_split_toggles_window_zoom_not_fullscreen() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip（NINJA_E2E 未设）");
        return;
    }
    let dir = sandbox("winzoom");
    let _gui = GuiLock::acquire(); // 宿主串行（同上）
    let shell = fakesh(&dir, false);
    let mut reaper = Reaper(Vec::new());
    let (pid, cmd_file, dump_file) = spawn_host(&mut reaper, &dir, &shell, None);
    wait_shells(pid, 1, Duration::from_secs(15));
    let mut nth = 0u64;

    let base = dump(&mut nth, &cmd_file, &dump_file);
    assert_eq!(top_scalar(&base, "zoomed"), "false");
    assert_eq!(leaves(&base).len(), 1);
    assert_eq!(window_zoomed(&base), Some(false));
    let w0 = window_w(&base).expect("window w");

    // 单面 ⌘⇧Enter → 窗口 zoom。
    let zoomed = toggle_and_wait(
        &mut nth,
        &cmd_file,
        &dump_file,
        Duration::from_secs(5),
        |s| window_zoomed(s) == Some(true),
    );
    assert_eq!(
        window_fullscreen(&zoomed),
        Some(false),
        "窗口 zoom 是最大化，不是全屏：{zoomed}"
    );
    let wz = window_w(&zoomed).expect("zoomed w");
    assert!(
        wz > w0 * 1.25,
        "zoom 后窗口应变宽（{w0:.0} → {wz:.0}）：{zoomed}"
    );
    assert_eq!(
        top_scalar(&zoomed, "zoomed"),
        "false",
        "单面窗不进 pane 放大态：{zoomed}"
    );
    assert!(alive(pid), "窗口 zoom 不杀进程");

    // 再按：回原尺寸。
    let back = toggle_and_wait(
        &mut nth,
        &cmd_file,
        &dump_file,
        Duration::from_secs(5),
        |s| window_zoomed(s) == Some(false),
    );
    let wb = window_w(&back).expect("restored w");
    assert!(
        (wb - w0).abs() < 4.0,
        "再按应回原窗口宽（{w0:.0} → {wb:.0}）：{back}"
    );
    assert!(alive(pid) && shell_count(pid) == 1, "shell 不受影响");
}

/// 场景 3：真实 ⌘⇧Enter（osascript keystroke 过菜单键等价物）触发同一
/// toggle_zoom 路径——钉默认绑定 cmd+shift+enter 真的接上了菜单。
#[test]
fn real_cmd_shift_enter_binds_to_zoom() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip（NINJA_E2E 未设）");
        return;
    }
    let dir = sandbox("key");
    let _gui = GuiLock::acquire();
    let shell = fakesh(&dir, false);
    let mut reaper = Reaper(Vec::new());
    let (pid, cmd_file, dump_file) = spawn_host(&mut reaper, &dir, &shell, None);
    wait_shells(pid, 1, Duration::from_secs(15));
    write_cmd_and_wait_tick(&cmd_file, "split");
    wait_shells(pid, 2, Duration::from_secs(10));
    let mut nth = 0u64;

    let mut keystroke = |expect_zoomed: bool| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            synth_activate(pid);
            let out = Command::new("osascript")
                .arg("-e")
                .arg(
                    "tell application \"System Events\" to keystroke return using {command down, shift down}",
                )
                .output()
                .expect("osascript keystroke");
            assert!(
                out.status.success(),
                "⌘⇧Enter keystroke 失败: {:?}",
                String::from_utf8_lossy(&out.stderr)
            );
            for _ in 0..10 {
                std::thread::sleep(Duration::from_millis(200));
                let snap = dump(&mut nth, &cmd_file, &dump_file);
                let z = top_scalar(&snap, "zoomed") == "true";
                if z == expect_zoomed {
                    return snap;
                }
            }
            assert!(
                Instant::now() < deadline,
                "真实 ⌘⇧Enter 未触发 toggle_zoom（菜单键等价物没接上？）"
            );
        }
    };

    let zoomed = keystroke(true);
    assert_eq!(
        top_scalar(&zoomed, "zoomed_pane"),
        "2",
        "⌘⇧Enter 应放大焦点 pane：{zoomed}"
    );
    assert_eq!(shell_count(pid), 2, "两 shell 都活");

    let restored = keystroke(false);
    assert!(leaves(&restored).iter().all(|l| !l.hidden), "再按还原：{restored}");
    assert_eq!(shell_count(pid), 2, "还原后两 shell 仍活");
}
