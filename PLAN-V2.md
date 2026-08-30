# Ninja v2 实现计划：转投 libghostty 嵌入

依据 [PRODUCT.md](PRODUCT.md)、[STACK.md](STACK.md)（含 v2 修订）。
本计划取代 [PLAN.md](PLAN.md)（v1，p0–p7 已全部完成，作为历史保留）。

## 决策记录（2026-08-30）

用户产品决策：**底层全量转投 libghostty 嵌入 API（cmux 路线）**——引擎/渲染/
PTY/配置全套用 Ghostty；ninja 的价值收敛为 ADE 开放插件协议宿主。
cmux（manaflow-ai/cmux）为可行性存在证明；嵌入 API 官方声明 pre-1.0、
预期破坏性变更，钉 commit 升级显式做。

**命门风险**（q0 硬门禁）：①嵌入 API 能否读网格/hyperlink（hit 的数据源）；
②插件层能否合成到 surface 之上。审计不过 → ok:false 停下回报，不硬做。

## 保留 / 退役

- **保留**：ninja-protocol（协议）、ninja-preview / ninja-theme（插件，进程外
  JSON 与引擎无关）、插件监督器与面板、p2 的 AppKit 壳布局经验、分发脚本。
- **退役**：自研引擎层（font/atlas/renderer/term/pty/view，p1 资产，
  q4 打 tag 后移除主干）。

## 阶段

### q0 引擎底座与能力审计

做：钉版构建 libghostty（vendored，含 `include/ghostty.h` 嵌入 API）；zig
工具链适配（钉 commit 要求的版本）；Rust FFI（bindgen，宿主链入）；最小
嵌入——一个 AppKit 窗口挂一个 surface，跑 bash、能输入能渲染。

过：嵌入 surface 真渲染真 PTY；**能力审计报告**逐项给出「有 API / 无 / 绕法」：
网格与 hyperlink 读取、屏幕快照、surface 之上合成层、配置加载与运行时改、
键位拦截。hit 数据源无且无绕法 → 停（ok:false）。

### q1 壳重接

做：surface 的 window/tab/split 上下文回调接现有多窗/标签/分屏布局树；
焦点/关闭/resize 全链；面板入口不变。

过：标签分屏日常用法在嵌入引擎上成立；⌘W/⌘⇧Enter 等既有语义保持。

### q2 配置系统 ✅（2026-08-31，证据 docs/q2-evidence/）

做：加载 Ghostty 配置（含主题、字体、键位）+ 热重载；ninja.toml 收缩为
宿主/插件特有（plugins、spawn、面板）；ODP 为缺省主题。

过：用户既有 ghostty 配置的常用子集直接生效；主题/字体/键位实测。

实施定案（详见 docs/q2-evidence/README.md）：键位全量继承 ghostty
（菜单 keyEquivalent 由 config_trigger 推导、点击走 surface_binding_action，
ItemSpec 平行键位层已删）；ninja 特有动作（插件面板）认领空闲动作
toggle_visibility 绑 ⌘,（ghostty 动作集封闭，自定义动作名不可用）；
主题资源随宿主分发（vendored 补丁 0002 + GHOSTTY_RESOURCES_DIR，
574 主题含 Dracula）；ODP 层在用户设 theme= 时让位（finalize 的
loadTheme 会把已载配置重放在主题之上）；热重载双路径（mtime 监视 +
⌘⇧, reload_config action）→ ghostty_app_update_config 传播全部 surface。

### q3 ADE 重接 + 三门禁重跑

做：hit（按 q0 审计结论的路径）、layer（合成到 surface 上方）、
input、theme.set→Ghostty 配置动态改；三插件只走公开协议不动。
关掉即轻语义对嵌入引擎重验。

过：**三大门禁全部重跑通过**（空载内存对照 Ghostty 本尊、第一个插件、
关掉即轻）；157 测试基线中等价项全绿。

### q4 分发与退役

做：打包脚本适配（引擎动态/静态链接、体积）；DMG 重出；v1 引擎层
打 tag `v1-engine` 后移除；文档同步（STACK/PRODUCT/DISTRIBUTION）。

过：安装即日常可用；仓库描述与实际一致。

## Workflow 合同

同 PLAN.md 三角色（盘点 → 实施 → 独立验证，验证失败修一轮再验，
第二次仍失败 ok:false 停）。每次调用只传一个 `phase`（`q0`…`q4`）。
