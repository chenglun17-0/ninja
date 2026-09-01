# 分发：打包、签名与 Gatekeeper

本文描述现行打包链与安装行为。打包脚本在 `scripts/`；开发环境见 [docs/development.md](docs/development.md)。

**决策状态：不购 99 美元 Developer ID、不做公证。** 分发 = 本地 tap + cask + 拖拽式 DMG。仓库不公开；DMG 公开托管与 tap 公开发布是后续决定（公开分发前必须补 Developer ID + notarization，见文末）。

## 产物与打包链

| 脚本 | 产物 | 做什么 |
| --- | --- | --- |
| `scripts/package_app.sh` | `dist/Ninja.app` | `cargo build --release -p ninja` → 程序化图标（像素自检失败即中止）→ 拷 vendored ghostty 主题资源 → `Contents/Resources/ghostty` → tic `xterm-ghostty` terminfo → `Contents/Resources/terminfo` → 组 bundle（Info.plist 最小键集）→ 动态解析签名身份 → `codesign --force --identifier dev.ninja.ninja --options runtime` → `--verify --deep --strict`；自检 MacOS/ 只有宿主二进制（默认零插件红线） |
| `scripts/package_dmg.sh` | `dist/Ninja-<version>-arm64.dmg` | staging（Ninja.app + `/Applications` 符号链接）→ `hdiutil UDZO` → 挂卷自检（验签、MacOS/ 只有宿主、图标、themes、terminfo）→ 再生 `scripts/tap/Casks/ninja.rb`（version + sha256 + file:// url 钉 DMG 实物） |
| `scripts/tap/` | tap 仓库模板（入库） | `Casks/ninja.rb`（生成物，勿手改）+ README（用法与 Gatekeeper 语义） |

- version 单源 = workspace `Cargo.toml` 的 `[workspace.package] version`：cask `version`、DMG 文件名、`Info.plist` 的版本键全部由它派生；cask `sha256` 钉 DMG 实物（每次打包再生）。
- bundle id **dev.ninja.ninja**（= codesign `--identifier`）。
- `LSMinimumSystemVersion` 取产物二进制 `LC_BUILD_VERSION` minos（11.0）。
- `dist/` 已入 `.gitignore`。
- **默认零插件红线**：bundle/DMG 只有宿主二进制 + 图标 + ghostty 主题资源 + terminfo；打包与 DMG 自检双向断言。
- **terminfo 随包**：libghostty 设 PTY 的 `TERM=xterm-ghostty`、`TERMINFO=<Resources>/terminfo`。缺这份数据库时 zsh 的 autosuggestions/syntax-highlighting 光标回退失败，输入会画成 `llsls`。
- **主题资源随包**：宿主 `ensure_resources_dir` 解析优先级 = 已设 `GHOSTTY_RESOURCES_DIR`（用户覆盖）> bundle 相对（`Contents/Resources/ghostty`）> build.rs 烘入的开发树绝对路径。分发机上烘入路径不存在，bundle 相对是唯一真源。
- **shell-integration 随包**（`Contents/Resources/ghostty/shell-integration`）：缺则 OSC-7 不来，相对路径 ⌘+click 无法解析。

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

- 卸载：`brew uninstall --cask ninja`（带走 `/Applications/Ninja.app` 与 Caskroom 条目）；拔 tap：`brew untap ninja/local`。
- 不走 brew：双击挂载 DMG 拖 `Ninja.app` 进 `/Applications`（本机自打 DMG 手工安装不带隔离属性，直接可开）。
- **换构建后须完全退出 Ninja 再开**（⌘Q，不是关窗）。

## 签名身份现实与 Gatekeeper（macOS 26.6.1 arm64，Homebrew 5.1.8；如实记录）

- 本机只有 Apple Development 证书（`security find-identity -v -p codesigning` 动态解析；缺身份则打包失败，绝不 adhoc），没有 Developer ID Application，不做公证（决策）。
- `spctl -a -vv /Applications/Ninja.app` → **rejected**；syspolicyd 首开扫描报 `Code did not match any currently allowed policy`。这是无公证的预期状态，不伪造、不绕过。
- 隔离语义实测：
  1. Homebrew 5.1.8 对 **file:// DMG 的 cask 安装也会给 `/Applications/Ninja.app` 打 `com.apple.quarantine`**（brew 自己打的，与是否网络下载无关）。
  2. 带隔离属性时 `open` 被 Gatekeeper 拦：syspolicyd `Prompt shown` → 无人响应 → 进程不启动。
  3. `--no-quarantine` CLI 开关在本机 brew 5.1.8 已禁用（"Calling the `--[no-]quarantine` switch is disabled!"）；**`HOMEBREW_CASK_OPTS="--no-quarantine"` 是有效开关**（装出的副本无隔离属性、直接可开）。
  4. 事后处理：`xattr -dr com.apple.quarantine /Applications/Ninja.app` → 可开。
  5. 本机直拖 DMG（不经 brew）无隔离属性，直接可开。

## 插件本地安装 / 卸载

插件不随分发物（默认零插件）。二进制解析段次序：`[plugins.paths]` 显式路径 → `$NINJA_PLUGIN_DIR/<name>` → `~/.config/ninja/plugins/<name>`（分发缺省安装位）→ 宿主二进制同目录（开发布局回退；**往已签名 bundle 的 Contents/MacOS/ 里放文件会破坏签名封条，分发链路不承诺该段**）。

放进 `~/.config/ninja/plugins/` 的二进制会在面板（⌘,）里出现为「未启用」行；开关即启停并写回 ninja.toml；替换二进制自动热重载（mtime）。安装示例见 [docs/cookbook/write-a-plugin.md](docs/cookbook/write-a-plugin.md)。

卸载：面板 off → 删 `~/.config/ninja/plugins/<name>`。

## 公证与公开分发（后续决定，当前不做）

公开分发（他人下载 DMG/cask）之前必须：购 Developer ID Application → 重签 → `notarytool submit` + `stapler`。当前 `--options runtime` 已带上（公证前置要求），补证书即可走公证，无需改打包策略。在那之前：仓库不公开、DMG 不公开托管、tap 不公开发布（`url` 是本机 file:// 路径，`homepage` 占位 `example.invalid`）——这些是决策状态，不是遗漏。
