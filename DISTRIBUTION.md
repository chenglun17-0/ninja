# q4 分发：brew tap + cask/DMG（无公证）

对应 [PLAN.md](PLAN.md) q4「分发（brew tap + cask/DMG）」。**2026-08-31 用户决策：不购
99 美元 Developer ID、不做公证**——分发 = 本地 tap + cask + 拖拽式 DMG。仓库仍不公开；
DMG 公开托管与 tap 公开发布是**后续决定**（公开分发前必须补 Developer ID + notarization，
见文末）。

## 产物与打包链

| 脚本 | 产物 | 做什么 |
| --- | --- | --- |
| `scripts/package_app.sh` | `dist/Ninja.app` | `cargo build --release -p ninja`（仅宿主，**不打 ninja-preview/ninja-theme**）→ 程序化图标（`make_icon.sh` → `Resources/AppIcon.icns`，像素自检失败即中止）→ **拷 vendored ghostty 主题资源 → `Contents/Resources/ghostty`（574 主题，资源不是插件）** → 组 bundle（Info.plist 最小键集）→ 动态解析签名身份 → `codesign --force --identifier dev.ninja.ninja --options runtime` → `--verify --deep --strict` |
| `scripts/package_dmg.sh` | `dist/Ninja-0.1.0-arm64.dmg` | staging（Ninja.app + `/Applications` 符号链接，拖拽安装）→ `hdiutil UDZO` → 挂卷自检（验签、二进制在、**插件不在**、图标两侧在、**Resources/ghostty/themes 在**）→ **再生 `scripts/tap/Casks/ninja.rb`**（version + sha256 + file:// url 钉 DMG 实物） |
| `scripts/tap/` | tap 仓库模板（入库） | `Casks/ninja.rb`（生成物，勿手改）+ README（用法与 Gatekeeper 语义）；物化成独立 git 目录后 `brew tap` 接入 |

- version 单源 = workspace `Cargo.toml` 的 `[workspace.package] version`（0.1.0）：
  cask `version`、DMG 文件名、`Info.plist` 的 `CFBundleShortVersionString/CFBundleVersion`
  全部由它派生；cask `sha256` 钉 DMG 实物（每次打包再生）。
- bundle id **dev.ninja.ninja**（= codesign `--identifier`，签名稳定标识）。
- `LSMinimumSystemVersion` 取**产物二进制 LC_BUILD_VERSION minos**（11.0）。
- `dist/` 已入 `.gitignore`（DMG/.app 不入 git）；DMG 拖拽路径与 cask 路径并存。
- **默认零插件红线**：bundle/DMG 只有宿主二进制 + 图标 + ghostty 主题资源，
  无 ninja-preview/ninja-theme（打包与 DMG 自检双向断言；实测见
  [docs/q4-evidence/](docs/q4-evidence/)）。
- **主题资源随包**（q4 宿主唯一代码改动）：宿主 `ensure_resources_dir` 解析优先级 =
  已设 `GHOSTTY_RESOURCES_DIR`（用户覆盖/调试）> **bundle 相对**（`Contents/Resources/ghostty`）
  > build.rs 烘入的开发树绝对路径。分发机上烘入路径不存在，bundle 相对是唯一真源；
  本机（开发树也在）实测 bundle 相对优先（q4 取证 C4：安装副本 cfgdump 的
  `resources_dir` = `/Applications/Ninja.app/Contents/Resources/ghostty`）。

## 安装（本地 tap + file:// DMG）

```sh
# 打包（cask 随 DMG 再生）
scripts/package_app.sh && scripts/package_dmg.sh

# tap 仓库物化（独立 git 目录，brew tap 需要至少一个 commit）
rm -rf ~/my_repos/ninja-tap && mkdir -p ~/my_repos/ninja-tap
cp -R scripts/tap/. ~/my_repos/ninja-tap/
git -C ~/my_repos/ninja-tap init -q && git -C ~/my_repos/ninja-tap add -A && git -C ~/my_repos/ninja-tap commit -qm "ninja tap"

# 接入 + 安装（cask url 指向本机 dist/ 的 file:// DMG）
brew tap ninja/local ~/my_repos/ninja-tap
HOMEBREW_CASK_OPTS="--no-quarantine" brew install --cask ninja   # 为何要 env：见下节
```

- 卸载：`brew uninstall --cask ninja`（带走 `/Applications/Ninja.app` 与 Caskroom 条目，
  实测无残留，见 q4 取证 E 段）；拔 tap：`brew untap ninja/local`。
- 不走 brew 的路径：双击挂载 DMG 拖 `Ninja.app` 进 `/Applications`（本机自打 DMG
  手工安装**不带**隔离属性，直接可开）。

