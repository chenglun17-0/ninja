//! D1 回归：快 shell 首帧必须进 vt（启动期唤醒注册竞态）。
//!
//! 缺陷（p1 第三轮验证反馈，6/6 复现）：`TerminalView::new` 里
//! `Pty::spawn` 先于 `set_wake_hook` 执行，中间隔着 `Renderer::new`
//! 的运行时着色器编译——快 shell（立即 printf 的脚本）的首批 PTY
//! 字节在 WAKE_HOOK==0 时入队 rx，读线程 wake_main 空转丢信号 →
//! 字节滞留队列、vt 收不到文本 → 首屏全黑（atlas 只有 1px 白块）。
//! 交互式 shell 启动慢于注册，碰巧赢过竞态，掩盖缺陷。
//!
//! 本测试拉起真实 ninja 二进制：SHELL=立即输出脚本 + NINJA_DUMP_ATLAS
//! 读回，空闲 4s 后 atlas 非零像素必须远超白块——与验证员定罪用的
//! 同一探针（display 无关，锁屏下也有效）。
//! 需要能拿 Metal drawable 的 GUI 会话：默认跳过，`NINJA_E2E=1` 启用。

use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::Duration;

#[test]
fn fast_shell_first_frame_reaches_atlas() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip: 需要 GUI 会话（拉真实窗口），设 NINJA_E2E=1 启用");
        return;
    }

    let dir = std::env::temp_dir();
    let fakesh = dir.join("ninja_e2e_fakesh.sh");
    std::fs::write(
        &fakesh,
        concat!(
            "#!/bin/bash\n",
            "printf 'first frame probe abc XYZ\\n'\n",
            "printf 'line2 0123 你好\\n'\n",
            "printf 'idle%% '\n",
            // 阻塞到 app 退出（master 关闭 → EOF）：即使测试 SIGKILL 宿主，
            // 脚本也不会留孤儿进程。
            "read _x\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fakesh, std::fs::Permissions::from_mode(0o755)).unwrap();

    let pgm = dir.join("ninja_e2e_atlas.pgm");
    let _ = std::fs::remove_file(&pgm);

    let mut child = Command::new(env!("CARGO_BIN_EXE_ninja"))
        .env("SHELL", &fakesh)
        .env("NINJA_DUMP_ATLAS", &pgm)
        .spawn()
        .expect("spawn ninja binary");
    std::thread::sleep(Duration::from_secs(4));
    let _ = child.kill();
    let _ = child.wait();

    let data = std::fs::read(&pgm).expect("atlas dump 应已写出（首帧至少画过一次）");
    let idx = data
        .windows(4)
        .position(|w| w == b"255\n")
        .expect("PGM header")
        + 4;
    let ink = data[idx..].iter().filter(|&&v| v > 0).count();
    // 快 shell 首帧字形同帧进纹理：白块恰 1px，两行文本 + 提示符
    // 实测 ~4000px；阈值留足余量。=1 即竞态回归（字节滞留 rx）。
    assert!(
        ink > 1000,
        "快 shell 首帧未进 vt/atlas（启动唤醒竞态回归）：ink={ink}（1=黑屏）"
    );
}
