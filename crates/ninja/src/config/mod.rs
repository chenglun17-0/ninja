//! q2 配置系统：ghostty 配置装载管线 + ninja.toml 收缩 +
//! 键位全量继承（trigger→菜单换算）+ 热重载（文件监视）+ 取证 dump。
//!
//! ## 装载管线（键位/主题/字体全部走 ghostty 配置）
//!
//! [`load_pipeline`]：
//!
//! 1. `config_new`（ghostty 默认值，含默认键位表）；
//! 2. `load_file` 宿主层（[`HOST_LAYER_FILE`]，恒装载）：ninja 特有动作
//!    （插件面板）认领 ghostty 的空闲动作 `toggle_visibility` 绑 ⌘,，
//!    用户随后可用普通 `keybind = …=toggle_visibility` 重绑——ghostty
//!    的 Action 联合是封闭的（Binding.zig 对未知动作名抛 InvalidAction，
//!    vendored 测试 "parse: action invalid"），自定义动作名进不了
//!    keybind 配置，认领空闲动作是唯一让宿主动作进 ghostty 键位系统的路；
//! 3. `load_default_files`（XDG + macOS App Support，bundle_id 钉
//!    com.mitchellh.ghostty → 读用户既有 ~/Library/Application
//!    Support/com.mitchellh.ghostty/config）。空配置 = libghostty 默认
//!    色板（与原版 Ghostty 相同，不垫 One Dark Pro）；
//! 4. `load_recursive_files`（`config-file=` 包含链）；
//! 5. 可选插件主题层（`theme.set`，压用户文件之后、finalize 之前）；
//! 6. `finalize`（具名 `theme=` 在此装载并压顶）；
//! 7. 诊断打印（diagnostics_count/get_diagnostic → stderr）。
//!
//! C API 只能从文件装载（无 config_set，q0 审计 #5 实测），程序化注入走
//! 生成文件。层文件写在 `{{tmp}}/ninja-{pid}/`，取证可见。
//!
//! ## 主题资源
//!
//! 具名主题只从 `<XDG>/ghostty/themes/` 或 `<resources>/themes/` 解析；
//! 嵌入构建的资源目录 = `GHOSTTY_RESOURCES_DIR`（resourcesdir.zig，
//! ReleaseFast 生效，ghostty_init 读一次）。vendored 补丁 0002 把钉版
//! iterm2_themes 装到 `vendor/ghostty/out/share/ghostty/themes`，宿主在
//! `ghostty_init` 前设 `GHOSTTY_RESOURCES_DIR` 指过去
//! （[`ensure_resources_dir`]，build.rs 烘焙路径，已设的环境变量不动）。
//!
//! ## ninja.toml 收缩（宿主/插件特有）
//!
//! v1 的 shell/font-family/font-size/[theme] 终端项与 [keys] 键位段不再
//! 属于宿主：终端项走 ghostty 配置，[keys] 平行键位层语义不复活。q2
//! schema 只收 `[plugins]`（enabled/paths），只解析不拉起（监督器是 q3；
//! 空载零插件进程/零 socket 不变）。出现收缩掉的键一律 stderr 警告并忽略
//! （[`parse_host_config`]）。
//!
//! ## 热重载
//!
//! 宿主自监视（NSTimer 轮询 mtime，[`WatchState`]）：默认路径 4 个 +
//! `config-file=` 递归链 + ninja.toml，任一变化 → 重跑装载管线 →
//! `ghostty_app_update_config`（embedded 克隆新配置并传播全部 surface，
//! 回 CONFIG_CHANGE action）→ host 刷新派生态（bg/焦点环/窗口 chrome、
//! 菜单键位重建）。ghostty 默认键位 ⌘⇧,（reload_config action）同途。
mod ghostty;
mod host;
mod theme;

pub use ghostty::*;
pub use host::*;
pub use theme::*;

use std::path::PathBuf;
use std::time::SystemTime;

pub const PLUGIN_THEME_LAYER_FILE: &str = "plugin-theme.conf";

/// vendored 构建烘进来的 ghostty 资源目录（含 themes/；无则空串）。
pub const BAKED_RESOURCES_DIR: &str = env!("NINJA_GHOSTTY_RESOURCES_DIR");

/// 宿主层文件名（恒装载：ninja 特有动作的键位认领）。
pub const HOST_LAYER_FILE: &str = "host.conf";

/// 菜单镜像的 ghostty 动作名（keyEquivalent 全部由
/// `ghostty_config_trigger(action)` 推导；键位单一来源 = ghostty keybind）。
/// 顺序即 dump 顺序。
pub const MENU_ACTIONS: &[&str] = &[
    "quit",
    "toggle_visibility",
    "new_window",
    "new_tab",
    "close_surface",
    "new_split:right",
    "new_split:down",
    "toggle_split_zoom",
    "goto_split:left",
    "goto_split:right",
    "goto_split:up",
    "goto_split:down",
    "goto_split:previous",
    "goto_split:next",
    "previous_tab",
    "next_tab",
    "copy_to_clipboard",
    "paste_from_clipboard",
    "select_all",
];

