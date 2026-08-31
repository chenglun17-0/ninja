# tools/：本地钉版工具链

本目录被 `.gitignore` 忽略（`README.md` 除外），存放按需钉版的工具链。

## zig 0.15.2（q0）

终端核是 vendored libghostty 嵌入（`vendor/ghostty/`，钉 ghostty commit
`a887df42c56f6de86c0fe6da9c4eeca37931e083`，1.3.2-dev）；该钉点
`build.zig.zon` 要求 `minimum_zig_version = "0.15.2"`，而 ghostty HEAD 与
brew stable 均已要求 0.16——所以本仓库**不要** `brew install zig`（会反向
遮蔽钉版，vendored 构建会用到错的 zig；PATH 上 `/opt/homebrew/bin` 在
`/usr/local/bin` 之前）。安装方式（aarch64 macos tarball）：

```sh
mkdir -p tools && cd tools
curl -L -o zig-0.15.2.tar.xz \
  "https://ziglang.org/download/0.15.2/zig-aarch64-macos-0.15.2.tar.xz"
tar xJf zig-0.15.2.tar.xz && rm zig-0.15.2.tar.xz   # -> zig-aarch64-macos-0.15.2/
```

本机 `/usr/local/bin/zig` 是指向本目录钉版的符号链接（tools/ 目录被删后
该链接悬空、`zig version` 报 command not found，按上面装回来即复活）：

```sh
ln -sf "$PWD/tools/zig-aarch64-macos-0.15.2/zig" /usr/local/bin/zig
zig version   # 0.15.2
```

macOS 之外或别的机器同理：保证 `zig version` 输出 0.15.2 即可（x86_64 用
`zig-x86_64-macos-0.15.2.tar.xz`；`vendor/ghostty/build.sh` 会按本机架构
选 `-Dtarget`）。

首次 `cargo build` 会经 `vendor/ghostty/fetch.sh` 联网拉 ghostty 钉点
codeload tarball（约 100MB，带 sha256 校验；离线可预置
`GHOSTTY_EMBED_TARBALL` 环境变量指向已下载的 tarball）。