## 签名身份现实与 Gatekeeper 实测（macOS 26.6.1 arm64，Homebrew 5.1.8；如实记录）

- **本机只有 Apple Development 证书**（`Apple Development: zhan zong (35SLXS3LTS)`，
  `security find-identity -v -p codesigning` 动态解析；缺身份则打包失败，绝不 adhoc），
  **没有 Developer ID Application，不做公证**（用户决策）。
- `spctl -a -vv /Applications/Ninja.app` → **rejected**（origin=Apple Development…）；
  syspolicyd 首开扫描报 `GatekeeperPolicyScanError -67018 "Code did not match any
  currently allowed policy"`。这是无公证的**预期状态**，不伪造、不绕过。
- **实测隔离语义**（q4 取证 B/D 段，证据 `docs/q4-evidence/`）：
  1. Homebrew 5.1.8 对 **file:// DMG 的 cask 安装也会给 `/Applications/Ninja.app` 打
     `com.apple.quarantine`**（`0381;…`；brew 自己打的，与是否网络下载无关）。
  2. 带隔离属性时 `open` 被 Gatekeeper 拦：syspolicyd `Prompt shown` → 无人响应 →
     `denial breadcrumb`，**进程不启动**（两处实测：默认装副本、手工 `xattr -w
     com.apple.quarantine` 的 /tmp 副本即「他人下载模拟」，一致被拦）。
  3. **`--no-quarantine` CLI 开关在本机 brew 5.1.8 已禁用**（`brew install --cask
     ninja --no-quarantine` → "Calling the `--[no-]quarantine` switch is disabled!
     There is no replacement."）；**`HOMEBREW_CASK_OPTS="--no-quarantine"` 是有效开关**
     （实测装出的副本无隔离属性、直接可开）。
  4. 事后处理：`xattr -dr com.apple.quarantine /Applications/Ninja.app` → 可开（实测）。
  5. 本机直装（手工拖 DMG、不经 brew）**无隔离属性**，直接可开——与 cask 默认装的行为
     差异即上述隔离属性一物。
- 已如实记录的一次异常：实施首轮非受控试跑中，带隔离属性的默认装副本出现过一次
  「open 直接启动、syspolicyd 无扫描记录」；随后受控复跑（全新安装 → 立即 open，两次
  不同路径）均被拦。未归因，以受控链为准；取证脚本 D1 断言会抓住任何复现。

## 打开即日常终端（抽查结论）

q4 取证 C 段（虚拟屏）：cask 安装副本去隔离后 `open --env NINJA_E2E_SCREEN=…` 落虚拟
屏 → 窗口在虚拟屏、cfgdump 证 bundle 资源解析生效、背景像素 = ODP #282c34（真渲染）、
**真键盘输入 `touch` 命令被 shell 执行**（输入链路活）、无 ADE socket / 无插件进程
（零插件红线）。证据：`docs/q4-evidence/run-e2e.sh` + 产物（`c-terminal.png`、
`b-brew-install.log`、`d*-*.log`、`regression-q3.log`）。

## 插件本地安装 / 卸载

插件不随分发物（默认零插件）。宿主 `resolve_plugin_binary` 段次序：`[plugins.paths]`
显式路径 → `$NINJA_PLUGIN_DIR/<name>` → **`~/.config/ninja/plugins/<name>`（分发缺省
安装位）** → 宿主二进制同目录（开发布局回退；**往已签名 bundle 的 Contents/MacOS/ 里
放文件会破坏签名封条，分发链路不承诺该段**）。

```sh
cargo build --release -p ninja-preview
mkdir -p ~/.config/ninja/plugins
cp target/release/ninja-preview ~/.config/ninja/plugins/preview
# ~/.config/ninja/ninja.toml：
#   [plugins]
#   enabled = ["preview"]
```

- **启用即拉起**（q3 语义）：`enabled` 非空时宿主启动即拉起对应插件进程；theme 插件
  （`~/.config/ninja/plugins/theme`）同理，连接后即推色板。空载门禁不变：默认零插件 =
  零进程、零 socket。
- 卸载：ninja.toml 移出名单（跑着的会话按 q3 监督器语义回收干净）→ 删
  `~/.config/ninja/plugins/<name>`。

## 公证与公开分发（后续决定，当前不做）

- 公开分发（他人下载 DMG/cask）之前必须：购 Developer ID Application → 重签 →
  `notarytool submit` + `stapler`。当前 `--options runtime` 已带上（公证前置要求），
  补证书即可走公证，无需改打包策略。
- 在那之前：仓库不公开、DMG 不公开托管、tap 不公开发布（`url` 是本机 file:// 路径，
  `homepage` 占位 `example.invalid`）——这些是决策状态，不是遗漏。