// ---------------------------------------------------------------------------
// 热重载监视（mtime 轮询）
// ---------------------------------------------------------------------------

/// 配置文件集的 mtime 快照（None = 文件不存在）。
#[derive(Clone, Debug, Default)]
pub struct WatchState {
    mtimes: Vec<Option<SystemTime>>,
}

pub fn snapshot_watch(files: &[PathBuf]) -> WatchState {
    WatchState {
        mtimes: files
            .iter()
            .map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
            .collect(),
    }
}

impl WatchState {
    /// 与当前文件集比较（集合本身变化也算变化）。变化时更新快照。
    pub fn changed(&mut self, files: &[PathBuf]) -> bool {
        let now = snapshot_watch(files);
        let diff = now.mtimes != self.mtimes;
        if diff {
            self.mtimes = now.mtimes;
        }
        diff
    }
}

// ---------------------------------------------------------------------------
// 单元测试（纯函数）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::ghostty::line_config_file;
    use super::*;
    use ghostty_sys::*;

    #[test]
    fn host_layer_claims_toggle_visibility_on_bare_comma() {
        // 认领触发器必须与 ghostty 默认 ⌘,（super + unicode ','）同形，
        // 才能替换默认的 open_config 绑定（Trigger.set 同键覆盖）。
        let text = host_layer_text();
        assert!(text.contains("keybind = super+,=toggle_visibility\n"));
    }

    #[test]
    fn config_file_line_parsing() {
        assert_eq!(
            line_config_file("config-file = /abs/x.conf"),
            Some("/abs/x.conf".into())
        );
        assert_eq!(
            line_config_file("config-file=\"a b.conf\""),
            Some("a b.conf".into())
        );
        assert_eq!(
            line_config_file("config-file = rel.conf # 尾注释"),
            Some("rel.conf".into())
        );
        assert_eq!(line_config_file("config-file ="), None);
        assert_eq!(line_config_file("font-size = 18"), None);
    }

    #[test]
    fn collect_follows_config_file_chain_with_cycle_guard() {
        let dir = std::env::temp_dir().join(format!("ninja-cfg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("main.conf"),
            "theme=Dracula\nconfig-file = inc.conf\n",
        )
        .unwrap();
        // 环：inc 反指 main（visited 防环不死循环）。
        std::fs::write(
            dir.join("inc.conf"),
            "config-file = main.conf\nbackground = #010203\n",
        )
        .unwrap();
        let files = collect_ghostty_files(&[dir.join("main.conf")]);
        assert_eq!(files.len(), 2, "chain followed, cycle cut: {files:?}");
        assert!(user_sets_theme(&files));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn odp_yields_when_user_sets_theme_anywhere_in_chain() {
        // theme= 扫描：默认文件或 config-file 链上任一处 theme= 都算用户设了主题。
        let dir = std::env::temp_dir().join(format!("ninja-yield-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a"), "font-size = 14\nconfig-file = b\n").unwrap();
        std::fs::write(dir.join("b"), "theme = Dracula\n").unwrap();
        let files = collect_ghostty_files(&[dir.join("a")]);
        assert!(user_sets_theme(&files), "链上 theme= 必须探测到");
        // 注释里的 theme= 不算。
        std::fs::write(dir.join("b"), "# theme = Dracula\n").unwrap();
        let files = collect_ghostty_files(&[dir.join("a")]);
        assert!(!user_sets_theme(&files));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------------
    // ninja.toml 收缩
    // ------------------------------------------------------------------

    #[test]
    fn host_config_plugins_only() {
        let cfg = parse_host_config(
            r##"
[plugins]
enabled = [" preview ", "preview", "doc"]
[plugins.paths]
preview = "/usr/local/bin/ninja-preview"
"##,
        );
        assert_eq!(
            cfg.plugins.enabled,
            vec!["preview".to_string(), "doc".to_string()]
        );
        assert_eq!(
            cfg.plugins.paths,
            vec![(
                "preview".to_string(),
                "/usr/local/bin/ninja-preview".to_string()
            )]
        );
    }

    #[test]
    fn host_config_default_empty_no_spawn() {
        assert_eq!(parse_host_config(""), HostConfig::default());
        assert_eq!(
            parse_host_config("[plugins]\nenabled = []").plugins.enabled,
            Vec::<String>::new()
        );
        // 破损 TOML：全忽略（不炸）。
        assert_eq!(
            parse_host_config("this is [ not toml"),
            HostConfig::default()
        );
    }

    #[test]
    fn host_config_terminal_keys_and_keys_ignored() {
        // v1 终端项 + [keys] + 未知键：警告 + 忽略，但 [plugins] 仍生效
        //（收缩是常态，不整体降级）。
        let cfg = parse_host_config(
            r##"shell = "/bin/zsh"
font-family = "Menlo"
font-size = 14.0

[theme]
cursor = "#528BFF"

[keys]
new_window = "cmd+n"

[plugins]
enabled = ["theme"]

[unknown_section]
x = 1
"##,
        );
        assert_eq!(cfg.plugins.enabled, vec!["theme".to_string()]);
        assert_eq!(cfg.plugins.paths, Vec::new());
        // 只有 [plugins] 进结果——shell/font/theme/keys 无字段可查（结构体里没有）。
    }

    // ------------------------------------------------------------------
    // trigger → keyEquivalent
    // ------------------------------------------------------------------

    fn uni(cp: u32, mods: u32) -> ghostty_input_trigger_s {
        ghostty_input_trigger_s {
            tag: GHOSTTY_TRIGGER_UNICODE,
            key: ghostty_input_trigger_key_u { unicode: cp },
            mods: mods as _,
        }
    }

    fn phys(k: ghostty_input_key_e, mods: u32) -> ghostty_input_trigger_s {
        ghostty_input_trigger_s {
            tag: GHOSTTY_TRIGGER_PHYSICAL,
            key: ghostty_input_trigger_key_u { physical: k },
            mods: mods as _,
        }
    }

    fn empty_trigger() -> ghostty_input_trigger_s {
        ghostty_input_trigger_s {
            tag: GHOSTTY_TRIGGER_PHYSICAL,
            key: ghostty_input_trigger_key_u { physical: 0 },
            mods: 0,
        }
    }

    #[test]
    fn trigger_conversion_forms() {
        // ⌘T（默认 new_tab：unicode 't' + super）。
        let e = trigger_to_equivalent(uni('t' as u32, GHOSTTY_MODS_SUPER)).unwrap();
        assert_eq!(e.key, 't' as u16);
        assert!(e.cmd && !e.shift && !e.alt && !e.ctrl);

        // ⌘⇧Enter（toggle_split_zoom：physical enter）。
        let e = trigger_to_equivalent(phys(
            GHOSTTY_KEY_ENTER,
            GHOSTTY_MODS_SUPER | GHOSTTY_MODS_SHIFT,
        ))
        .unwrap();
        assert_eq!(e.key, 0x0D);
        assert!(e.cmd && e.shift);

        // ⌥⌘←（goto_split:left：physical arrow → F702）。
        let e = trigger_to_equivalent(phys(
            GHOSTTY_KEY_ARROW_LEFT,
            GHOSTTY_MODS_SUPER | GHOSTTY_MODS_ALT,
        ))
        .unwrap();
        assert_eq!(e.key, 0xF702);
        assert!(e.cmd && e.alt);

        // 空 trigger（动作未绑定：physical 0 = unidentified）→ None。
        assert_eq!(trigger_to_equivalent(empty_trigger()), None);
        // catch-all → None。
        assert_eq!(
            trigger_to_equivalent(ghostty_input_trigger_s {
                tag: GHOSTTY_TRIGGER_CATCH_ALL,
                ..empty_trigger()
            }),
            None
        );
        // BMP 外 unicode → None。
        assert_eq!(trigger_to_equivalent(uni(0x1F600, 0)), None);
    }

    // ------------------------------------------------------------------
    // 热重载快照
    // ------------------------------------------------------------------

    #[test]
    fn plugins_memory_limit_mb_parses() {
        let cfg = parse_host_config("[plugins]\nenabled = []\nmemory_limit_mb = 256\n");
        assert_eq!(cfg.plugins.memory_limit_mb, Some(256));
        let cfg = parse_host_config("[plugins]\nenabled = []\n");
        assert_eq!(cfg.plugins.memory_limit_mb, None);
        let cfg = parse_host_config("[plugins]\nmemory_limit_mb = -1\n");
        assert_eq!(cfg.plugins.memory_limit_mb, None, "负值应忽略");
    }

    #[test]
    fn watch_state_detects_change_and_new_file() {
        let dir = std::env::temp_dir().join(format!("ninja-watch-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("config");
        std::fs::write(&f, "font-size = 13\n").unwrap();
        let files = vec![f.clone()];
        let mut st = snapshot_watch(&files);
        assert!(!st.changed(&files), "unchanged");
        std::fs::write(&f, "font-size = 14\n").unwrap();
        assert!(st.changed(&files), "mtime change detected");
        assert!(!st.changed(&files), "snapshot updated");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
