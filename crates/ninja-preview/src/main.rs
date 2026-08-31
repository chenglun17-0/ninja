//! ninja-preview：官方示例插件（q3）——文本/代码 pager，独立进程。
//!
//! 产品定位（PRODUCT.md）：只预览文本和代码；第一次点击才被宿主拉起
//! （宿主启动/面板开即 spawn 本进程——2026-08-29 决策「启用即拉起」；
//! 本进程等 hit 认领）；官方不特权——与社区插件
//! 走同一套 ADE 协议，只经 Unix socket 交换 JSON 帧（ninja-protocol），
//! 永不链宿主内部 API（`cargo tree -p ninja-preview` 无宿主 crate）。
//!
//! 生命周期（v0 协议子集，未知 `v` 必须退出不猜）：
//!
//! ```text
//! 宿主 spawn（env NINJA_ADE_SOCK，启用即拉起）→ connect
//! ← hit            （kind=path；text 可为相对路径，cwd 是解析基）
//! → hit.claim      （存在、非二进制、≤8MiB 的文本文件；否则 hit.ignore）
//! → layer.open     （overlay，锚点 = 命中 cell）
//! ← layer.ready    （尺寸/DPI/IOSurface global id）
//!   画文本进 IOSurface（CoreText→CGContext，跨进程共享内存）
//! → layer.present
//! ← input.key "esc" 或 ← layer.close（宿主 Esc 兜底/收回）
//!   清层状态，回到等 hit（进程驻留；生命周期归宿主监督器）
//! ```
//!
//! 与宿主的全部交互 = 上述帧。文件系统读取（读目标文件）是插件自身
//! 行为，不经协议。二进制判定：首 8KiB 含 NUL 即拒（git 的等效启发）。

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use ninja_protocol::frame::{FrameDecoder, encode_frame};
use ninja_protocol::{
    DecodeError, Hit, HitClaim, HitIgnore, HitKind, LayerClose, LayerOpen, LayerPresent,
    LayerReady, Message, Placement, PROTOCOL_VERSION,
};

/// socket 路径环境变量（宿主拉起时注入；约定见 ninja-protocol 文档）。
const SOCK_ENV: &str = "NINJA_ADE_SOCK";
/// claim 优先级：文本 pager 是「预览」的常规档位，社区插件可用更大值
/// 覆盖（协议仲裁：priority 大者胜）。
const CLAIM_PRIORITY: u32 = 100;
/// 预览文件大小上限（与协议单帧上限同数量级；更大的文件不是 pager
/// 的职责）。
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// 二进制嗅探窗口。
const SNIFF_BYTES: usize = 8 * 1024;

fn main() {
    let code = run();
    std::process::exit(code);
}

/// 返回进程退出码：0 = 正常（socket EOF / 宿主退出）；2 = 环境错；
/// 78 = 协议版本不支持（必须退出、不猜，见 ninja-protocol 契约）。
fn run() -> i32 {
    let Some(sock) = std::env::var_os(SOCK_ENV) else {
        eprintln!("ninja-preview: 缺 {SOCK_ENV}（应由宿主拉起）");
        return 2;
    };
    // 宿主先绑 socket 再 spawn（plugins.rs 顺序），连接重试只兜调度抖动。
    let mut stream = None;
    for _ in 0..100 {
        match UnixStream::connect(&sock) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
    let Some(mut stream) = stream else {
        eprintln!("ninja-preview: 连不上 ADE socket {sock:?}");
        return 2;
    };
    eprintln!("ninja-preview: 已连接宿主（v0）");

    let mut decoder = FrameDecoder::new();
    let mut st = PluginState::default();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return 0, // 宿主退出：正常收尾
            Ok(n) => {
                if decoder.extend(&buf[..n]).is_err() {
                    eprintln!("ninja-preview: 帧缓冲超限，断开");
                    return 2;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                eprintln!("ninja-preview: socket 读失败：{e}");
                return 2;
            }
        }
        while let Some(payload) = decoder.pop() {
            let payload = match payload {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("ninja-preview: 帧级违规（{e}），断开");
                    return 2;
                }
            };
            match Message::decode_plugin(&payload) {
                Ok(msg) => {
                    if !handle(&mut st, &msg, &mut stream) {
                        return 0; // 处理中要求退出（如版本拒绝已在 decode 报错路径）
                    }
                }
                Err(DecodeError::UnsupportedVersion { got, supported }) => {
                    // 契约：不支持的 v 必须立即退出，不猜。
                    eprintln!(
                        "ninja-preview: 协议版本 v{got} 不支持（本实现 v{supported}），退出"
                    );
                    return 78;
                }
                Err(e) => {
                    // 同版本内的坏消息：拒收这一条，不断连（宿主对未知
                    // 字段 lenient，我们对坏帧保守跳过，保持连接活性）。
                    eprintln!("ninja-preview: 丢弃无法解码的消息：{e}");
                }
            }
        }
    }
}

