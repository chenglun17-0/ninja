//! p4 命中分派的 E2E 取证（`NINJA_E2E=1` 门控，同 idle_no_plugins 惯例）。
//!
//! 三个场景，全部走真实 ninja 二进制 + 真实 ADE socket：
//!
//! 1. **默认配置（无插件）**：`NINJA_P4_HIT` 触发 Cmd+点击路径 →
//!    `NINJA_OPEN_PROBE` 收到 file URL——无插件时点路径走系统默认
//!    （与普通终端一致），且不弹任何安装提示。
//! 2. **插件回 ignore**：最小脚本插件连 `NINJA_ADE_SOCK` 回
//!    `hit.ignore` → 插件收到**完整 hit 字段**（id/kind/text/row/col/
//!    pane/modifiers，对齐协议 golden 样例）；probe 仍收到 URL
//!    （ignore → 系统默认）。
//! 3. **插件回 claim**：回 `hit.claim` priority 7 → probe **不出现**
//!    （认领即止，系统默认不触发）。
//!
//! 插件是测试自己拉起的独立 python3 进程（宿主在 p4 永不拉插件进程
//! ——那是 p5 的事）；协议只经 socket 交换 JSON 字节（ADE 红线）。

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// 宿主/插件进程收割器：drop（含 panic 展开）即 kill+wait，不留孤儿。
struct Reaper(Vec<Child>);

