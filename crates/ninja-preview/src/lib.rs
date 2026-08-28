//! ninja-preview 宿主库位（无产品代码）：全部实现在 `src/main.rs`
//!（独立进程 crate，无 lib 目标以外的形态）。
//!
//! 见 `src/main.rs` 的 crate 文档：官方示例插件——文本/代码 pager，
//! 只经 ADE 协议（ninja-protocol JSON 帧 + 宿主分配的 IOSurface）与
//! 宿主说话，永不链宿主内部 API。
