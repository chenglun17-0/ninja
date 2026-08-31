# q3 取证：插件系统 + 三门禁

复跑：`./docs/q3-evidence/run-e2e.sh`（需 GUI 会话 + TCC 辅助功能/屏幕录制；全程虚拟屏，不落主屏）。

- 结果：**47 断言全绿两轮**（`e2e-summary.txt`、`e2e-summary-run{1,2}.log`），OVERALL: PASS。
- 回归：q1 套件 45 断言全绿（`regression-q1.log`）；q2 套件 38 断言全绿（`regression-q2.log`，[plugins] 断言按 q3「启用即拉起」语义更新，见下）；q0 取证模式 PASS（`d-report.txt`，脚本 D 段内跑）。
- 日志：`e2e-logs/`（宿主/插件 stderr 全量，含 `NINJA_ADE_DEBUG` 的逐帧链路）。

## 三门禁对照

| 门禁 | 断言 | 证据 |
| --- | --- | --- |
| 一 空载内存 | ninja 空载（默认零插件、单窗）vs Ghostty 本尊 1.3.0，各 15 样本 `proc_pid_rusage ri_phys_footprint` 中位数，**比值 ≤1.5×** | `footprint-idle.txt`：ninja 92.7MB / ghostty 169.6MB = **0.547×**（嵌入静态链反而比本尊轻——本尊是完整 Swift app）；空载红线 lsof unix socket=0、pgrep 插件空（A1/A2） |
| 二 第一个插件 | 虚拟屏真鼠标（全局 HID tap）⌘+click 终端内路径 → hit 广播 → claim → 层出现（像素验层内容含文件文本）→ Esc 关层焦点回终端 | B1-B14：`b.log` 逐帧（`hit id=…kind=Path`、`claim priority=100`、`layer.open → ready …iosurface=`、`layer.present`、`Esc 关层`）+ `b-layer.png`/`b-layer-pixel.txt`（层背景 #282c34±14、正文带 std 33/34/35 = 密集字形）+ 关层后像素回终端背景、Esc 后键入 23 字符全走 surface_key 通道 |
| 三 关掉即轻 | 禁用（面板钩子）/杀插件/SIGKILL 宿主三场景：socket 消失、无插件进程、无层、footprint 回空载基线、再启用即重拉 | C10-C22：面板 off → 插件已禁用日志 + socket 删除 + pgrep 空；杀 ninja-preview → EOF 收层（像素回终端背景）；杀 ninja-theme → 色板回退（像素回 #3a2a5b）；SIGKILL 宿主 → 插件 EOF 自退 + 陈旧 socket 下次启动被清扫；C14 footprint 回基线（98.6MB ≤ idle+8MiB）；C15 on 即重拉 |

## 协议契约（0 段）

- `cargo test -p ninja-protocol`：往返 / golden 17 文件字节钉死 / 信封不变量 / 宿主 lenient 与插件版本门（v=1 → 退出码 78）/ 帧层（半帧/背靠背/超限/空载荷）/ 第二语言（Python 参考解码器）。
- golden 与旧树（1240428）**字节一致**——线格式稳定，按 PRODUCT/PLAN 重钉未改语义。
- 依赖红线：`tree-ninja-preview.txt`/`tree-ninja-theme.txt`——两个示例插件只依赖 `ninja-protocol` + 系统框架，无宿主 crate、无 ghostty-sys。
- 宿主单测（`e2e-logs/host-unit.log`）：socket 级集成（python3 最小插件真实连接）——claim/ignore 仲裁、坏协议断连、禁用回收、陈旧 socket 清扫、空载零 socket。

## 实施决策（q3 特有的坑与取舍）