impl Drop for Reaper {
    fn drop(&mut self) {
        for c in self.0.iter_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// 每个测试独有的临时目录。
fn sandbox(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ninja_hit_e2e_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// ADE socket 路径：**必须短**（macOS sun_path 上限 104 字节；带纳秒
/// 时间戳的沙盒目录名会超限，宿主会降级成「插件禁用」——实测踩过）。
/// tag 每场景唯一，同 pid 内不碰撞；宿主绑定时先清陈旧文件。
fn short_sock(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("nh_{tag}_{}.sock", std::process::id()))
}

/// fakesh：打一行带路径的输出（行首两格空白，点击列 2 落在 's' 上），
/// 然后阻塞到宿主退出（master 关闭 → EOF）。
fn fakesh(dir: &Path) -> PathBuf {
    let f = dir.join("fakesh.sh");
    std::fs::write(
        &f,
        concat!(
            "#!/bin/bash\n",
            "printf '  src/main.rs:42:13\\n'\n",
            "read _x\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
    f
}

/// 最小 ADE 插件（python3，进程外，只说 JSON）：
/// 连 socket（重试等宿主绑定）→ 收 hit 帧 → 把收到的消息逐行记进日志
/// → 按 mode 回 hit.ignore / hit.claim。
const PLUGIN_PY: &str = r#"
import json, socket, struct, sys, time

sock_path, mode, log_path = sys.argv[1], sys.argv[2], sys.argv[3]

s = None
for _ in range(200):  # 最多等 10s 宿主绑 socket
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.connect(sock_path)
        break
    except OSError:
        time.sleep(0.05)
if s is None:
    sys.exit(2)

log = open(log_path, "w")
log.write('{"type":"_plugin_connected"}\n')
log.flush()

def recv_exact(n):
    buf = b""
    while len(buf) < n:
        chunk = s.recv(n - len(buf))
        if not chunk:
            raise EOFError
        buf += chunk
    return buf

while True:
    try:
        (length,) = struct.unpack("<I", recv_exact(4))
        msg = json.loads(recv_exact(length))
    except EOFError:
        break
    log.write(json.dumps(msg, separators=(",", ":")) + "\n")
    log.flush()
    if msg.get("type") == "hit":
        rid = msg["id"]
        if mode == "ignore":
            reply = {"type": "hit.ignore", "v": 0, "id": rid}
        else:
            reply = {"type": "hit.claim", "v": 0, "id": rid, "priority": 7}
        data = json.dumps(reply).encode()
        s.sendall(struct.pack("<I", len(data)) + data)
"#;

/// 拉一个 ninja 实例：隔离配置 + fakesh + 取证钩子（登记进收割器）。
fn launch_ninja(
    reaper: &mut Reaper,
    config_text: &str,
    dir: &Path,
    sock: Option<&Path>,
    probe: &Path,
) {
    let cfg = dir.join(format!(
        "cfg_{}.toml",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&cfg, config_text).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ninja"));
    cmd.env("NINJA_CONFIG", &cfg)
        .env("SHELL", fakesh(dir))
        .env("NINJA_OPEN_PROBE", probe)
        // Cmd+点击路径的免 CGEvent 触发（首帧落定后延时执行）。
        .env("NINJA_P4_HIT", "2,0")
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(dir.join("host_err.txt")).unwrap());
    if let Some(s) = sock {
        cmd.env("NINJA_ADE_SOCK", s);
    }
    let child = cmd.spawn().expect("spawn ninja binary");
    reaper.0.push(child);
}

/// 拉最小插件进程（测试自己拉，宿主永不拉）。
fn launch_plugin(reaper: &mut Reaper, script: &Path, sock: &Path, mode: &str, log: &Path) {
    let child = Command::new("python3")
        .arg(script)
        .arg(sock)
        .arg(mode)
        .arg(log)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn python3 plugin");
    reaper.0.push(child);
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

/// 等待文件出现并稳定（两拍内容不变）。
fn wait_stable(p: &Path, total: Duration) -> String {
    let deadline = std::time::Instant::now() + total;
    let mut last = String::new();
    loop {
        let cur = read(p);
        if !cur.is_empty() && cur == last {
            return cur;
        }
        last = cur;
        if std::time::Instant::now() >= deadline {
            return last;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// 场景骨架：起插件（可选）→ 起宿主 → 等分发跑完 → 收割。
/// 返回（probe 内容, 插件日志内容）。
fn run_scenario(tag: &str, config_text: &str, plugin_mode: Option<&str>) -> (String, String) {
    let mut reaper = Reaper(Vec::new());
    let dir = sandbox(tag);
    let sock = short_sock(tag);
    let probe = dir.join("open_probe.txt");
    let plugin_log = dir.join("plugin_log.txt");
    let plugin_script = dir.join("plugin.py");
    std::fs::write(&plugin_script, PLUGIN_PY).unwrap();

    if let Some(mode) = plugin_mode {
        launch_plugin(&mut reaper, &plugin_script, &sock, mode, &plugin_log);
    }
    launch_ninja(&mut reaper, config_text, &dir, Some(&sock), &probe);

    // 前置诊断：插件必须先连上（宿主 socket 在启动时绑定，插件重试
    // 连接；连上后记 marker）。连不上就能区分「插件没连」和「没分发」。
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    if plugin_mode.is_some() {
        while !read(&plugin_log).contains("_plugin_connected") {
            if std::time::Instant::now() >= deadline {
                panic!("插件未连上 ADE socket（日志：{:?}）", read(&plugin_log));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    // 宿主 P4 钩子（3s 延时）+ 分发超时预算（500ms），留足余量。
    let probe_text = wait_stable(&probe, Duration::from_secs(8));
    std::thread::sleep(Duration::from_millis(500)); // 插件日志再冲一把盘
    let log_text = read(&plugin_log);
    let host_err = read(&dir.join("host_err.txt"));
    eprintln!("[{tag}] probe={probe_text:?} log={log_text:?} host_err={host_err:?}");
    // 启用了插件却绑不上 socket（如路径超 sun_path 上限）会静默降级成
    // 「无插件」——那不足以为「分发链路活着」取证，直接判失败。
    if plugin_mode.is_some() {
        assert!(
            !host_err.contains("绑定失败"),
            "宿主 ADE socket 绑定失败（降级无插件），host_err：{host_err:?}"
        );
    }

    (probe_text, log_text)
}

#[test]
fn e2e_no_plugins_click_path_goes_system_default() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip: 拉真实窗口进程，设 NINJA_E2E=1 启用");
        return;
    }
    // 默认配置（空文件 = 内置默认）：无插件、无 socket（空载门禁不破）。
    let (probe, _log) = run_scenario("noplugin", "", None);
    assert!(
        probe.contains("src/main.rs"),
        "无插件点路径应走系统默认打开（probe 收到 file URL），得到 {probe:?}"
    );
    assert!(
        probe.starts_with("file://"),
        "Path 命中应转成 file URL，得到 {probe:?}"
    );
}

#[test]
fn e2e_plugin_ignore_receives_full_hit_and_falls_back() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip: 拉真实窗口进程，设 NINJA_E2E=1 启用");
        return;
    }
    let (probe, log) = run_scenario(
        "ignore",
        "[plugins]\nenabled = [\"probe\"]\n",
        Some("ignore"),
    );
    // 插件收到完整 hit 字段（够预览认领，对齐协议样例）。
    for needle in [
        r#""type":"hit""#,
        r#""v":0"#,
        r#""id":1"#,
        r#""kind":"path""#,
        r#""text":"src/main.rs:42:13""#,
        r#""row":0"#,
        r#""col":2"#,
        r#""pane":1"#,
        r#""modifiers":["cmd"]"#,
    ] {
        assert!(
            log.contains(needle),
            "插件应收到完整 hit 字段 {needle}，日志：{log:?}"
        );
    }
    // 全 ignore → 系统默认打开照常发生。
    assert!(
        probe.contains("src/main.rs"),
        "全 ignore 应回退系统默认，probe：{probe:?}"
    );
}

#[test]
fn e2e_plugin_claim_suppresses_system_default() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip: 拉真实窗口进程，设 NINJA_E2E=1 启用");
        return;
    }
    let (probe, log) = run_scenario("claim", "[plugins]\nenabled = [\"probe\"]\n", Some("claim"));
    // 插件收到 hit 且认领。
    assert!(
        log.contains(r#""type":"hit""#) && log.contains(r#""text":"src/main.rs:42:13""#),
        "claim 场景插件应收到 hit：{log:?}"
    );
    // 认领即止：系统默认打开不触发（p4 到此为止，层是 p5）。
    assert!(
        probe.is_empty(),
        "有插件认领时不应走系统默认打开，probe：{probe:?}"
    );
}