/// 插件会话状态（一次至多一个层（v0 简化））。
#[derive(Default)]
struct PluginState {
    /// 当前打开的层句柄（layer.ready 分配；close 后清空）。
    layer: Option<u64>,
}

/// 处理一条消息；返回 false = 要求退出进程。
fn handle(st: &mut PluginState, msg: &Message, stream: &mut UnixStream) -> bool {
    match msg {
        Message::Hit(hit) => on_hit(hit, stream),
        Message::LayerReady(ready) => on_layer_ready(st, ready, stream),
        Message::InputKey(key) => {
            // Esc 关层（宿主也会在 Esc 时直接 layer.close——两条路都收口）。
            if key.key == "esc" {
                if let Some(layer) = st.layer.take() {
                    let _ = send(stream, &Message::LayerClose(LayerClose::new(layer)));
                }
            }
            true
        }
        Message::LayerClose(close) => {
            // 宿主收回（Esc 兜底 / pane 关闭 / resize）。
            if st.layer == Some(close.layer) {
                st.layer = None;
            }
            true
        }
        // v0 其余消息（hotkey / spawn / config）与 pager 无关：忽略。
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// hit → claim/ignore + layer.open
// ---------------------------------------------------------------------------

fn on_hit(hit: &Hit, stream: &mut UnixStream) -> bool {
    if std::env::var_os("NINJA_ADE_DEBUG").is_some() {
        eprintln!(
            "ninja-preview: hit id={} kind={:?} text={:?} cwd={:?}",
            hit.id, hit.kind, hit.text, hit.cwd
        );
    }
    match claimable_target(hit) {
        Some(target) => {
            eprintln!(
                "ninja-preview: claim {}（行 {} 起）",
                target.path.display(),
                target.line.unwrap_or(1)
            );
            let _ = send(
                stream,
                &Message::HitClaim(HitClaim::new(hit.id, CLAIM_PRIORITY)),
            );
            // 认领即要层：overlay，锚在命中 cell。
            let _ = send(
                stream,
                &Message::LayerOpen(LayerOpen::new(
                    hit.id,
                    Placement::Overlay,
                    hit.row,
                    hit.col,
                )),
            );
            // 待 layer.ready 时用（无重排：把解析结果留在进程内
            // 的「最近一次认领」槽位，on_layer_ready 取用）。
            LAST_CLAIM.with(|c| *c.borrow_mut() = Some(target));
            true
        }
        None => {
            let _ = send(stream, &Message::HitIgnore(HitIgnore::new(hit.id)));
            true
        }
    }
}

/// 预览目标（claim 决策产物）。
struct Target {
    path: PathBuf,
    /// 命中行（1 基；无 :line 后缀 = 从头看）。
    line: Option<u32>,
}

thread_local! {
    /// 最近一次认领的目标（单线程插件；层就绪时消费）。
    static LAST_CLAIM: std::cell::RefCell<Option<Target>> = const { std::cell::RefCell::new(None) };
}

/// claim 判定：path 类 + 可解析（绝对 / ~ 展开 / cwd 基）+ 存在 + 普通文件
/// + ≤ 上限 + 非二进制嗅探。任何一步不满足 → None（回 ignore）。
fn claimable_target(hit: &Hit) -> Option<Target> {
    if hit.v != PROTOCOL_VERSION {
        return None; // 版本门已在 decode 层把关；这里防御
    }
    if hit.kind != HitKind::Path {
        return None; // pager 只吃路径；URL/OSC-8 留给别人（系统默认）
    }
    let (bare, line, _col) = strip_line_col(&hit.text);
    if bare.is_empty() {
        return None;
    }
    let resolved = resolve_path(bare, &hit.cwd)?;
    let meta = std::fs::metadata(&resolved).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    if is_probably_binary(&resolved) {
        return None;
    }
    Some(Target {
        path: resolved,
        line,
    })
}

/// 剥尾部 `:line[:col]`（1 基）。与宿主 open.rs 同规则：非数字段不剥。
fn strip_line_col(s: &str) -> (&str, Option<u32>, Option<u32>) {
    let b = s.as_bytes();
    let mut end = b.len();
    let mut nums: Vec<u32> = Vec::new();
    for _ in 0..2 {
        let Some(colon) = b[..end].iter().rposition(|&c| c == b':') else {
            break;
        };
        let digits = &b[colon + 1..end];
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            break;
        }
        let Ok(n) = std::str::from_utf8(digits).unwrap().parse::<u32>() else {
            break;
        };
        nums.push(n);
        end = colon;
    }
    let bare = &s[..end];
    match nums.len() {
        0 => (bare, None, None),
        1 => (bare, Some(nums[0]), None),
        _ => (bare, Some(nums[1]), Some(nums[0])), // 后剥的是 line
    }
}

/// 路径解析：`~` 展开（$HOME）；相对路径按 cwd 拼；cwd 空 → 放弃。
/// 绝对路径原样。不猜除 $HOME 外的任何家目录（同宿主 open.rs 约定）。
fn resolve_path(bare: &str, cwd: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME");
    resolve_path_with(bare, cwd, home.as_deref())
}

/// 同上，家目录由调用方给（单测不碰进程环境变量，edition 2024 下
/// set_var 也不安全）。
fn resolve_path_with(
    bare: &str,
    cwd: &str,
    home: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    if let Some(rest) = bare.strip_prefix("~/") {
        let home = home?;
        return Some(PathBuf::from(home).join(rest));
    }
    if bare.starts_with('/') {
        return Some(PathBuf::from(bare));
    }
    if bare == "~" {
        return home.map(PathBuf::from);
    }
    if bare.starts_with('~') {
        // `~user/...` 不猜别人家目录（比宿主 open.rs 更保守：pager
        // 只预览自己能解析的路径）。
        return None;
    }
    if cwd.is_empty() {
        return None; // 相对路径且宿主没给 cwd：不认领（无法解析）
    }
    Some(PathBuf::from(cwd).join(bare))
}

/// 首段 NUL 嗅探：含 NUL 几乎必是二进制（git 同款启发式）。
fn is_probably_binary(path: &std::path::Path) -> bool {
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return true, // 读不开就当不可预览
    };
    let mut head = vec![0u8; SNIFF_BYTES.min(4096)];
    let n = f.read(&mut head).unwrap_or(0);
    head[..n].contains(&0)
}

