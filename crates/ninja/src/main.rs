//! ninja 宿主（libghostty 嵌入）。
//!
//! - 默认（无参数）：q2 交互壳——配置系统（ghostty 配置全量装载 + 热重载
//!   + ODP 缺省）+ 多窗/原生标签/分屏布局树接 libghostty surface
//!   （[`app::run`]）。
//! - `--evidence-dir DIR`：q0 取证模式保留可重跑（[`q0_demo::run`]，
//!   审计文档 docs/Q0-CAPABILITY-AUDIT.md 的复现入口，勿破坏）。

mod app;
mod config;
mod host;
mod keymap;
mod pane;
// q0 取证机原样保留（审计文档复现依赖其行为与输出；q0 时代的 lint
// 噪声不随之重构）。
#[allow(deprecated, clippy::manual_c_str_literals, clippy::unnecessary_cast, clippy::ptr_arg, clippy::collapsible_if, clippy::manual_clamp)]
mod q0_demo;
mod shell;
mod surface;

use std::path::PathBuf;

fn main() {
    // q2：具名主题解析需要 GHOSTTY_RESOURCES_DIR（resourcesdir.zig 只在
    // ghostty_init 读一次）——两条运行路径（app/q0_demo）之前统一就位。
    config::ensure_resources_dir();
    let args: Vec<String> = std::env::args().collect();
    let mut evidence_dir: Option<PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--evidence-dir" => {
                evidence_dir = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            other => {
                eprintln!("unknown arg {other}; usage: ninja [--evidence-dir DIR]");
                std::process::exit(2);
            }
        }
    }
    match evidence_dir {
        Some(dir) => q0_demo::run(dir),
        None => app::run(),
    }
}
