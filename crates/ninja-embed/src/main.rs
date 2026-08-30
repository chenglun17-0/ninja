//! ninja-embed：v2 嵌入路径宿主。
//!
//! - 默认（无参数）：q2 配置壳——ghostty 配置全量装载（主题/字体/键位
//!   全继承，菜单/热重载接配置系统，[`app::run`]）。
//! - `--evidence-dir DIR`：q0 取证模式保留可重跑（[`q0_demo::run`]，
//!   审计文档 docs/Q0-CAPABILITY-AUDIT.md 的复现入口，勿破坏）。

mod app;
mod config;
mod host;
mod keymap;
mod pane;
mod panel;
// q0 取证机原样保留（审计文档复现依赖其行为与输出；q0 时代的 lint
// 噪声不随之重构）。
#[allow(deprecated, clippy::manual_c_str_literals, clippy::unnecessary_cast, clippy::ptr_arg, clippy::collapsible_if)]
mod q0_demo;
mod shell;
mod surface;

use std::path::PathBuf;

fn main() {
    // 具名主题资源目录（q2）：vendored 构建烘入的路径在 ghostty_init
    // **之前**设为 GHOSTTY_RESOURCES_DIR（resourcesdir.zig 在 init 时读
    // 一次；ReleaseFast 下环境变量优先）。已设（用户覆盖/调试）不动。
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
                eprintln!("unknown arg {other}; usage: ninja-embed [--evidence-dir DIR]");
                std::process::exit(2);
            }
        }
    }
    match evidence_dir {
        Some(dir) => q0_demo::run(dir),
        None => app::run(),
    }
}