// ---------------------------------------------------------------------------
// layer.ready → 画 → present
// ---------------------------------------------------------------------------

fn on_layer_ready(st: &mut PluginState, ready: &LayerReady, stream: &mut UnixStream) -> bool {
    let Some(target) = LAST_CLAIM.with(|c| c.borrow_mut().take()) else {
        // 没有 pending 认领却收到 ready：宿主状态错位，退层。
        let _ = send(
            stream,
            &Message::LayerClose(LayerClose::new(ready.layer)),
        );
        return true;
    };
    st.layer = Some(ready.layer);
    match draw_preview(&target, ready) {
        Ok(()) => {
            let _ = send(
                stream,
                &Message::LayerPresent(LayerPresent::new(ready.layer)),
            );
            eprintln!(
                "ninja-preview: present 层 {}（{}x{} @{}dpi）",
                ready.layer, ready.width_px, ready.height_px, ready.dpi
            );
        }
        Err(e) => {
            eprintln!("ninja-preview: 画层失败：{e}");
            let _ = send(
                stream,
                &Message::LayerClose(LayerClose::new(ready.layer)),
            );
            st.layer = None;
        }
    }
    true
}

fn send(stream: &mut UnixStream, msg: &Message) -> std::io::Result<()> {
    let frame = encode_frame(msg)
        .map_err(|e| std::io::Error::other(format!("encode: {e}")))?;
    stream.write_all(&frame)
}

