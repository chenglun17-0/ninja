//! p3 空载门禁的运行时取证：默认配置启动真实 ninja 二进制后，
//! (a) 本 pid 约定路径上没有 ADE socket 文件；
//! (b) 除 PTY shell 外没有任何子进程（插件进程零个）；
//! (c) 对照组：[plugins] enabled 非空时 socket 文件确实出现——
//!     证明 (a) 是门在起作用，不是探针瞎。
//!
//! 与 fast_shell_first_frame.rs 同风格：拉真实 AppKit 进程，需要能起
//! GUI 会话，默认跳过，`NINJA_E2E=1` 启用。锁屏下同样有效（不依赖
//! 窗口内容，只看文件系统 + ps）。
//! 宿主进程用 RAII guard 收割（panic 也杀，不留孤儿窗口进程）。

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// 宿主进程收割器：drop（含 panic 展开与测试结束）即 kill+wait。
struct Reaper(Vec<Child>);

impl Drop for Reaper {
    fn drop(&mut self) {
        for c in self.0.iter_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn fakesh() -> PathBuf {
    let dir = std::env::temp_dir();
    let f = dir.join(format!("ninja_idle_fakesh_{}.sh", std::process::id()));
    std::fs::write(
        &f,
        concat!(
            "#!/bin/bash\n",
            "printf 'idle probe\\n'\n",
            // 阻塞到宿主退出（master 关闭 → EOF），不留孤儿进程。
            "read _x\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
    f
}

/// 启动一个 ninja 实例（隔离配置），登记进收割器，返回其 pid。
fn launch(reaper: &mut Reaper, config_text: &str) -> u32 {
    let cfg = std::env::temp_dir().join(format!(
        "ninja_idle_cfg_{}_{}.toml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&cfg, config_text).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_ninja"))
        .env("NINJA_CONFIG", &cfg)
        .env("SHELL", fakesh())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ninja binary");
    let pid = child.id();
    reaper.0.push(child);
    pid
}

fn socket_path_for(pid: u32) -> PathBuf {
    std::env::temp_dir().join(format!("ninja-ade-{pid}.sock"))
}

/// pid 的直接子进程命令名列表（ps 取证）。
fn child_comms(pid: u32) -> Vec<String> {
    let out = Command::new("ps")
        .args(["-axo", "ppid=,comm="])
        .output()
        .expect("ps 取证");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let (ppid, comm) = l.split_once(' ')?;
            if ppid.trim() == pid.to_string() {
                Some(comm.trim().to_string())
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn idle_default_config_no_socket_no_plugin_processes() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip: 拉真实窗口进程，设 NINJA_E2E=1 启用");
        return;
    }

    let mut reaper = Reaper(Vec::new());

    // —— 对照组先行：启用插件 → socket 必须出现（证明门与探针都活着）。
    let on_pid = launch(&mut reaper, "[plugins]\nenabled = [\"preview\"]\n");
    std::thread::sleep(Duration::from_secs(3));
    let on_sock = socket_path_for(on_pid);
    assert!(
        on_sock.exists(),
        "对照组失败：enabled 非空应建 socket {on_sock:?}（探针或门坏了）"
    );

    // —— 空载组：默认配置（空文件 = 内置默认）。
    let off_pid = launch(&mut reaper, "");
    std::thread::sleep(Duration::from_secs(3));

    let off_sock = socket_path_for(off_pid);
    assert!(
        !off_sock.exists(),
        "空载门禁违规：默认配置创建了 ADE socket {off_sock:?}"
    );

    let kids = child_comms(off_pid);
    // PTY 里 exec 的是 $SHELL（脚本走 shebang → bash），comm 形如
    // "bash"/"zsh"。不变量：空载下除 shell 外无任何子进程
    //（插件进程/辅助进程零个）。
    assert!(
        kids.iter().all(|c| c.contains("sh")),
        "空载出现非 shell 子进程（插件进程？）：{kids:?}"
    );
    assert!(
        !kids.is_empty(),
        "探针失效：PTY shell 应作为子进程可见（否则 ps/启动有问题）"
    );

    // 对照组 socket 由测试收尾（宿主被 SIGKILL，drop 不会跑）。p6 起
    // 陈旧 socket 有正式清扫：下一个启用插件的宿主启动时扫死 pid
    // （plugins.rs sweep_stale_sockets；E2E 见 tests/off_is_light.rs
    // 场景 2），这里仍手工清以免依赖测试执行顺序。
    let _ = std::fs::remove_file(socket_path_for(on_pid));
    // reaper 在此（或 panic 展开）收割全部宿主进程。
}
