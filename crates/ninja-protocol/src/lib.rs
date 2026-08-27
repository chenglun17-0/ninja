//! ninja-protocol（p0 空壳）。
//!
//! ADE 协议：进程外、版本化、五类消息（hit / layer / input / spawn / config）。
//! 线格式：Unix socket 上 `u32le 长度 + UTF-8 JSON`，每条消息带 `v` 和 `type`；
//! 宿主忽略未知字段，插件遇到不支持的 `v` 必须退出。
//!
//! 本 crate 不依赖宿主 ninja，宿主也不依赖它——协议只在启用插件后
//! 经 socket 交换字节，双方永远不共享地址空间。编码解码与契约测试在 p3 实现。

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder() {}
}
