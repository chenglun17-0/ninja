//! ninja 宿主 crate（p0 骨架）。
//!
//! 当前阶段只做一件事：把 [libghostty-vt] 的公开 C API（`include/ghostty/*.h`）
//! 静态链进宿主，并用 `tests/vt_smoke.rs` 证明链接真的可用。
//!
//! 空载路径 = 本 crate + 它的依赖。没有插件运行时、没有 wasmtime、没有 JS 引擎；
//! 插件以独立进程 + JSON 协议（ninja-protocol）在 p3+ 落地，永远不进宿主地址空间。
//!
//! PTY、Metal、AppKit 壳是 p1/p2 的事，这里刻意不写。
//!
//! [libghostty-vt]: https://libghostty.tip.ghostty.org/

/// 重导出钉版的终端核，宿主从第一天起只通过这层公开 API 说话。
pub use libghostty_vt;
