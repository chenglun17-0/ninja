//! D-C 渲染跳帧的 E2E 取证（`NINJA_E2E=1` 门控，同 idle_no_plugins 惯例）。
//!
//! 场景：fakesh 打一行提示符后，反复 `printf '\r'`（光标归零——vt 对纯
//! 光标移动不标脏，第二次起就是 Clean 且视觉未变的帧）。
//! 断言（`NINJA_FRAME_STATS` 探针）：
//!
//! 1. `skipped > 0`——Clean 未变帧确实不提交 drawable（修前每帧全量
//!    重画：组顶点 + nextDrawable + 提交，skipped 恒 0）；
//! 2. `drawn >= 2`——首帧与首次光标移动仍正常提交（跳帧不吞真变化）；
//! 3. 同时 `NINJA_DUMP_ATLAS` 的 atlas 落盘有墨迹——首帧字形同帧上传
//!    语义未被跳帧破坏（空闲首开回归）。
//!
//! 红线对照（空闲 CPU=0，无自旋）由 tools/idle_cpu_probe.sh 取证，
//! 不在此拉进程测量。

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Reaper(Vec<Child>);

impl Drop for Reaper {
    fn drop(&mut self) {
        for c in self.0.iter_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[test]
fn clean_unchanged_frames_skip_in_real_app() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip: 拉真实窗口进程，设 NINJA_E2E=1 启用");
        return;
    }

    let dir = std::env::temp_dir();
    let tag = format!("{}_{}", std::process::id(), std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos());
    let fakesh = dir.join(format!("ninja_dcamp_fakesh_{tag}.sh"));
    // 首行提示符 + 12 次 \r（首帧后，第 1 次动光标，第 2..12 次 Clean 未变）
    // + 阻塞收尾。
    std::fs::write(
        &fakesh,
        concat!(
            "#!/bin/bash\n",
            "printf 'idle%% '\n",
            "for i in 1 2 3 4 5 6 7 8 9 10 11 12; do printf '\\r'; sleep 0.2; done\n",
            "read _x\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fakesh, std::fs::Permissions::from_mode(0o755)).unwrap();

    let stats: PathBuf = dir.join(format!("ninja_dcamp_stats_{tag}.json"));
    let pgm: PathBuf = dir.join(format!("ninja_dcamp_atlas_{tag}.pgm"));
    let _ = std::fs::remove_file(&stats);
    let _ = std::fs::remove_file(&pgm);

    let mut reaper = Reaper(Vec::new());
    let child = Command::new(env!("CARGO_BIN_EXE_ninja"))
        .env("SHELL", &fakesh)
        .env("NINJA_FRAME_STATS", &stats)
        .env("NINJA_DUMP_ATLAS", &pgm)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ninja binary");
    reaper.0.push(child);

    // 提示符 + 12×0.2s 的 \r 序列 + 探针 ≥200ms 节流 → 4s 足够收敛。
    std::thread::sleep(Duration::from_secs(4));
    let _ = std::fs::remove_file(&fakesh);

    let stats_text = std::fs::read_to_string(&stats)
        .unwrap_or_else(|_| panic!("帧统计探针未落盘 {stats:?}（首帧都没画？）"));
    let (drawn, skipped) = {
        // {"drawn":N,"skipped":M,"dirty":"..."}
        let get = |k: &str| -> u64 {
            stats_text
                .split(&format!("\"{k}\":"))
                .nth(1)
                .and_then(|rest| {
                    rest.split([',', '}'])
                        .next()
                        .and_then(|v| v.trim().parse().ok())
                })
                .unwrap_or(0)
        };
        (get("drawn"), get("skipped"))
    };
    let _ = std::fs::remove_file(&stats);

    // 修前语义：每个 PTY 字节事件都全量重画，skipped 恒 0。
    assert!(
        skipped >= 5,
        "Clean 未变帧没有被跳过（skipped={skipped}, drawn={drawn}）——跳帧判据失效"
    );
    // 真变化不被吞：首帧 + 首次光标移动至少两帧。
    assert!(drawn >= 2, "跳帧吞掉了真变化（drawn={drawn}）");

    // 空闲首开同帧上传未被破坏：atlas 有字形墨迹（远超 1px 白块）。
    let data = std::fs::read(&pgm)
        .unwrap_or_else(|_| panic!("atlas 取证未落盘 {pgm:?}（首帧未画？）"));
    let _ = std::fs::remove_file(&pgm);
    let idx = data
        .windows(4)
        .position(|w| w == b"255\n")
        .expect("PGM header")
        + 4;
    let ink = data[idx..].iter().filter(|&&v| v > 0).count();
    assert!(ink > 200, "首帧字形未进 atlas：ink={ink}（同帧上传回归？）");
}
