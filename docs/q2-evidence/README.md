# q2 配置系统取证（ninja-embed）

- 二进制：`cargo build --release -p ninja-embed && ./target/release/ninja-embed`
- 引擎：libghostty 钉点 a887df42（vendor/ghostty，zig 0.15.2；q2 增补丁 0002 装 themes 资源）
- 复跑：`./docs/q2-evidence/run-e2e.sh`（30 项断言，产物覆盖写入本目录；会临时移开
  真实用户配置 `~/Library/Application Support/com.mitchellh.ghostty/config` 并在退出时恢复）
- 验证纪律：全部断言跑命令/读 dump JSON 或像素探针，不信实施者 summary。

## 验收对照

| 验收项 | 证据 |
| --- | --- |
| ODP 缺省主题 | run-e2e A（隔离无用户配置）：a-dump.json `odp_applied=true`，background=#282c34、foreground=#abb2bf、ANSI16=ODP 钉值；像素实测 [40,44,52]（a-pixel.txt + shot-odp-default.png） |
| 用户既有配置常用子集直接生效（主题/字体/键位） | run-e2e B（真实配置不隔离）：theme=Dracula 探测→ODP 让位（b-dump.json `user_theme=true`），Dracula bg/fg/ANSI + font-size=18 全生效（具名主题经 `GHOSTTY_RESOURCES_DIR`→vendor/ghostty/out/share/ghostty/themes 解析，574 个主题含 Dracula）；像素 [39,42,54]≈#282a36（b-pixel.txt + shot-user-dracula.png） |
| 键位全量继承 ghostty | 菜单 keyEquivalent 全部由 `ghostty_config_trigger(action)` 推导（c-dump.json triggers 表）；菜单点击走 `ghostty_surface_binding_action`（与键位同一 action 路径）；ItemSpec 硬编码表已删。真键实测：⌘G 重绑 new_split:right 生效（C1/C2/C3，action_cb 收到 NEW_SPLIT）、改绑后旧键失效（C4/C5）、新动作 decrease_font_size 经绑定核心触发（C6，cols 39+39→47+47）、⌘T 默认 new_tab（C7） |
| ninja 特有动作进 ghostty keybind 系统 | 宿主层 `keybind = super+,=toggle_visibility`（认领空闲动作；ghostty 动作集封闭，Binding.zig 对未知动作名抛 InvalidAction——取证见下）；用户 `keybind = super+shift+p=toggle_visibility` + `super+,=ignore` 重绑实测：旧键 ⌘, 失效（D2）、新键 ⌘⇧P 开面板（D3，win 1→2）、TOGGLE_VISIBILITY action 到宿主 dispatch（D4，d.log `action tag=12`） |
| 热重载 | 两条路径都实测：① mtime 监视（改文件自动重载，C4/C6）；② ⌘⇧,（reload_config action，宿主重跑管线，C8）；`ghostty_app_update_config` 传播全部 surface：字号 13↔24 生效（C6 cols 变化）、背景色 #ff00ff 传播到像素（C10，c-pixel.txt [255,0,255]）；重载日志见 c.log「配置已重载」 |
| ninja.toml 收缩为宿主/插件特有 | run-e2e E：v1 终端项（shell/font-family/font-size/[theme]）与 [keys] 一律 stderr 警告并忽略（e.log；[keys] 语义不复活），[plugins]（enabled/paths）解析收下但 q2 不拉起（e-dump.json plugins_enabled=["preview","theme"] + E4 零插件进程） |
| 空载红线 | E4：无插件进程；ninja.toml 只解析不拉起（监督器 q3） |
| q0 取证模式回归 | F1：`--evidence-dir` report `overall: PASS` |

## 实施决策记录（plan 里的两个决策点）

1. **主题资源（plan 第 5 点，选 a）**：vendored 补丁
   `vendor/ghostty/patches/0002-install-themes-on-embed-route.patch` 把钉版
   iterm2_themes（build.zig.zon 钉哈希）装到 `out/share/ghostty/themes`，
   宿主 build.rs 烘路径、`ghostty_init` 前设 `GHOSTTY_RESOURCES_DIR`
   （resourcesdir.zig 非 Debug 构建读环境变量）。用户 theme=Dracula 因此
   真实生效（B 组证据），不是子集外。
2. **ninja 特有动作的宿主接入（plan 第 4 点，选 a）**：认领 ghostty 空闲
   动作 `toggle_visibility` 绑 ⌘,（embedded apprt 对它无特殊处理，全量
   转发宿主 action_cb——embedded.zig performAction 只短接 set_title 与
   config_change 的克隆）。备选（surface_key_is_binding 后宿主自比对）未采用。
3. **ODP 与 theme= 的装载次序（盘点 plan 之外的必要决策）**：ghostty
   finalize 的 loadTheme 会把已装载配置重放在主题之上（Config.zig
   loadTheme）——ODP 层若在用户设了 theme= 时装载会反压用户主题。宿主在
   装载前扫描用户默认文件 + config-file 链（config.rs collect/user_sets_theme），
   有 theme= 则跳过 ODP 层（B1 实证）。行级扫描，`$if` 条件块内 theme=
   也算设置（宁缺 ODP 不压主题）。
4. **App Support 路径必须同源解析**：ghostty 的 macOS App Support 路径走
   NSFileManager/NSSearchPath，**不随 HOME env 变**（实测）——宿主侧扫描
   用同一 API（config.rs macos_app_support_dir），否则 theme 探测会扫错文件。
   E2E 隔离因此用「临时移开真实配置」而非 HOME 覆盖。

## 本机环境事实（E2E 方法论）

- **焦点受限**：E2E 由 Orca harness 驱动，activate 后焦点被 Orca 抢回
  （keyWindow 常空）。菜单键等价物路径（performKeyEquivalent → binding_action）
  不依赖窗口 key 态，实测稳定；宿主另在无 key 窗口时兜底取活面
  （host.rs current_surface_view）。
- **⌘T/⌘N 合成键被系统吞掉**：无菜单匹配时这两个键的合成/系统级投递事件
  到不了应用 keyDown（⌘W/⌘G/⌘, 正常；tabbingMode=disallowed 也不变）。
  有菜单等价物时正常（C7）。真实用户按键是否同此未验证——键位继承的取证
  用 ⌘G/⌘⇧P 等普通键完成。
- **像素探针走显示色空间**：screencapture 读数相对 sRGB 有 ~10% 系统性压暗
  （同一显示恒定）。绝对色断言容差 ±10（A5/B5 实测恰好命中），颜色传播
  断言用强对比色 #ff00ff（C10 读数恰 [255,0,255]）。
- **copy/paste 菜单无快捷键显示**：ghostty 默认 ⌘C/⌘V 绑定带 performable
  旗标，Trigger.Set 不为 performable 绑定建反向映射（getTrigger 返空，
  Binding.zig putFlags track_reverse）——菜单不显示但键仍经 surface_key
  运行时判定执行，不被菜单拦截（语义更正确）。

## 文件

- `run-e2e.sh`：可复跑驱动（30 断言）。
- `a/b/c/d/e-dump.json`：NINJA_CFG_DUMP 生效配置快照（色值/字号/trigger 表/
  ODP 决策/监视集/plugins）。
- `a/b/c-pixel.txt`：窗口像素探针读数。
- `c-zoom.json`：C 组末态 zoom dump（布局/网格）。
- `*.log`：各次运行的完整宿主日志（NINJA_Q1_DEBUG 决策链）。
- `shot-odp-default.png` / `shot-user-dracula.png`：ODP 缺省与用户 Dracula 配置的窗口截图。
