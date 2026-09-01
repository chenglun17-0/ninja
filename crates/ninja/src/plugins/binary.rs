//! 插件二进制的发现、解析、socket 约定与进程足迹（文件系统与系统调用，无 GUI）。

use std::path::{Path, PathBuf};

use super::PluginsConfig;

// ---------------------------------------------------------------------------
// socket 约定与解析
// ---------------------------------------------------------------------------

/// socket 路径约定：`${TMPDIR:-/tmp}/ninja-ade-{pid}.sock`。
pub fn socket_path() -> PathBuf {
    let pid = std::process::id();
    std::env::temp_dir().join(format!("ninja-ade-{pid}.sock"))
}

/// 实际生效路径：`NINJA_ADE_SOCK` 覆盖（拉起插件进程时经同名环境变量
/// 告知路径；测试钩子同途）。
pub(crate) fn effective_socket_path() -> PathBuf {
    match std::env::var_os("NINJA_ADE_SOCK") {
        Some(p) => PathBuf::from(p),
        None => socket_path(),
    }
}

/// 陈旧 socket 清扫：宿主 SIGKILL/崩溃时 [`PluginHost::shutdown`] 不跑，
/// 约定目录下会留下 `ninja-ade-<pid>.sock` 尸体。规则：文件名里的 pid
/// 已死（`kill(pid,0)`=ESRCH）才删；活 pid 一律不动（并行实例，或 pid
/// 被复用——保守不动）。只在启用插件启动时扫（[`start`]）：空载路径
/// 零文件系统改动。
pub fn sweep_stale_sockets() {
    sweep_stale_sockets_in(&std::env::temp_dir());
}

/// [`sweep_stale_sockets`] 的实现核心（目录可注入，单测用隔离目录）。
pub(crate) fn sweep_stale_sockets_in(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(pid_str) = name
            .strip_prefix("ninja-ade-")
            .and_then(|s| s.strip_suffix(".sock"))
        else {
            continue; // 非本约定的文件不碰
        };
        let Ok(pid) = pid_str.parse::<i32>() else {
            continue; // 名字里不是数字（垃圾名）：不碰
        };
        if pid <= 0 || pid == std::process::id() as i32 {
            continue; // 自己的路径由 bind 处置；非正数必是垃圾名
        }
        // kill(pid, 0)：0/EPERM = 有进程在（不动）；ESRCH = 进程已死。
        let alive = unsafe { libc::kill(pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if !alive {
            let _ = std::fs::remove_file(e.path());
            eprintln!(
                "ninja: 清扫陈旧 ADE socket {}（pid {pid} 已死）",
                e.path().display()
            );
        }
    }
}

/// 用户级插件目录：`~/.config/ninja/plugins`。`HOME` 缺失 → None：该
/// 搜索段整体跳过（其余段照常）。
pub fn user_plugin_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/ninja/plugins"))
}

/// 按名解析插件二进制：`[plugins.paths]` 显式路径 → `$NINJA_PLUGIN_DIR/
/// <name>` → `~/.config/ninja/plugins/<name>` → 宿主二进制同目录
/// `<name>`。都不存在 → None（调用方降级）。
pub fn resolve_plugin_binary(name: &str, cfg: &PluginsConfig) -> Option<PathBuf> {
    resolve_plugin_binary_in(name, cfg, user_plugin_dir().as_deref())
}

