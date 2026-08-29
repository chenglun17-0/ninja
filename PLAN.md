# Ninja 实现计划

依据 [PRODUCT.md](PRODUCT.md)、[STACK.md](STACK.md)。Agent、市场、开源不在本计划内。

执行单位是**一个阶段**。每个阶段走固定 workflow：盘点 → 实施 → 独立验证。验证失败则停，不进入下一阶段。不要一次跑完整条链。同一阶段可以反复跑，直到「过」；p1、p2 预期会跑多次。

## 依赖

```text
p0 核与仓库
  └─ p1 单终端面（PTY + vt + Metal + IME）
        ├─ p2 标签/分屏/多窗口     → 空载内存门禁
        └─ p3 ADE 协议
              └─ p4 命中分发
                    └─ p5 文本预览插件 + 层
                          └─ p6 关掉即轻门禁（依赖 p2 + p5）
                                └─ p7 签名分发
```

`p2` 与 `p3–p5` 无文件冲突时可并行，但不要在同一 workflow 调用里并行——验证员抓不到跨面回归。要并行就开两次 workflow，cwd 隔离。

七个编号是门禁，不是七个均匀迭代。p1（把 vt 画成能用的终端）和 p2（日常壳 + 空载内存）各自比 p3–p6 加起来都大。不拆编号，是因为再切碎会变成「渲染一周、IME 一周、选区一周」，门禁对不齐产品。缺的日常能力补进 p1/p2 的「过」，不新开阶段。

不另开阶段的：搜索 UI、设置界面、图片预览、Agent、Linux、自动更新。那不是第一年门禁。

## 阶段

### p0 核与仓库

盘点：`libghostty-vt` 可钉的 commit、公开 C API 是否覆盖 grid / render state / 键鼠编码 / OSC；Zig 工具链。

做：Cargo workspace（`ninja` / `ninja-protocol` / `ninja-preview`）；钉 vt 库；Rust FFI 能链上；空 crate 能 `cargo test`。

过：宿主 crate 静态链接 `libghostty-vt`，不碰内部 `ghostty.h`；空载路径没有插件代码、没有 wasmtime。

### p1 单终端面

盘点：vt render state → Metal atlas 的最小闭环；AppKit IME；posix PTY。

做：一个窗口、一个面、一个 PTY。CoreText 出字形，Metal 画 cell。输入（含 IME）进 vt 编码再写入 PTY。选区、复制粘贴、剪贴板走宿主，不走插件。

过：打开即 bash；中文输入；resize reflow；滚动；选中复制、粘贴。单窗口内存有数，后面拿来对照 Ghostty。没有标签也可以，但不能是只能看不能选的演示画面。

### p2 标签 / 分屏 / 多窗口

盘点：AppKit 标签与分屏谁画、每个 pane 的 vt/PTY/视图生命周期。

做：空载宿主的完整壳。每个 pane 独立 PTY + vt + Metal 视图。菜单栏与宿主快捷键覆盖新建/关闭窗口、标签、分屏。默认 TOML（shell、字体、键位）可缺省文件启动。

过：**空载门禁。** 带标签和分屏的日常用法，内存仍与 Ghostty 同量级。失败则产品不成立，后面插件不准靠「先做功能再减内存」续命。

### p3 ADE 协议

盘点：消息的 JSON schema（v0 六类：hit/layer/input/spawn/config/theme）；未知字段 / 版本失败策略。

做：`ninja-protocol` 编解码与契约测试。宿主只在启用插件时听 Unix socket。空载不创建 socket、不拉进程。

过：消息有 `v` 和 `type`；插件遇不支持的 `v` 必须退出；第二个语言只靠文档能写出解码器（验证阶段用最小脚本证明，不进产品）。

### p4 命中分发

盘点：vt 网格上的路径 / URL / OSC-8 怎么读到 cell。

做：点击 → `hit`。有插件 `claim` 则交给插件；全 `ignore` 或未启用插件则系统默认打开。不弹「请安装插件」。

过：无插件时点路径行为与普通终端一致；有插件时命中事件字段够预览认领。

### p5 层 + 文本预览

盘点：IOSurface 合成到终端视图之上；焦点与 Esc。

做：`layer` 原语。`ninja-preview` 独立进程，只预览文本和代码。第一次点击才拉起。Esc 关层，焦点回终端。

过：**第一个插件门禁。** 只通过公开协议完成「点击路径 → 终端内看文本」。预览插件不链宿主内部 API。

### p6 关掉即轻

盘点：插件进程、socket、IOSurface、监督器的生命周期。

做：启用 → 用一次 → 禁用 / 卸载。杀掉残留，释放层。

过：**关掉即轻门禁。** 内存回到 p2 空载；无残留进程、无隐藏窗口。失败说明插件泄漏进了宿主。

### p7 签名分发

盘点：`.app` 布局、公证、配置路径、插件本地安装路径。

做：可安装的 macOS 应用。默认零插件。公证签名。插件本地目录可装、可卸。

过：别人能装上当日常终端用。仓库仍不公开。配置在 p2 已能工作，本阶段不补功能，只补分发。

## Workflow 合同

脚本：[workflows/ninja-implement-phase.js](workflows/ninja-implement-phase.js)

每次调用只传一个 `phase`（`p0`…`p7`）。三角色：

1. **盘点**：只读。对照本阶段「过」的标准写差距，不改代码。
2. **实施**：只做本阶段。禁止加 Agent、图片预览、市场、Linux、内部 `ghostty.h`。
3. **验证**：新会话，不看实施者的自我声明。按「过」的标准抓缺陷。有缺陷就停，不自动开下一阶段。

实施者与验证者不得同 thread。验证失败允许实施者修一轮，再验一次；第二次仍失败则 workflow 以 `ok: false` 结束。
