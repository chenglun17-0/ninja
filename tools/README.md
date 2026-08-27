# tools/：本地钉版工具链

本目录被 `.gitignore` 忽略（`README.md` 除外），存放按需钉版的工具链。

## zig 0.15.2（p0）

宿主通过 `libghostty-vt` 0.2.1（sys 层钉 ghostty commit
`a887df42c56f6de86c0fe6da9c4eeca37931e083`，1.3.2-dev）vendored 构建终端核，
该钉点要求 `minimum_zig_version = "0.15.2"`；ghostty HEAD 已要求 0.16，
故 brew stable（0.16.x）不可用于钉点构建。安装方式：

```sh
curl -L -o zig-0.15.2.tar.xz \
  "https://ziglang.org/download/0.15.2/zig-aarch64-macos-0.15.2.tar.xz"
tar xJf zig-0.15.2.tar.xz && rm zig-0.15.2.tar.xz   # -> zig-aarch64-macos-0.15.2/
```

本机历史上 `/usr/local/bin/zig` 指向损坏的 Homebrew Intel 前缀 zig 0.13
（缺 `libz3.4.13.dylib`，`zig version` 即 abort）。p0 已把该符号链接重指向
本目录的钉版：

```sh
ln -sf "$PWD/zig-aarch64-macos-0.15.2/zig" /usr/local/bin/zig
zig version   # 0.15.2
```

注意 PATH 顺序：`/opt/homebrew/bin` 在 `/usr/local/bin` 之前。因此本仓库
**不要** `brew install zig`（0.16 会反向遮蔽钉版，vendored 构建会用到错的
zig）。macOS 之外或别的机器同理：保证 `zig version` 是 0.15.x 即可。

首次 `cargo build/test` 会联网 clone ghostty 钉点并拉 zig 依赖
（离线场景可用 `GHOSTTY_SOURCE_DIR` / `GHOSTTY_ZIG_SYSTEM_DIR` 预置）。
