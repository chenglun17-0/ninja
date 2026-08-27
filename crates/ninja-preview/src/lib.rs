//! ninja-preview（p0 空壳）。
//!
//! 官方示例插件：只预览文本和代码，独立进程，第一次点击才被拉起。
//! 它只允许通过 ninja-protocol 的 JSON 消息与宿主说话，禁止链接宿主内部 API；
//! 层像素走宿主分配的 IOSurface。真正实现在 p5（层 + 预览）。

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder() {}
}
