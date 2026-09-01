//! ghostty-sys：libghostty 全量嵌入 API（`include/ghostty.h`）的 Rust FFI。
//!
//! 本 crate 只做两件事：
//!
//! 1. build.rs 确保钉版 vendored 构建存在（`vendor/ghostty/`，钉
//!    ghostty commit a887df42…、zig 0.15.2，详见 vendor/ghostty/build.sh），
//!    静态链 `libghostty-internal.a`；
//! 2. bindgen 对安装出的 `include/ghostty.h` 生成绑定（仅公开嵌入面）。
//!
//! 嵌入 API pre-1.0：破坏性升级必须显式改钉点（vendor/ghostty/fetch.sh 的
//! COMMIT + SHA256）与本 crate 的 bindgen allowlist。

#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals, dead_code, clippy::all)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
