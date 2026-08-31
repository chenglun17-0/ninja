# ninja tap（本地 Homebrew tap 模板）

本目录是 **tap 仓库模板**（入库；真正给 brew 用的 tap 仓库是**本仓库之外**
的独立 git 目录，见下）。当前状态（与 [../../DISTRIBUTION.md](../../DISTRIBUTION.md) 一致）：

- 仓库不公开、DMG 不公开托管、tap 不公开发布——都是后续决定。
- 本地验证链走 `url "file:///…/dist/Ninja-x.y.z-arm64.dmg"`（绝对路径指向
  本机 `dist/`，由 `scripts/package_dmg.sh` 每次打包**再生** `Casks/ninja.rb`：
  version + sha256 钉 DMG 实物，不要手改）。

## 用法（本地 tap 验证）

```sh
# 0) 打包（两个脚本都会自检，cask 随 DMG 再生）
scripts/package_app.sh && scripts/package_dmg.sh

# 1) 把模板物化成独立 tap 仓库（一次性；重复跑就先删掉旧目录）
rm -rf ~/my_repos/ninja-tap
mkdir -p ~/my_repos/ninja-tap
cp -R scripts/tap/. ~/my_repos/ninja-tap/
git -C ~/my_repos/ninja-tap init -q
git -C ~/my_repos/ninja-tap add -A
git -C ~/my_repos/ninja-tap commit -qm "ninja tap"

# 2) 接入 brew 并安装（cask 走 file:// DMG）
brew tap ninja/local ~/my_repos/ninja-tap
brew install --cask ninja --no-quarantine   # 见下：默认装会带隔离属性、被拦
# 卸载无残留：brew uninstall --cask ninja（会带走 /Applications/Ninja.app）
# 拔 tap：brew untap ninja/local
```

## Gatekeeper / 隔离语义（无公证——2026-08-31 用户决策：不购 Developer ID）

实测结论（macOS 26.6.1 arm64，Homebrew 5.1.8；完整实测记录见
[../../docs/q4-evidence/](../../docs/q4-evidence/) 与 DISTRIBUTION.md）：

- 本 `.app` 用 **Apple Development** 身份签名（本机真实身份；无身份打包即
  失败，绝不 adhoc），**未经公证**。
- **Homebrew 5.1.8 对 file:// DMG 的 `brew install --cask` 也会给
  `/Applications/Ninja.app` 打 `com.apple.quarantine`**（实测 `0381;…`）。
- 带隔离属性时 `open` 被 Gatekeeper 拦：弹确认框、进程不启动
  （syspolicyd：scan `Code did not match any currently allowed policy`
  → `Prompt shown` → `denial breadcrumb`；`spctl -a -vv` = rejected）。
- 两条可用路径：
  - 安装时 `brew install --cask ninja --no-quarantine`
    （或 `HOMEBREW_CASK_OPTS="--no-quarantine"`，同效）→ 无隔离属性，直接可开；
  - 装完去属性：`xattr -dr com.apple.quarantine /Applications/Ninja.app`。
- 本机自打 DMG 手工拖拽安装（不经 brew）**不带**隔离属性，直接可开。
- 公开分发（他人下载）之前必须补 Developer ID + notarization——见
  DISTRIBUTION.md「公证与公开分发」。