/// [`resolve_plugin_binary`] 的实现核心：用户插件目录可注入（单测用
/// 隔离目录，不碰真实 `~/.config`）。段次序见外层文档。
pub(crate) fn resolve_plugin_binary_in(
    name: &str,
    cfg: &PluginsConfig,
    user_dir: Option<&Path>,
) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') {
        return None; // 名字即文件系统注入向量：只收裸名
    }
    if let Some(p) = cfg.paths.get(name) {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
        eprintln!("ninja: plugins.paths.{name} = {} 不存在，跳过该路径", p.display());
    }
    if let Some(dir) = std::env::var_os("NINJA_PLUGIN_DIR") {
        let p = Path::new(&dir).join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(dir) = user_dir {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    // 宿主二进制同目录（开发布局：宿主与插件同置一个 target
    // 目录）。只探测存在性，不写。
    if let Ok(exe) = std::env::current_exe() {
        let p = exe.parent()?.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// 已安装插件的发现（面板行集的「已安装」部分）：扫描可发现段的
/// 直接子项，只收可执行的常规文件裸名（隐藏项跳过；名字即文件系统
/// 注入向量，与 [`resolve_plugin_binary`] 同一卫生规则），并入
/// `[plugins.paths]` 显式键。宿主同目录段**不扫**（开发布局会捞进
/// 无关二进制）。排序去重。空载路径同样可用（只读目录，不建 socket）。
/// 这是「面板能看见已装插件」，不是插件市场。
pub fn discover_plugin_names(cfg: &PluginsConfig) -> Vec<String> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("NINJA_PLUGIN_DIR") {
        dirs.push(PathBuf::from(d));
    }
    if let Some(d) = user_plugin_dir() {
        dirs.push(d);
    }
    discover_plugin_names_in(&dirs, cfg)
}

/// [`discover_plugin_names`] 的实现核心（目录可注入，单测用隔离目录）。
fn discover_plugin_names_in(dirs: &[PathBuf], cfg: &PluginsConfig) -> Vec<String> {
    use std::collections::BTreeSet;
    use std::os::unix::fs::PermissionsExt;
    let mut names: BTreeSet<String> = cfg.paths.keys().cloned().collect();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue; // 目录不存在 = 没装
        };
        for e in rd.flatten() {
            let Some(name) = e.file_name().into_string().ok() else {
                continue;
            };
            if name.starts_with('.') || name.contains('/') {
                continue;
            }
            let Ok(md) = e.metadata() else {
                continue;
            };
            if md.is_file() && md.permissions().mode() & 0o111 != 0 {
                names.insert(name);
            }
        }
    }
    names.into_iter().collect()
}

/// env 门控的调度调试（stderr 一步一行；取证用，不设不打印）。
pub(crate) fn ade_debug(msg: &str) {
    if std::env::var_os("NINJA_ADE_DEBUG").is_some() {
        eprintln!("ninja[ade]: {msg}");
    }
}

// ---------------------------------------------------------------------------
// footprint（面板内存列；空载门禁采样器同款口径）
// ---------------------------------------------------------------------------

/// 子进程真实物理足迹（字节）：`proc_pid_rusage` 的 ri_phys_footprint
/// （与 footprint 工具同源；libSystem 自带，无新增链接面）。
///
/// **结构体尺寸坑（旧树实测 + 本机复测，必须照抄）**：内核按**当前**
/// flavor 的完整结构体写穿（v6 = 16B uuid + 31×u64 = 264B；本机内核
/// 实测写得更宽——264B 缓冲在退出期触发 stack-protector abort），固定
/// 给 512B 裕量；flavor 用 V4（只读前缀字段，偏移由 ABI 钉死）——
/// `ri_phys_footprint` 在公共前缀里（uuid[16] + 7×u64 之后，偏移 72）。
pub fn footprint_bytes(pid: u32) -> Option<u64> {
    const RI_PHYS_FOOTPRINT_OFF: usize = 16 + 7 * 8;
    let mut info = [0u8; 512];
    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut std::ffi::c_void) -> i32;
    }
    // RUSAGE_INFO_V4 = 4；只读前缀字段，偏移由 ABI 钉死。
    let r = unsafe { proc_pid_rusage(pid as i32, 4, info.as_mut_ptr() as *mut std::ffi::c_void) };
    (r == 0).then(|| {
        u64::from_le_bytes(
            info[RI_PHYS_FOOTPRINT_OFF..RI_PHYS_FOOTPRINT_OFF + 8]
                .try_into()
                .expect("常量切片恰 8 字节"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_finds_executable_bare_names_only() {
        let dir = std::env::temp_dir().join(format!(
            "ninja_disc_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mk = |name: &str, exec: bool| {
            let p = dir.join(name);
            std::fs::write(&p, b"#!/bin/sh\n").unwrap();
            let mut perm = std::fs::metadata(&p).unwrap().permissions();
            perm.set_mode(if exec { 0o755 } else { 0o644 });
            std::fs::set_permissions(&p, perm).unwrap();
        };
        mk("alpha", true);
        mk("beta", false); // 不可执行：不算已装插件
        mk(".hidden", true); // 隐藏项：跳过
        std::fs::create_dir_all(dir.join("subdir")).unwrap(); // 目录：跳过
        let mut cfg = PluginsConfig::default();
        cfg.paths.insert("explicit".into(), "/opt/explicit".into());
        let names = discover_plugin_names_in(&[dir.clone()], &cfg);
        assert_eq!(names, vec!["alpha".to_string(), "explicit".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
