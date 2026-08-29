//! p5 层+文本预览的 E2E 取证（`NINJA_E2E=1` 门控，同 idle_no_plugins
//! 惯例）：**第一个插件门禁**——只通过公开协议（ADE socket JSON 帧 +
//! 宿主分配的 IOSurface）完成「Cmd+点击路径 → 终端内看文本 → Esc
//! 关层」。
//!
//! 场景：
//! 1. **绝对路径冷启动**：启用 preview、不预拉进程（PRODUCT：启用≠
//!    常驻）→ `NINJA_P4_HIT` 触发点击路径 → 宿主首次分发时才拉起
//!    ninja-preview → claim（open_probe 必须为空：系统默认不触发）→
//!    `layer.open/ready/present` → 层内容探针（`NINJA_LAYER_PROBE`：
//!    渲染器把层纹理读回落盘 PPM）出现且有文本墨迹 → 真实 Esc
//!    （tools/verify/synth_input.swift 合成 CGEvent）→ 层关（探针
//!    文件被删除）。
//! 2. **相对路径 + OSC-7 cwd**：fakesh 先报 OSC 7 pwd 再打相对路径
//!    `rel.txt:2` → 插件靠 hit.cwd 解析并认领 → 层照常出现（钉死
//!    p5 的 Hit.cwd 协议修订）。
//!
//! 运行前提：先 `cargo build -p ninja-preview`（本测试从
//! CARGO_BIN_EXE_ninja 同目录解析插件二进制；宿主配置的也是它）。

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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
        "ninja_p5_e2e_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 点击目标目录：**路径必须短**——终端只有 80 列，fakesh 打印的长路径
