//! T-主题 E2E 取证（`NINJA_E2E=1` 门控，同 idle_no_plugins 惯例）：
//! **启动即 One Dark Pro**——像素探针（`NINJA_DUMP_DRAWABLE`：渲染器把
//! 已呈现 drawable 读回落盘 PPM）逐像素验色：
//!
//! 1. **背景 #282C34**：远离光标/文本的右下角像素 = (40,44,52)±2
//!    （clear 色即 frame.bg，钉在 vt 核默认背景上）；
//! 2. **前景 #ABB2BF**：fakesh 的无色提示文本有足够多的 ≈(171,178,191)
//!    像素（字形核心像素 = 默认前景）；
//! 3. **ANSI 官方色**：SGR 31/91/42 的文本像素 ≈ #e05561/#ff616e/#8cc265
//!    （vt 调色板 0-15 被 One Dark Pro 官方 16 色替换）。
//!
//! 纯逻辑回归（vt 默认色/调色板、Theme 常量）在 theme.rs 单测；本测试
//! 只证「真实窗口进程从启动到像素」这条链。

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Reaper(Vec<Child>);

impl Drop for Reaper {
    fn drop(&mut self) {
        for c in self.0.iter_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// 解析 P6 PPM：返回 (宽, 高, 像素 RGB)。
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

/// 统计 ≈ 目标色（逐通道容差 tol）的像素数。
fn count_near(px: &[u8], target: (u8, u8, u8), tol: i32) -> usize {
    px.chunks_exact(3)
        .filter(|p| {
            (i32::from(p[0]) - i32::from(target.0)).abs() <= tol
                && (i32::from(p[1]) - i32::from(target.1)).abs() <= tol
                && (i32::from(p[2]) - i32::from(target.2)).abs() <= tol
        })
        .count()
}

/// 等到帧集合稳定且「终点标记」已出现——任一帧里有 SGR 亮红像素
///（fakesh 最后一段输出；跳帧后无更新）——返回全部槽位。先见稳定就
/// 返回会采到「shell 还没输出完」的中间帧（首跑冷启动即现场：
/// 纯背景帧稳定 250ms → 前景/ANSI 断言误炸）。超时返回手头内容
///（断言会给出具体哪个色不够，便于取证）。
fn wait_frames(dir: &Path, total: Duration) -> Vec<Vec<u8>> {
    let deadline = Instant::now() + total;
    let mut last: Vec<Vec<u8>> = Vec::new();
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        let mut frames: Vec<Vec<u8>> = Vec::new();
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = entry.path();
            let is_frame = p.extension().is_some_and(|x| x == "ppm")
                && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("frame_"));
            if is_frame && let Ok(d) = std::fs::read(&p) {
                frames.push(d);
            }
        }
        let marker_seen = frames.iter().any(|d| {
            parse_ppm(d).is_some_and(|(_, _, px)| count_near(px, (0xFF, 0x61, 0x6E), 10) > 0)
        });
        if !frames.is_empty() && frames == last && marker_seen {
            return frames;
        }
        last = frames;
    }
    last
}

#[test]
fn startup_pixels_are_one_dark_pro() {
    if std::env::var_os("NINJA_E2E").is_none() {
        eprintln!("skip: 拉真实窗口进程，设 NINJA_E2E=1 启用");
        return;
    }

    let dir = std::env::temp_dir();
    let tag = format!(
        "t{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    // fakesh：无色提示词（前景）+ SGR 31/91/42 三段官方 ANSI 色，然后
    // 阻塞等收割（窗口保持活着，跳帧后探针文件稳定）。
    let fakesh = dir.join(format!("ninja_theme_fakesh_{tag}.sh"));
    std::fs::write(
        &fakesh,
        concat!(
            "#!/bin/bash\n",
            "printf 'odp%% '\n",
            "printf '\\033[31mRED\\033[0m \\033[91mBRED\\033[0m \\033[42mGREENBG\\033[0m\\n'\n",
            "read _x\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fakesh, std::fs::Permissions::from_mode(0o755)).unwrap();

    let probe_dir = dir.join(format!("ninja_theme_probe_{tag}"));
    std::fs::create_dir_all(&probe_dir).unwrap();

    let mut reaper = Reaper(Vec::new());
    let child = Command::new(env!("CARGO_BIN_EXE_ninja"))
        .env("SHELL", &fakesh)
        .env("NINJA_DUMP_DRAWABLE", &probe_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ninja binary");
    reaper.0.push(child);

    // 首帧 + SGR 三段输出 + 跳帧收敛 → 6s 足够；再等内容稳定。
    let frames = wait_frames(&probe_dir, Duration::from_secs(6));
    let _ = std::fs::remove_file(&fakesh);
    let _ = std::fs::remove_dir_all(&probe_dir);

    assert!(
        !frames.is_empty(),
        "drawable 探针未落盘（首帧都没画？）：{probe_dir:?}"
    );

    // 断言集合：在「最新可用」的帧集合上找证据（cyclic 3 槽位，任一
    // 帧命中即算——fakesh 三段输出可能跨槽位）。
    let bg_target: (u8, u8, u8) = (0x28, 0x2C, 0x34);
    let fg_target: (u8, u8, u8) = (0xAB, 0xB2, 0xBF);
    let red_target: (u8, u8, u8) = (0xE0, 0x55, 0x61);
    let bred_target: (u8, u8, u8) = (0xFF, 0x61, 0x6E);
    let green_bg_target: (u8, u8, u8) = (0x8C, 0xC2, 0x65);

    let mut bg_ok = false;
    let mut fg_px = 0usize;
    let mut red_px = 0usize;
    let mut bred_px = 0usize;
    let mut green_bg_px = 0usize;
    for data in &frames {
        let Some((w, h, px)) = parse_ppm(data) else { continue };
        // 背景：右下角 16x16 区域（远离文本与光标，纯 clear 色）。
        let mut corner_bg = true;
        'outer: for y in (h - 16..h).step_by(2) {
            for x in (w - 16..w).step_by(2) {
                let p = &px[(y * w + x) * 3..(y * w + x) * 3 + 3];
                if (i32::from(p[0]) - i32::from(bg_target.0)).abs() > 2
                    || (i32::from(p[1]) - i32::from(bg_target.1)).abs() > 2
                    || (i32::from(p[2]) - i32::from(bg_target.2)).abs() > 2
                {
                    corner_bg = false;
                    break 'outer;
                }
            }
        }
        bg_ok |= corner_bg;
        fg_px = fg_px.max(count_near(px, fg_target, 10));
        red_px = red_px.max(count_near(px, red_target, 10));
        bred_px = bred_px.max(count_near(px, bred_target, 10));
        green_bg_px = green_bg_px.max(count_near(px, green_bg_target, 10));
    }

    assert!(bg_ok, "背景像素 != #282C34（One Dark Pro 未生效？）");
    assert!(fg_px > 50, "默认前景 #ABB2BF 像素不足：{fg_px}");
    assert!(red_px > 30, "ANSI red #e05561 像素不足：{red_px}");
    assert!(bred_px > 30, "ANSI bright red #ff616e 像素不足：{bred_px}");
    assert!(green_bg_px > 30, "ANSI green 背景 #8cc265 像素不足：{green_bg_px}");
}
