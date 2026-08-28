//! ninja 宿主入口：p2 = 多窗口/标签/分屏的 AppKit 壳 + 每 pane 一个
//! PTY + vt + Metal 终端面，TOML 配置（缺省可启动）。
//! p3：[plugins] enabled 非空才绑 ADE Unix socket；默认空载不建 socket、
//! 不拉插件进程，无 wasmtime/JS。

fn main() {
    ninja::app::run();
}
