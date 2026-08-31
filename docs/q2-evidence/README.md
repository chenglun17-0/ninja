# q2 配置系统取证（ninja）

- 二进制：`cargo build --release -p ninja && ./target/release/ninja`
- 引擎：libghostty 钉点 a887df42（vendor/ghostty，zig 0.15.2；q2 增补丁 0002
  装 themes 资源到 `out/share/ghostty/themes`，574 个主题含 Dracula/One Dark*）
- 复跑：`bash docs/q2-evidence/run-e2e.sh`（38 项断言，产物覆盖写入本目录；
  全程虚拟屏；会临时移开真实用户配置 `~/Library/Application
  Support/com.mitchellh.ghostty/config{,.ghostty}` 并在退出时恢复）
- 验证纪律：全部断言跑命令/读 dump JSON/读像素探针，不信实施者的自我声明。

## 验收对照（PLAN q2「过」：用户既有 ghostty 配置的常用子集直接生效；主题/字体/键位实测）

| 验收项 | 证据 |
| --- | --- |
| ODP 缺省主题 | run-e2e A（隔离无用户配置）：a-dump.json `odp_applied=true`，background=#282c34、foreground=#abb2bf、ANSI16=ODP 钉值；像素实测 [38,42,49]（a-pixel.txt + shot-odp-default.png，期望 [40,44,52]±10） |
| 用户既有配置常用子集直接生效（主题/字体/键位） | run-e2e B（macOS 正宗位置 App Support 写用户配置）：theme=Dracula 探测 → ODP 让位（b-dump.json `user_theme=true`），Dracula bg/fg/ANSI + font-size=18 + keybind 重绑 ⌘⇧O→new_split:right 全生效（具名主题经 `GHOSTTY_RESOURCES_DIR`→vendor themes 解析）；像素实测 [40,42,54]=#282a36（b-pixel.txt + shot-user-dracula.png） |
| 键位全量继承 ghostty | 菜单 keyEquivalent 全部由 `ghostty_config_trigger(action)` 推导（dump triggers 表）；菜单点击走 `ghostty_surface_binding_action`（同一 action 路径）；q1 ItemSpec 硬编码表已删。真键实测：⌘G 重绑 new_split:right 生效（C1/C2/C3）、mtime 重载后旧键失效（C4/C5）、新动作 decrease_font_size:2 经绑定核心触发（C6 cols 85→88）、⌘T 默认 new_tab 经菜单等价物→binding_action→NEW_TAB（C7/C7b） |
| ninja 特有动作进 ghostty keybind 系统 | 宿主层 `keybind = super+,=toggle_visibility`（认领空闲动作；ghostty 动作集封闭，Binding.zig 对未知动作名抛 InvalidAction）。用户 `keybind = super+shift+p=toggle_visibility` + `super+,=ignore` 重绑实测：旧键 ⌘, 失效（D2）、新键 ⌘⇧P → TOGGLE_VISIBILITY action 到宿主 dispatch（D3）、binding_action 直驱同途（D4）。面板 UI 是 q3 交付（q2 dispatch 记日志，不建面板/不拉插件运行时） |
| 热重载 | 两条路径都实测：① mtime 监视（改文件自动重载，C4/C6——ghostty 内建监视与宿主轮询双路汇到同一 reload）；② ⌘⇧,（reload_config action，zoom 钩子 reloadcfg 同途，C8）；`ghostty_app_update_config` 传播全部 surface（CONFIG_CHANGE 回调，C9b）：字号 24→20 生效（C8）、背景色 #ff00ff 传播到像素 [255,0,255]（C10，c-pixel.txt）；重载日志见 c.log「配置已重载」 |
| ninja.toml 收缩为宿主/插件特有 | run-e2e E：v1 终端项（shell/font-family/font-size/[theme]）与 [keys] 一律 stderr 警告并忽略（E1/E2/E2b；[keys] 语义不复活），[plugins]（enabled/paths）解析收下但 q2 不拉起（E2c/E3 + E4 零插件进程 + E5 零 unix socket） |
| q0 取证模式回归 | F1/F2：`--evidence-dir` report `overall: PASS`（f-report.txt） |

## 实施决策记录

1. **主题资源**：vendored 补丁 `vendor/ghostty/patches/0002-install-themes-on-embed-route.patch`
   （~16 行 build.zig：`emit_themes`（默认 true）时把钉版 iterm2_themes
   （build.zig.zon 懒依赖）装到 `<prefix>/share/ghostty/themes`），crates/ninja
   build.rs 烘路径、`ghostty_init` 前设 `GHOSTTY_RESOURCES_DIR`（已设不动；
   resourcesdir.zig 只在 init 读一次）。用户 theme=Dracula 因此真实生效
   （B 组），不是色键子集外。
