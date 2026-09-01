//! ninja.toml 收缩（宿主/插件特有）：解析、缺省、写回。

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// ninja.toml 收缩（宿主/插件特有）
// ---------------------------------------------------------------------------

/// `[plugins]`：只解析不拉起（监督器消费；空载零插件进程/零 socket）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginsConfig {
    pub enabled: Vec<String>,
    pub paths: Vec<(String, String)>,
    /// 单插件内存上限（MiB）。None = 宿主默认（512）；0 = 不限。
    pub memory_limit_mb: Option<u64>,
}

/// ninja.toml 收缩后的宿主配置（q2 只有 [plugins]）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostConfig {
    pub plugins: PluginsConfig,
}

/// ninja.toml 路径（NINJA_CONFIG 覆盖 → ~/.config/ninja/ninja.toml）。
pub fn host_config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("NINJA_CONFIG") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".config/ninja/ninja.toml")
}

/// v1 收缩掉的终端项（警告文案用）。
const V1_TERMINAL_KEYS: &[&str] = &["shell", "font-family", "font_family", "font-size", "theme"];

/// 解析收缩后的 ninja.toml。任何不认识的键（含 v1 的终端项与 [keys]）
/// 一律 stderr 警告并忽略——不整体降级（收缩是常态，用户旧文件应尽量
/// 保留可用部分）。
pub fn parse_host_config(text: &str) -> HostConfig {
    let mut cfg = HostConfig::default();
    let value: toml::Value = match toml::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ninja: ninja.toml 解析失败（忽略全部内容）: {e}");
            return cfg;
        }
    };
    let Some(table) = value.as_table() else {
        eprintln!("ninja: ninja.toml 顶层不是表（忽略全部内容）");
        return cfg;
    };
    for (key, val) in table {
        match key.as_str() {
            "plugins" => parse_plugins(val, &mut cfg),
            "keys" => eprintln!(
                "ninja: ninja.toml [keys] 已收缩（键位全量继承 ghostty \
                 keybind 系统），忽略——重绑请写 ghostty 配置 \
                 `keybind = super+x=action`（config 路径见启动日志）"
            ),
            k if V1_TERMINAL_KEYS.contains(&k) => eprintln!(
                "ninja: ninja.toml `{k}` 已收缩——终端配置（shell/字体/主题色）\
                 走 ghostty 配置文件，忽略"
            ),
            other => eprintln!("ninja: ninja.toml 未知键 `{other}`（q2 只收 [plugins]），忽略"),
        }
    }
    cfg
}

fn parse_plugins(val: &toml::Value, cfg: &mut HostConfig) {
    let Some(t) = val.as_table() else {
        eprintln!("ninja: ninja.toml [plugins] 不是表，忽略");
        return;
    };
    for (key, v) in t {
        match key.as_str() {
            "enabled" => {
                let Some(arr) = v.as_array() else {
                    eprintln!("ninja: [plugins] enabled 不是数组，忽略");
                    continue;
                };
                let mut seen = std::collections::BTreeSet::new();
                for item in arr {
                    match item.as_str() {
                        Some(s) => {
                            let s = s.trim();
                            if s.is_empty() || !seen.insert(s.to_string()) {
                                continue;
                            }
                            cfg.plugins.enabled.push(s.to_string());
                        }
                        None => eprintln!("ninja: [plugins] enabled 项不是字符串，忽略"),
                    }
                }
            }
            "paths" => {
                let Some(pt) = v.as_table() else {
                    eprintln!("ninja: [plugins] paths 不是表，忽略");
                    continue;
                };
                for (name, pv) in pt {
                    match pv.as_str() {
                        Some(s) if !s.trim().is_empty() => {
                            cfg.plugins.paths.push((name.clone(), s.trim().to_string()))
                        }
                        _ => eprintln!("ninja: [plugins.paths] {name} 不是非空字符串，忽略"),
                    }
                }
            }
            "memory_limit_mb" => match v.as_integer() {
                Some(n) if n >= 0 => {
                    cfg.plugins.memory_limit_mb = Some(n as u64);
                }
                _ => eprintln!("ninja: [plugins] memory_limit_mb 不是非负整数，忽略"),
            },
            other => eprintln!("ninja: ninja.toml [plugins] 未知键 `{other}`，忽略"),
        }
    }
}

/// 读 ninja.toml（缺失 = 默认；q2 只解析不拉起）。
pub fn load_host_config() -> HostConfig {
    let path = host_config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_host_config(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HostConfig::default(),
        Err(e) => {
            eprintln!("ninja: 读 {path:?} 失败（{e}），用默认值");
            HostConfig::default()
        }
    }
}

/// q3 面板写回：把 `[plugins] enabled` 名单写回 ninja.toml（toml 往返：
/// 其它段与键保留，注释/格式不保——面板是机器写入方，手写文件请自行
/// 备份；文件不存在则创建）。返回写入路径。
pub fn write_plugins_enabled(enabled: &[String]) -> std::io::Result<PathBuf> {
    let path = host_config_path();
    let mut value: toml::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_else(|| toml::Value::Table(toml::map::Map::new()));
    if !value.is_table() {
        value = toml::Value::Table(toml::map::Map::new()); // 顶层坏值：重建
    }
    let table = value.as_table_mut().expect("顶层是表");
    let plugins = table
        .entry("plugins".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    if !plugins.is_table() {
        *plugins = toml::Value::Table(toml::map::Map::new());
    }
    plugins["enabled"] = toml::Value::Array(
        enabled
            .iter()
            .map(|s| toml::Value::String(s.clone()))
            .collect(),
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = toml::to_string(&value)
        .map_err(|e| std::io::Error::other(format!("toml 序列化失败：{e}")))?;
    std::fs::write(&path, text)?;
    Ok(path)
}