1. **hit 双数据源，链接源是路径主源**：ghostty 的 ⌘+click 链（URL 匹配器 + `resolvePathForOpening`）会把路径 token 解析成**绝对路径**再送 `OPEN_URL` action——宿主在 action 分发接管，无 scheme 的载荷归 `path`（ninja-preview 只认领 path）。网格源（`read_text` 行读取 + token 展开）做无链接命中时的兜底。⌘+hover 修饰语义、`link-previews` 门控、`config_get(link-previews)` 回读怪象全部留在 ghostty 内核与适配器（`plugins.rs` 的 `classify_url`/`handle_grid_hit`），不进协议。
2. **层合成走「present 拷贝」**：三条路实测——(a) `layer.contents = IOSurfaceRef`：宿主 layer-hosting 树（ghostty Metal CAMetalLayer 是 view.layer）不渲染（宿主直写不透明像素也不显示）；(b) sublayer 挂 Metal 层：几何不随 view 坐标系走（实测位置漂移）；(c) **LayerView（layer-backed NSView）drawRect 画 CGImage**：稳定可见。故 v0 的 present = lock → 拷 BGRA 字节 → CGImage → 重画（每帧一次 ~0.5MiB CPU 拷贝）；协议面不变（插件仍写 IOSurface，跨进程共享照旧，`b.log` 的 `iosurface=` 是 global id）。另：flipped 视图的 CG 上下文对原生 CG 调用仍是 y-up，`CGContextDrawImage` 需先翻 CTM（实测倒置）。
3. **theme 层文件次序**：无 `config_set`（q0 审计 #5）→ `theme.set` 校验后写 `${TMPDIR}/ninja-{pid}/plugin-theme.conf`，装载序 = 用户文件**之后**、finalize **之前**——finalize 的 `loadTheme` 重放会把这层压顶（q2 已证的机制反向利用）；插件死亡/禁用删层重载回 ODP/用户基线（C4-C6）。`divider` 无 ghostty 对应键（宿主分隔条色由 background 派生），协议保留字段不落地；`selection_alpha` 按不透明度合成进 `selection-background`（RGB 键）。
4. **footprint 采样坑（rusage 缓冲尺寸）**：内核按**当前** flavor 完整结构体写穿——本机实测比 v6（264B）更宽，264B 缓冲在进程退出期触发 stack-protector abort（`footprint_sampler.c` 复现）；宿主 `footprint_bytes` 与采样器都固定 512B 缓冲，`ri_phys_footprint` 偏移 72（ABI 钉死）。
5. **多插件泵等待窗**：拉起后的「等首个连接」窗口**不**因首个连接到达而提前关闭——多插件会话里第一个连上就关窗，较慢的插件连接会卡在 listen backlog 里直到首次点击才被消化（theme.set 晚了几十秒，实测踩过）；窗口只按 5s 时间过期。
6. **E2E 虚拟屏的建面次序**：`NINJA_E2E_SCREEN` 落位必须在 `attach_surface` **之前**——建面首推 `push_size` 读窗口所在屏的 backingScale，先建面再移屏（主屏 2x → 虚拟屏 1x）会让 surface 记账与视图 points 错位（渲染挤压 + 底部暗带 + 网格/像素换算全漂，实测）。另 hit 的像素→cell 换算用**网格占比**（视口 bounds ÷ 网格行列）而非 CELL_SIZE px，免疫跨屏 scale 漂移。
7. **键盘探子与用户 shell**：E2E 键盘就绪探子按 q2 惯例打 'z'——但用户 zsh 装了 `z` 跳转命令，探子后回车会打出整屏目录表把 echo 输出顶出首屏；改退格清场。B14 的「焦点回终端」用日志证据（Esc 后键入全走 `surface_key` 通道 = 层前台路由已退出），zsh autosuggestion ghost 文本让 zoom dump 的 last 不可靠。
8. **面板不夺焦**：插件面板 `orderFrontRegardless`（附属工具窗）——`makeKeyAndOrderFront` 会把 zoom 取证钩子（按 keyWindow 找终端容器）打断（q2 回归实测）。
9. **spawn/config 不接线（防镀金）**：协议面保留 17 型消息与完整契约测试；宿主对 `spawn.*` 记 debug 忽略——q3 验收点名 hit/layer/input/theme.set，ninja-preview 自读文件、ninja-theme 只推色板，都不需要宿主代拉进程。
10. **q2 套件的两处语义更新**：`[plugins]` 从「只解析不拉起」升级为「启用即拉起」——E2c 提示语、E4/E5 断言改为监督器语义（本段 fixture 的名字/路径解析不到二进制 → 断言「降级为未启用」边界；真实拉起/回收链在 q3 B/C 段取证）。

## 环境事实

- Ghostty 本尊 `/Applications/Ghostty.app`（1.3.0）；宿主嵌入 libghostty `a887df42`（1.3.2-dev）。
- 虚拟屏 `scripts/e2e/virtual-display hold 1440 900 0`（hidpi=0，像素 1:1，**无显示色空间压暗**——q2 在主屏见过 ~10%，q3 像素断言用原始值 ±14）。
- 窗口 chrome：titlebar 32pt（窗高-内容高实测）；zsh 多行 prompt 下 `echo` 输出在 ~第 3 行（点击行梯子 3/4/2/5/6 兜底）。
- 工具：`synth_input.swift`（q1 版 + 鼠标修饰键 + ':' 键码）、`probe_window.swift`（q2 版 + 方差模式）、`footprint_sampler.c`（新，含缓冲尺寸坑）。

## 产物清单

- `run-e2e.sh`（取证脚本）/ `e2e-summary.txt` / `e2e-summary-run{1,2}.log` / `regression-q{1,2}.log`
- `e2e-logs/`：宿主/插件全量日志（contract/host-unit/idle/b/c/c2/d/ghostty）
- `footprint-idle.txt`（门禁一原始数据）；`b-*.png`/`c-*.png` 截图与 `*-pixel.txt` 探针记录；`b-grid.json`/`b-focus.json`/`c-grid.json`/`{b,c}-dump.json`（生效配置含 `plugin_theme` 字段）；`d-report.txt`（q0 回归）
- `tree-ninja-{preview,theme}.txt`（依赖红线）