/// 会在网格上折行，宿主只能认出截断后的前段（文件不存在 → 插件合理
/// ignore）。/tmp 下短目录（~20 字符）保证整行装得下。
fn short_target_dir(tag: &str) -> PathBuf {
    let dir = PathBuf::from("/tmp").join(format!("np5e2e_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// ADE socket 路径必须短（macOS sun_path 上限 104 字节）。
fn short_sock(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("np5_{tag}_{}.sock", std::process::id()))
}

/// ninja-preview 二进制：宿主 bin 同目录（`cargo build -p ninja-preview`
/// 后存在）。缺失 = E2E 前提不满足，直接失败（不静默跳过：门禁）。
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

/// fakesh：可选 OSC-7 pwd + 一行可点路径，然后阻塞等宿主退出。
fn fakesh(dir: &Path, osc7_pwd: Option<&Path>, line: &str) -> PathBuf {
    let f = dir.join("fakesh.sh");
    let mut script = String::from("#!/bin/bash\n");
    if let Some(pwd) = osc7_pwd {
        // OSC 7：file://host/pwd（宿主 vt 的 pwd() 从这里来，返回完整
        // URI；宿主侧解码成路径——见 open::osc7_to_path）。注：Rust
        // format! 里 % 无特殊义，直接写 %s。
        script.push_str(&format!(
            "printf '\\033]7;file://%s%s\\033\\\\' \"$(hostname -s)\" {pwd:?}\n",
        ));
    }
    script.push_str(&format!("printf '  {line}\\n'\n"));
    script.push_str("read _x\n");
    std::fs::write(&f, script).unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
    f
}

/// PPM（P6）解析：统计「亮」像素数（文本墨迹；层背景是 One Dark Pro
/// 底色 #282c34，暗色不干扰计数）。
fn ppm_bright_ink(data: &[u8]) -> Option<usize> {
    // 头：P6\n<w> <h>\n255\n（本仓渲染器写出的形态）。逐段找头并
    // 定位像素起始偏移（避免不稳定 API）。
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
    let dims = lines[1];
    if dims.iter().filter(|&&b| b == b' ').count() != 1 {
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
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "ppm") {
                    return Some(p);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// 等文件消失（层关：close 摘探针文件）。
fn wait_gone(p: &Path, total: Duration) -> bool {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if !p.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// 场景骨架：起宿主（enabled preview + 路径表）→ 触发 NINJA_P4_HIT →
/// 等层探针出现。`relative`：true = OSC-7 pwd + 相对路径 rel.txt:2；
/// false = 绝对路径 target.rs:7:1。返回（探针文件, open_probe, 探针目录,
/// host_err 路径）。
fn run_scenario(
    reaper: &mut Reaper,
    tag: &str,
    relative: bool,
) -> (PathBuf, PathBuf, PathBuf, PathBuf, u32) {
    let dir = sandbox(tag);
    let probe_dir = dir.join("layers");
    std::fs::create_dir_all(&probe_dir).unwrap();
    let open_probe = dir.join("open_probe.txt");
    let sock = short_sock(tag);

    // 预览目标：多行真实内容（层要有东西可画），放短目录（免 80 列
    // 折行——见 short_target_dir 文档）。
    let target_dir = short_target_dir(tag);
    let target = target_dir.join(if relative { "src/rel.txt" } else { "target.rs" });
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut content = String::new();
    for i in 1..=60 {
        content.push_str(&format!("// line {i} MARKER-P5-E2E preview content\n"));
    }
    std::fs::write(&target, content).unwrap();

    // 点击行：绝对带 :行:列；相对靠 OSC-7 pwd（pwd 本身可以长：折行的
    // 是命中文本，不是 cwd）。相对路径必须含 `/`（link.rs 的 Path
    // 分类：含斜杠且末段带点的相对路径才认）。
    let (osc7, line) = if relative {
        (Some(target_dir.as_path()), "src/rel.txt:2".to_string())
    } else {
        (None, format!("{}:7:1", target.display()))
    };
    let shell = fakesh(&dir, osc7, &line);

    let cfg = dir.join("cfg.toml");
    std::fs::write(
        &cfg,
        format!(
            "[plugins]\nenabled = [\"preview\"]\n\n[plugins.paths]\npreview = {:?}\n",
            preview_bin()
        ),
    )
    .unwrap();

    let host = Command::new(env!("CARGO_BIN_EXE_ninja"))
        .env("NINJA_CONFIG", &cfg)
        .env("SHELL", &shell)
        .env("NINJA_ADE_SOCK", &sock)
        .env("NINJA_LAYER_PROBE", &probe_dir)
        .env("NINJA_OPEN_PROBE", &open_probe)
        .env("NINJA_P4_HIT", "2,0") // 点在第 0 行第 2 列（落在路径首字符上）
        .env("NINJA_ADE_DEBUG", "1") // host_err 里带分发/层握手轨迹（取证）
        .process_group(0) // 收割时 killpg 连插件一起收（reaper）
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(dir.join("host_err.txt")).unwrap())
        .spawn()
        .expect("spawn ninja");
    let host_pid = host.id();
    reaper.0.push(host);

    let ppm = wait_ppm(&probe_dir, Duration::from_secs(20)).expect(
        "层探针未出现：插件未认领/未 present/渲染未合成（看 host_err.txt 与 open_probe）",
    );
    (ppm, open_probe, probe_dir, dir.join("host_err.txt"), host_pid)
}

/// 合成 Esc 定向投递到宿主进程（tools/verify/synth_input.swift 的
/// keypid 子命令；Esc=0x35=53；CGEventPostToPid 免前台焦点抖动）。
fn post_esc(host_pid: u32) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/verify/synth_input.swift");
    let out = Command::new("swift")
        .arg(&script)
        .args(["keypid", "53", &host_pid.to_string()])
        // 仓内 .cargo/config.tomn 强制 DEVELOPER_DIR=CommandLineTools（zig
        // 链接需要），但 CLT 的 swift 6.0.2 与自身 SDK 不配套编译会报
        // 错；合成输入要用 xcode-select 选中的 Xcode 工具链——去掉覆盖。
        .env_remove("DEVELOPER_DIR")
        .output()
        .expect("swift synth_input");
    eprintln!(
        "esc post: {} out={:?} err={:?}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "Esc 合成失败（见上行 swift 输出）");
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

#[test]
fn e2e_absolute_path_cold_spawn_layer_present_and_esc_close() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip: 拉真实窗口进程，设 NINJA_E2E=1 启用");
        return;
    }
    let mut reaper = Reaper(Vec::new());
    let (ppm, open_probe, probe_dir, host_err_path, host_pid) =
        run_scenario(&mut reaper, "abs", false);

    // 层内容有文本墨迹（不是纯背景色块）。
    let data = std::fs::read(&ppm).expect("读层探针 PPM");
    let ink = ppm_bright_ink(&data).expect("解析层探针 PPM");
    assert!(
        ink > 2000,
        "层内文本墨迹不足（{ink}px 亮像素）：层可能只画了背景"
    );

    // claim 生效：系统默认打开不触发（probe 为空 = 没有走 NSWorkspace）。
    assert!(
        read(&open_probe).is_empty(),
        "插件认领后不应走系统默认打开：{}",
        read(&open_probe)
    );

    // 冷启动取证：宿主在首次分发时才拉起插件（stderr 有拉起日志）。
    let host_err = read(&host_err_path);
    assert!(
        host_err.contains("已拉起插件"),
        "应有首次分发拉起插件的日志：{host_err}"
    );

    // Esc 关层（定向 CGEvent → keyDown → 宿主兜底关层 + 删探针文件）。
    post_esc(host_pid);
    assert!(
        wait_gone(&ppm, Duration::from_secs(10)),
        "Esc 后层探针未被删除（关层路径未跑或未摘层）"
    );
    // 焦点回终端：open_probe 仍空（后续键不再进插件），且宿主仍活着
    //（reaper 里未退出——进程存在性由 wait_gone 前的窗口仍开着承载）。
    let _ = probe_dir;
}

#[test]
fn e2e_relative_path_resolved_via_osc7_cwd() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip: 拉真实窗口进程，设 NINJA_E2E=1 启用");
        return;
    }
    let mut reaper = Reaper(Vec::new());
    // OSC-7 pwd = 短目录（src/rel.txt 在里面）；点相对路径 src/rel.txt:2。
    let (ppm, open_probe, _probe_dir, host_err_path, _host_pid) =
        run_scenario(&mut reaper, "rel", true);

    let data = std::fs::read(&ppm).expect("读层探针 PPM");
    let ink = ppm_bright_ink(&data).expect("解析层探针 PPM");
    assert!(ink > 1000, "相对路径层墨迹不足（{ink}px）");
    // claim 生效（无系统默认打开）+ 拉起日志。
    assert!(read(&open_probe).is_empty(), "相对路径也应被认领");
    assert!(
        read(&host_err_path).contains("已拉起插件"),
        "应有拉起日志：{}",
        read(&host_err_path)
    );
}
