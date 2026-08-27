//! ninja 宿主入口：p1 = 一个 AppKit 窗口 + PTY + vt + Metal 终端面。
//! 无插件、无 ADE socket、无 wasmtime/JS —— 那些是 p3+ 的事。

fn main() {
    ninja::app::run();
}
