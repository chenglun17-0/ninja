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
