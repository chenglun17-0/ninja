# p7 分发：签名 .app + DMG（默认零插件）

对应 [PLAN.md](PLAN.md) p7「签名分发」；产品约束见 [PRODUCT.md](PRODUCT.md)（默认零插件：
分发物不含预览、不含 Agent）。仓库仍不公开——本文件描述的是**已知收件人**的分发，
不是公开下载分发（见「公证残留」）。

## 产物与打包

| 脚本 | 产物 | 做什么 |
| --- | --- | --- |
| `scripts/package_app.sh` | `dist/Ninja.app` | `cargo build --release -p ninja`（仅宿主，**不打 ninja-preview**）→ 组 bundle → `codesign --force --identifier dev.ninja.ninja --options runtime` → `--verify --deep --strict` 自检 |
| `scripts/package_dmg.sh` | `dist/Ninja-0.1.0-arm64.dmg` | staging（Ninja.app + `/Applications` 符号链接，拖拽安装）→ `hdiutil UDZO` → 挂卷验签/清点自检 |

- `dist/` 已入 `.gitignore`；脚本在 `scripts/`（tracked；`tools/` 整目录被忽略故不放那）。
- bundle id **dev.ninja.ninja**（同时是 codesign `--identifier`，签名稳定标识）。
- Info.plist 最小键集：`CFBundlePackageType=APPL`、`CFBundleExecutable=ninja`、
  `CFBundleName=Ninja`、`CFBundleIdentifier=dev.ninja.ninja`、
  `CFBundleShortVersionString=CFBundleVersion=0.1.0`、`CFBundleDevelopmentRegion=en`、
  `NSHighResolutionCapable=true`。
- `LSMinimumSystemVersion` 由脚本从**产物二进制的 `LC_BUILD_VERSION` minos** 读出
  （aarch64-apple-darwin rustc 默认部署目标 = **11.0**）。objc2 0.6 / objc2-app-kit
  0.3 声明的支持下限（约 macOS 10.12/10.13）比它宽，但 arm64 二进制在 11.0 以下本就
  无机器可跑且 dyld 拒载，写更低是谎——取「二进制实际下限」为准。实机验证：
  macOS 26.6.1 arm64。

## 安装

1. `scripts/package_app.sh && scripts/package_dmg.sh`；
2. 双击挂载 DMG，把 `Ninja.app` 拖进 `/Applications`（或 `~/Applications`），弹出卷；
   或直接 `cp -R dist/Ninja.app /Applications/`；
3. 打开即日常终端：多窗口/标签/分屏/中文输入/复制粘贴（p1–p2 面），默认零插件。

本机（签名所在机器）可直接 `open /Applications/Ninja.app` 运行。

## 公证残留（如实记录，不绕过）

- **本机只有 Apple Development 证书**（`Apple Development: zhan zong (35SLXS3LTS)`，
  脚本从 `security find-identity -v -p codesigning` 动态解析；缺身份则打包失败，
  绝不静默 adhoc），**没有 Developer ID Application**，因此：
- **notarization 做不了**（公证硬性要求 Developer ID Application + Apple Developer
  Program）。`--options runtime` 已带上（公证前置要求），证书补齐后即可走
  `notarytool submit` + `stapler`。
- 后果：**他人下载的隔离副本**（带 `com.apple.quarantine`）首次打开会被 Gatekeeper
  拦（`spctl -a -vv` 评估不通过——本机实测记录见下）。这是残留，不伪造凭据、不打包
  假 adhoc 副本掩盖。
- 本机验证途径（仅本机、非分发承诺）：
  `xattr -dr com.apple.quarantine /Applications/Ninja.app` 去隔离属性后照常打开；
  同机 Apple Development 签名 + 非下载来源的副本本就不带隔离属性。
- 图标（`CFBundleIconFile`）非门禁项：暂无 .icns 资产，未做（p7 验收不含图标）。

## 插件本地安装 / 卸载

插件解析次序（宿主 `resolve_plugin_binary`）：`[plugins.paths]` 显式路径 →
`$NINJA_PLUGIN_DIR/<name>` → **`~/.config/ninja/plugins/<name>`（分发缺省安装位）**
→ 宿主二进制同目录（开发布局回退；在已签名 bundle 里指向 `Contents/MacOS/`，
往里放文件会破坏签名封条——**分发链路不承诺该段**，装插件只用上面的用户目录）。

**安装**（以官方示例文本预览为例，ninja-preview 不随 .app 分发）：

```sh
cargo build --release -p ninja-preview
mkdir -p ~/.config/ninja/plugins
cp target/release/ninja-preview ~/.config/ninja/plugins/preview
# ~/.config/ninja/ninja.toml：
#   [plugins]
#   enabled = ["preview"]
```

启用 ≠ 常驻：首次 Cmd+点击路径时才拉起进程（PRODUCT 规则，p5 起即如此）。

**卸载**：

1. `ninja.toml` 里把名字移出 `enabled`（或清空该表）——已跑着的会话用 p6 的禁用
   钩子语义收干净（层收回、连接断开、子进程收割、socket 文件删除，无残留）；
2. `rm ~/.config/ninja/plugins/preview`——删文件时进程早已不在（p6 保证），直接删。

## 签名验证命令

```sh
codesign --verify --deep --verbose=2 dist/Ninja.app   # 应通过（Apple Development 签名）
spctl -a -vv dist/Ninja.app                            # 预期不通过（非 Developer ID，见公证残留）
```

## p7 本机取证摘要（2026-08，macOS 26.6.1 arm64）

实施者与验证员各自复跑（证据见 workflow 记录）：

- `codesign --verify --deep --verbose=2 dist/Ninja.app` → satisfied；
- `spctl -a -vv` → rejected（expected，公证残留）；
- `open` 启动 /Applications 与 dist 两处副本 → GUI 在；默认配置启动后
  `$TMPDIR` 无 `ninja-ade-*.sock`、无 `ninja-preview` 进程（默认零插件）；
- `HOME=<临时目录>` 启动 → 正常（配置路径机器无关）；
- `~/.config/ninja/plugins/preview` + `enabled=["preview"]` → 首击拉起（`NINJA_P4_HIT`
  钩子触发）；禁用（p6 钩子 `NINJA_P6_PLUGIN_FILE`）+ 删文件 → 无进程/无 socket 残留；
- bundle 与 DMG 内均无 `ninja-preview`（`Contents/` 只有 `MacOS/ninja` + `Info.plist`）。