// ---------------------------------------------------------------------------
// IOSurface 绘制（CoreText → CGContext 覆盖在共享内存上）
// ---------------------------------------------------------------------------

mod surface_draw;

fn draw_preview(target: &Target, ready: &LayerReady) -> Result<(), String> {
    let content = std::fs::read_to_string(&target.path)
        .map_err(|e| format!("读 {}: {e}", target.path.display()))?;
    surface_draw::draw(target, &content, ready)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(text: &str, cwd: &str) -> Hit {
        Hit::new(1, HitKind::Path, text, cwd, 0, 0, 1, vec![])
    }

    #[test]
    fn line_col_stripped() {
        assert_eq!(
            strip_line_col("/a/b/main.rs:42:13"),
            ("/a/b/main.rs", Some(42), Some(13))
        );
        assert_eq!(strip_line_col("/a/b/main.rs:42"), ("/a/b/main.rs", Some(42), None));
        assert_eq!(strip_line_col("/a/b.rs"), ("/a/b.rs", None, None));
        // 非数字尾不剥（Windows 风格盘符同理不受伤）。
        assert_eq!(strip_line_col("/a/b:c/d.txt").0, "/a/b:c/d.txt");
    }

    #[test]
    fn resolve_rules() {
        // 绝对路径原样；~ 展开；相对按 cwd；无 cwd 的相对 → None。
        let t = Some(std::ffi::OsStr::new("/Users/t"));
        assert_eq!(
            resolve_path_with("/abs/x.rs", "whatever", t).unwrap(),
            PathBuf::from("/abs/x.rs")
        );
        assert_eq!(
            resolve_path_with("~/x/y.rs", "", t).unwrap(),
            PathBuf::from("/Users/t/x/y.rs")
        );
        assert_eq!(
            resolve_path_with("src/main.rs", "/repo", None).unwrap(),
            PathBuf::from("/repo/src/main.rs")
        );
        assert!(resolve_path_with("src/main.rs", "", t).is_none());
        // 不猜别人家目录（同宿主 open.rs 约定）；HOME 缺失时 ~ 不展开。        assert_eq!(resolve_path_with("~other/x", "/repo", t), None);
        assert_eq!(resolve_path_with("~", "", None), None);
        assert_eq!(resolve_path_with("~", "", t).unwrap(), PathBuf::from("/Users/t"));
    }

    #[test]
    fn claimable_rules() {
        let dir = std::env::temp_dir().join(format!("ninja_prev_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let txt = dir.join("ok.rs");
        std::fs::write(&txt, "fn main() {}\n").unwrap();
        let bin = dir.join("blob.bin");
        std::fs::write(&bin, [0u8, 1, 0, 2]).unwrap();
        let abs = txt.to_str().unwrap().to_string();

        // 绝对 + :line:col → claim，行号带出。
        let t = claimable_target(&hit(&format!("{abs}:3:1"), "")).unwrap();
        assert_eq!(t.path, txt);
        assert_eq!(t.line, Some(3));

        // 相对 + cwd → claim。
        let t = claimable_target(&hit("ok.rs", dir.to_str().unwrap())).unwrap();
        assert_eq!(t.path, txt);
        assert_eq!(t.line, None);

        // 二进制 / 不存在 / 相对无 cwd / URL → None。
        let binabs = bin.to_str().unwrap().to_string();
        assert!(claimable_target(&hit(&binabs, "")).is_none());
        assert!(claimable_target(&hit("/no/such/file.rs", "")).is_none());
        assert!(claimable_target(&hit("ok.rs", "")).is_none());
        assert!(claimable_target(&hit("https://x.io/a", "")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
