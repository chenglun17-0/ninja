# tools/：本地钉版工具链

本目录被 `.gitignore` 忽略（`README.md` 除外），存放按需钉版的工具链。

## zig 0.15.2

产品宿主通过 vendored libghostty 嵌入（钉 ghostty commit
`a887df42c56f6de86c0fe6da9c4eeca37931e083`，1.3.2-dev）构建终端核，
该钉点要求 `minimum_zig_version = "0.15.2"`；ghostty HEAD 已要求 0.16，
故 brew stable（0.16.x）不可用于钉点构建。安装方式：

```sh
curl -L -o zig-0.15.2.tar.xz \
  "https://ziglang.org/download/0.15.2/zig-aarch64-macos-0.15.2.tar.xz"
tar xJf zig-0.15.2.tar.xz && rm zig-0.15.2.tar.xz   # -> zig-aarch64-macos-0.15.2/
```

本机 `/usr/local/bin/zig` 曾指向损坏的 Homebrew Intel 前缀 zig 0.13（缺
`libz3.4.13.dylib`，`zig version` 即 abort）。该 Homebrew zig（0.13.0 与一个
2024 年的 HEAD 构建）已整体卸载，现在 `/usr/local/bin/zig` 是指向本目录钉版
的符号链接：

```sh
ln -sf "$PWD/zig-aarch64-macos-0.15.2/zig" /usr/local/bin/zig
zig version   # 0.15.2
```

注意 PATH 顺序：`/opt/homebrew/bin` 在 `/usr/local/bin` 之前。因此本仓库
**不要** `brew install zig`（0.16 会反向遮蔽钉版，vendored 构建会用到错的
zig）。macOS 之外或别的机器同理：保证 `zig version` 是 0.15.x 即可。

首次 `cargo build/test` 会联网 clone ghostty 钉点并拉 zig 依赖
（离线场景可用 `GHOSTTY_SOURCE_DIR` / `GHOSTTY_ZIG_SYSTEM_DIR` 预置）。

## verify/ 取证脚本（本地不入库）

`tools/verify/` 在 `.gitignore` 里（同目录惯例：synth_input.swift /
shot_text.swift 也是本地脚本），但多个 `NINJA_E2E=1` 门控的 E2E 依赖
`tools/verify/synth_input.swift`（cmdw_surface_close / ctrl_c_interrupts /
layer_preview / off_is_light）；X2 起标题栏像素回归（titlebar_theme）
与 theme_switch 的标题栏采样依赖 `tools/verify/shot_window.swift`：
按 PID 找 CGWindowID → `screencapture -l<wid>` 截整窗（含标题栏）→
采样相对矩形平均色（标题栏不是 Metal drawable，`NINJA_DUMP_DRAWABLE`
探不到）。需要运行终端有屏幕录制授权（TCC）。脚本随仓库分发的脚本
在仓库里没有备份：新机器参照本文件与各测试头部注释重建，或从原机器
拷贝 `tools/verify/` 目录。

## D-C 渲染跳帧取证脚本（p6 后定点修复）

三个可复跑的 CPU 取证脚本（修前/修后用同一脚本同一时长对比）：

```sh
./tools/cpu_pressure_probe.sh <标签> [秒]     # yes 式全速输出（滚动型 Full 帧）
./tools/spinner_pressure_probe.sh <标签> [秒] # \r 重写型输出（进度条类 Partial 帧）
./tools/idle_cpu_probe.sh [秒]                # 空闲红线：稳态 CPU 必须为 0
```

读数是 `ps -o cputime=`（宿主进程累计 CPU 时间）。注意：debug 构建下
`yes` 洪峰的 CPU ~95% 在 vendored zig 库 `vt_write` 的 debug 完整性校验
（`PageList.grow → Page.verifyIntegrity`，`sample` 取证），渲染路径占比
<5%，所以该项修前修后基本持平——D-C 的 renderer 收益要看 spinner 项
（Partial 帧单行解码）与 idle 项（Clean 帧零提交）。帧级计数取证用
`NINJA_FRAME_STATS=<path>` 环境变量（宿主周期落盘 drawn/skipped）。
