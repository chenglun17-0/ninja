//! X2 标题栏 E2E 共用件：窗口级像素探针（tools/verify/shot_window.swift）。
//!
//! 标题栏不在任何 Metal drawable 里（系统画的窗口背景/标题文字），
//! `NINJA_DUMP_DRAWABLE` 探不到，只能整窗截图采样。swift 脚本按 PID 找
//! CGWindowID → `screencapture -l<wid>`（CGWindowListCreateImage 在
//! macOS 15 SDK 起废弃）→ 采样相对矩形平均色。
//!
//! 需要可交互 GUI 会话（同 NINJA_E2E 惯例）；screencapture 属 TCC 屏幕录
//! 制，运行终端需已授权（本仓 E2E 环境前提）。

// 共享模块被多个测试二进制各自编译，并非每个函数在每个二进制都用。
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// 标题栏采样矩形（窗口相对 0-1，左上原点）：标题栏上带——顶部边缘与
/// 标题文字之间的空档、红绿灯右侧，避开文字/按钮/分隔线；无标签窗与
/// 标签栏窗（标签条占标题栏区、标签文字更靠下）都适用。
pub const TITLEBAR_REGION: (f64, f64, f64, f64) = (0.55, 0.012, 0.25, 0.01);

/// 逐通道容差（screencapture 的色彩空间换算有 ±1-2 抖动，实测 ODP 探
/// 得 (40,44,52)-(41,45,53)、solarized 探得 (2,43,54)）。
pub const TOL: i32 = 4;

pub fn shot_window_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/verify/shot_window.swift")
}

/// 跑 shot_window.swift 的 probe/probeall：返回每窗口的平均 RGB。
/// 失败（无窗口/截图失败/坏 JSON）= 空 Vec（调用方重试或断言失败）。
fn run_probe(mode: &str, pid: u32, region: (f64, f64, f64, f64)) -> Vec<[u8; 3]> {
    let (x, y, w, h) = region;
    let out = Command::new("swift")
        .arg(shot_window_script())
        .args([
            mode,
            &pid.to_string(),
            &format!("{x}"),
            &format!("{y}"),
            &format!("{w}"),
            &format!("{h}"),
        ])
        // 同 layer_preview：CLT swift 与自身 SDK 不配套，剥 DEVELOPER_DIR。
        .env_remove("DEVELOPER_DIR")
        .output()
        .expect("swift shot_window");
    if !out.status.success() {
        eprintln!(
            "shot_window {mode} pid={pid} status={} err={:?}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            // 行形如 {"avg":[40,44,52],"w":1254,"h":784}
            let a = l.find("\"avg\":[")? + "\"avg\":[".len();
            let b = l[a..].find("]")? + a;
            let nums: Vec<i32> = l[a..b]
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            (nums.len() == 3).then(|| [nums[0] as u8, nums[1] as u8, nums[2] as u8])
        })
        .collect()
}

/// 第一个窗口的标题栏采样（单窗断言用）。
pub fn probe_titlebar(pid: u32) -> Option<[u8; 3]> {
    run_probe("probe", pid, TITLEBAR_REGION).into_iter().next()
}

/// 全部在屏窗口的标题栏采样（多窗口/标签一致性断言用；返回条数即窗口
/// 数）。
pub fn probe_all_titlebars(pid: u32) -> Vec<[u8; 3]> {
    run_probe("probeall", pid, TITLEBAR_REGION)
}

/// 平均色 ≈ 目标色（逐通道 ±TOL）。
pub fn near(a: [u8; 3], t: (u8, u8, u8)) -> bool {
    (i32::from(a[0]) - i32::from(t.0)).abs() <= TOL
        && (i32::from(a[1]) - i32::from(t.1)).abs() <= TOL
        && (i32::from(a[2]) - i32::from(t.2)).abs() <= TOL
}

/// 等条件成立（200ms 拍；probe 本身 ~1s，这里主要防 theme.set 到位前
/// 的竞态）。
pub fn wait_until<T>(total: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + total;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// 等任一在屏窗口标题栏 ≈ 目标色（宿主还有非终端窗——如插件面板，
/// 面板不主题化——时用；返回命中采样）。
pub fn wait_any_titlebar_near(pid: u32, target: (u8, u8, u8), total: Duration) -> Option<[u8; 3]> {
    wait_until(total, || {
        probe_all_titlebars(pid)
            .into_iter()
            .find(|&c| near(c, target))
    })
}

/// 等宿主进程在屏窗口出现且全部标题栏 ≈ 目标色（返回各窗采样，供断言
/// 消息取证）。
pub fn wait_all_titlebars_near(
    pid: u32,
    target: (u8, u8, u8),
    total: Duration,
) -> Option<Vec<[u8; 3]>> {
    wait_until(total, || {
        let shots = probe_all_titlebars(pid);
        (!shots.is_empty() && shots.iter().all(|&c| near(c, target))).then_some(shots)
    })
}
