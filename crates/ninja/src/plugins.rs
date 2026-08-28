//! p3：宿主侧 ADE 插件门（Unix socket，默认关）。
//!
//! 空载门禁：`[plugins] enabled` 为空（默认）时**不创建 socket 文件、
//! 不拉任何插件进程**——[`PluginHost::start`] 直接返回 `None`，宿主
//! 进程里没有任何插件运行时（验证：`cargo tree -p ninja` 无
//! wasmtime/tokio；默认配置启动后 socket 路径不存在，见
//! `tests/idle_no_plugins.rs` 的运行时取证）。
//!
//! 启用时：绑定 [`socket_path`] 约定的路径并 listen；accept / 握手 /
//! 拉插件进程是 p5 的事，p3 只把「门」钉住：监听与否完全由配置决定。
//! 消息编解码类型来自 `ninja-protocol`（纯 serde 数据 crate，零运行时
//! 成本；协议仍只经 socket 交换字节，双方不共享地址空间）。

use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

/// `[plugins]` 配置（ninja.toml）。默认空 = 插件全关（空载门禁）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginsConfig {
    /// 启用的插件名列表。空 = 关。p5 才真正按名字拉起插件进程。
    pub enabled: Vec<String>,
}

/// 已绑定的 ADE socket 句柄。Drop 时删除 socket 文件（不留残骸）。
#[derive(Debug)]
pub struct PluginHost {
    listener: UnixListener,
    path: PathBuf,
}

/// socket 路径约定：`${TMPDIR:-/tmp}/ninja-ade-{pid}.sock`。
pub fn socket_path() -> PathBuf {
    let pid = std::process::id();
    std::env::temp_dir().join(format!("ninja-ade-{pid}.sock"))
}

/// 实际生效路径：`NINJA_ADE_SOCK` 覆盖（测试钩子；p5 拉插件进程时
/// 也经同名环境变量告知路径）。
fn effective_socket_path() -> PathBuf {
    match std::env::var_os("NINJA_ADE_SOCK") {
        Some(p) => PathBuf::from(p),
        None => socket_path(),
    }
}

impl PluginHost {
    /// 唯一入口：按配置决定绑不绑 socket。
    ///
    /// - `enabled` 为空 → `None`：**不建 socket、不碰文件系统**（空载
    ///   不变量）。
    /// - 非空 → 绑定 + listen（非阻塞：p3 不 accept，内核排队）；
    ///   绑定失败不炸终端：stderr 警告 + `None`（同配置模块的降级哲学）。
    pub fn start(cfg: &PluginsConfig) -> Option<PluginHost> {
        if cfg.enabled.is_empty() {
            return None;
        }
        Self::bind(effective_socket_path())
    }

    /// 在给定路径上绑定（start 的实现核心；测试用隔离目录直调）。
    fn bind(path: PathBuf) -> Option<PluginHost> {
        // 极端场景：同 pid 复用留下陈旧文件。先清再绑。
        let _ = std::fs::remove_file(&path);
        match UnixListener::bind(&path) {
            Ok(listener) => {
                // p3 不 accept：非阻塞，避免任何路径卡 runloop；连接在
                // 内核 backlog 排队，等 p5 的监督器接管。
                if let Err(e) = listener.set_nonblocking(true) {
                    eprintln!("ninja: ADE socket 设非阻塞失败（{e}），插件禁用");
                    let _ = std::fs::remove_file(&path);
                    return None;
                }
                Some(PluginHost { listener, path })
            }
            Err(e) => {
                eprintln!("ninja: ADE socket {path:?} 绑定失败（{e}），插件禁用");
                None
            }
        }
    }

    /// 已绑定的路径（取证/日志用）。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 监听器引用（p5 监督器接管 accept 用；p3 仅持有）。
    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    /// 本测试进程独有的临时目录（不碰全局 TMPDIR 约定路径，避免并行
    /// 测试互踩）。
    fn sandbox(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ninja_plugins_test_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn default_config_starts_nothing() {
        // 空载门禁的核心：默认（空）配置 → None。bind 永不发生，
        // 因此任何路径上都不会出现 socket 文件/监听。
        let cfg = PluginsConfig::default();
        assert!(cfg.enabled.is_empty());
        assert!(
            PluginHost::start(&cfg).is_none(),
            "空载配置绝不起 PluginHost"
        );
    }

    #[test]
    fn bind_listens_and_drop_cleans() {
        let dir = sandbox("bind");
        let sock = dir.join("ade.sock");
        {
            let host = PluginHost::bind(sock.clone()).expect("显式绑定应成功");
            assert_eq!(host.path(), sock.as_path());
            assert!(sock.exists(), "绑定后 socket 文件应在");
            // listen 已生效：客户端能连上（内核排队，p3 不 accept）。
            UnixStream::connect(&sock).expect("启用后可连接（排队，不 accept）");
        } // host drop → 文件清除
        assert!(!sock.exists(), "drop 后 socket 文件应删除");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enabled_via_start_uses_convention_path() {
        // 走真实 start()（含 env 覆盖逻辑）：启用非空 → 绑生效路径
        //（NINJA_ADE_SOCK 设置时用它，否则约定路径）。
        let cfg = PluginsConfig {
            enabled: vec!["preview".into()],
        };
        let expected = match std::env::var_os("NINJA_ADE_SOCK") {
            Some(p) => PathBuf::from(p),
            None => socket_path(),
        };
        {
            let host = PluginHost::start(&cfg).expect("启用即绑");
            assert_eq!(host.path(), expected.as_path());
            assert!(expected.exists());
        }
        if std::env::var_os("NINJA_ADE_SOCK").is_none() {
            assert!(!expected.exists(), "drop 后约定路径应删除");
        }
    }

    #[test]
    fn socket_path_convention_contains_pid() {
        // 约定钉死：${TMPDIR}/ninja-ade-{pid}.sock。
        let p = socket_path();
        assert_eq!(
            p.file_name().unwrap().to_string_lossy(),
            format!("ninja-ade-{}.sock", std::process::id())
        );
        assert_eq!(p.parent(), Some(std::env::temp_dir().as_path()));
    }

    #[test]
    fn bind_failure_degrades_to_none() {
        // 路径不可达（父目录不存在）→ None，不 panic。
        let dir = sandbox("nope");
        let bad = dir.join("missing-dir/ade.sock");
        assert!(PluginHost::bind(bad).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