2. **ODP 与 theme= 的装载次序**：ghostty finalize 的 loadTheme 会把已装载
   配置重放在主题之上（Config.zig _replay_steps）——ODP 层若在用户设了
   theme= 时装载会反压用户主题。宿主在装载前扫描用户默认文件 + config-file
   链（config.rs collect/user_sets_theme，行级扫描含 `$if` 块，宁缺 ODP 不压
   主题），有 theme= 则跳过 ODP 层（B1 实证）。
3. **App Support 路径必须同源解析**：ghostty 的 macOS App Support 路径走
   NSSearchPath，不随 HOME env 变（实测）——宿主侧扫描用同一 API
   （config.rs macos_app_support_dir）。E2E 隔离用「临时移开真实配置」而非
   HOME 覆盖。
4. **C API 无 config_set**：程序化注入（宿主层/ODP 层）走生成文件
   （`/tmp/ninja-{pid}/{host,odp}.conf`，dump 的 layer_dir 可见），
   `ghostty_config_load_file` 装载，q0 审计 #5 同路径。
5. **ghostty 动作语义**：`decrease_font_size` 必须带步长参数
   （`decrease_font_size:2`），无参写法 InvalidFormat 进诊断（首跑实测，
   c.log 诊断行为证据）；copy/paste 默认绑定带 performable 旗标，Trigger.Set
   不为 performable 绑定建反向映射（getTrigger 返空）→ 菜单不显示 ⌘C/⌘V
   但键仍经 surface_key 运行时判定执行，不被菜单拦截（已知语义）。
6. **热重载双监视汇流**：ghostty embedded 自带配置文件监视（文件变化发
   RELOAD_CONFIG action，c.log tag=47），宿主另有 0.5s mtime 轮询
   （config-file 链 + ninja.toml；ghostty 内建监视不含 ninja.toml）——两路
   都进 `host::schedule_reload`（去重），重跑全量管线后
   `ghostty_app_update_config` 传播。

## q0 审计遗留：link-previews 回读怪象（顺带记录，不阻塞）

q0 审计 #2 记录：`config_get(link-previews)` 对 app 级句柄回读 false，但
surface 层 hover 动作实际放行。q2 复查（同一二进制）：

- q0 取证路径（`config_new → load_default_files → load_file(显式 true) →
  finalize`，q0_demo.rs load_config）：回读仍 **false**（/tmp/nq2-f/demo.log
  `config link-previews = false`，F 组同跑产出；同文件里 background/
  font-size 显式值都生效——非「defaults 后 load_file」整体失效）。
- q2 管线路径（用户配置经 load_default_files 装载）：默认与显式
  `link-previews = true` 回读均 **True**（a-dump.json / 手测 dump）。

即怪象与装载次序相关（显式文件在 defaults 之后时该键回读失真），钉点
libghostty 内部对 link-previews 的拷贝/解析未再深挖；hit 路径不受影响
（q0 report hyperlink-hover PASS、F 组回归 PASS）。留档，q3 需要时再查。

## 本机环境事实（E2E 方法论）

- **像素探针**：`screencapture -x -l <windowID>` + probe_window.swift（q0
  pixel-sample 同款位图采样）。实测窗口**底部 ~15% 是暗带**（surface 几何
  伪影，两配置同在）、顶部有 prompt/标题栏——采样区取终端中部
  （0.55..0.95 × 0.30..0.50）读数与配置色严格一致（B/C 组读到精确值）。
- **虚拟屏色彩**：虚拟屏上读数与 sRGB 标称值偏差在 ±3 内（A 组 [38,42,49]
  vs [40,44,52]），无旧树主屏时代的 ~10% 压暗；容差仍取 ±10。
- **键盘**：合成键经 CGEventPostToPid（q1 synth_input）；菜单键等价物路径
  （performKeyEquivalent → binding_action）不依赖窗口 key 态，实测稳定。
- **macOS bash 3.2 多字节坑**：`$var，`（裸变量名紧跟全角字符）触发
  unbound variable——脚本里此类位置一律 `${var}`（q2 首跑实测）。

## 文件

- `run-e2e.sh`：可复跑驱动（38 断言，A-F 六组）。
- `probe_window.swift`：窗口 PNG 相对区域平均 RGB 探针（JSON 输出）。
- `a/b/c/d/e-dump.json`：NINJA_CFG_DUMP 生效配置快照（色值/字号/trigger
  表/ODP 决策/监视集/link-previews 回读/plugins）。
- `a/b/c-pixel.txt`：像素探针原始 JSON（采样窗口中部空背景区）。
- `shot-odp-default.png` / `shot-user-dracula.png`：ODP 缺省与用户 Dracula
  配置的窗口截图（screencapture -l）。
- `c-zoom.json`：C 组末态 zoom dump（布局/网格）。
- `f-report.txt`：q0 取证模式回归报告（overall: PASS）。
- `e2e-logs/`：各次运行的完整宿主日志（NINJA_Q1_DEBUG 决策链）。
- `e2e-summary.txt`：PASS=38 FAIL=0 / OVERALL: PASS。
