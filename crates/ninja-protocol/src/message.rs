//! 六类消息的 Rust 类型（线格式 schema 的权威定义）。
//!
//! 每条消息一个结构体，公共字段 `v`（协议版本）+ 由 [`Message`] 的
//! internally-tagged 枚举注入的 `type`；线上形态 `{"type":..,"v":..,...}`
//!（type 在前、v 随后，golden 测试钉死字节）。字段集即线字段集——
//! 改字段 = 改协议，走版本规则（见 crate 文档）。
//!
//! 按 PRODUCT/PLAN 重钉（q3）：消息集与线语义对照旧树
//! （1240428:crates/ninja-protocol）移植，v0 未对外发布，字段集直接钉进
//! v0（含 `hit.cwd` 增补与 `theme.set` 增补的历史决策，见 crate 文档
//! 「版本与演化规则」）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 当前协议版本。本版本内所有消息 `v == 0`。
pub const PROTOCOL_VERSION: u32 = 0;

// ---------------------------------------------------------------------------
// 枚举（线上的字符串）
// ---------------------------------------------------------------------------

/// 修饰键。线上：`"shift"` / `"ctrl"` / `"alt"` / `"cmd"`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modifier {
    Shift,
    Ctrl,
    Alt,
    Cmd,
}

/// `hit` 的命中种类。线上：`"path"` / `"url"` / `"osc8"`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HitKind {
    Path,
    Url,
    Osc8,
}

/// 层的位置。线上：`"overlay"`（盖在命中 cell 上）/ `"side"`（侧开）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    Overlay,
    Side,
}

// ---------------------------------------------------------------------------
// hit：宿主认出路径/URL/OSC-8 → 插件 claim 或 ignore
// ---------------------------------------------------------------------------

/// 宿主→插件：在 cell (row, col)（pane 内 vt 网格坐标，0 基）认出
/// 可点对象。`id` 用于回执配对。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub v: u32,
    /// 关联 id：`hit.claim` / `hit.ignore` 必须回同一个值。
    pub id: u64,
    pub kind: HitKind,
    /// 命中文本（路径 / URL / OSC-8 链接）。原样字节，不猜测展开。
    pub text: String,
    /// 相对路径的解析基目录（shell 经 OSC 7 报告的工作目录；空 = 未知）。
    /// 绝对路径 / URL 时插件可忽略本字段。进程外插件无法访问宿主的
    /// OSC-7 状态，没有它就永远认领不了相对路径；收方对未知字段一律
    /// 忽略的规则保证旧实现兼容。
    #[serde(default)]
    pub cwd: String,
    /// 命中 cell 的行（0 基，pane 内 vt 网格）。
    pub row: u32,
    /// 命中 cell 的列（0 基）。
    pub col: u32,
    /// 命中所在 pane 的 id。
    pub pane: u32,
    /// 触发时的修饰键（无则空数组，字段必在）。
    pub modifiers: Vec<Modifier>,
}

impl Hit {
    pub fn new(
        id: u64,
        kind: HitKind,
        text: impl Into<String>,
        cwd: impl Into<String>,
        row: u32,
        col: u32,
        pane: u32,
        modifiers: Vec<Modifier>,
    ) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            kind,
            text: text.into(),
            cwd: cwd.into(),
            row,
            col,
            pane,
            modifiers,
        }
    }
}

/// 插件→宿主：认领本次命中。多个插件都 claim 时 `priority` 大者胜。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HitClaim {
    pub v: u32,
    /// 回执：等于所答 `hit` 的 `id`。
    pub id: u64,
    pub priority: u32,
}

impl HitClaim {
    pub fn new(id: u64, priority: u32) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            priority,
        }
    }
}

/// 插件→宿主：不认领本次命中。全 ignore → 系统默认打开。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HitIgnore {
    pub v: u32,
    pub id: u64,
}

impl HitIgnore {
    pub fn new(id: u64) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
        }
    }
}

// ---------------------------------------------------------------------------
// layer：插件要层 → 宿主给尺寸/DPI/IOSurface → 插件画完 present
// ---------------------------------------------------------------------------

/// 插件→宿主：请求一层。`anchor_*` 是命中 cell 坐标（overlay 的锚点、
/// side 的参考行）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerOpen {
    pub v: u32,
    pub id: u64,
    pub placement: Placement,
    pub anchor_row: u32,
    pub anchor_col: u32,
}

impl LayerOpen {
    pub fn new(id: u64, placement: Placement, anchor_row: u32, anchor_col: u32) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            placement,
            anchor_row,
            anchor_col,
        }
    }
}

/// 宿主→插件：层就绪。像素画进 `io_surface_id` 指向的 IOSurface
/// （global ID，macOS），画完发 [`LayerPresent`]。尺寸/DPI 由宿主定。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerReady {
    pub v: u32,
    /// 回执：等于所答 `layer.open` 的 `id`。
    pub id: u64,
    /// 层句柄：后续 present/close/input.key 引用它。
    pub layer: u64,
    pub width_px: u32,
    pub height_px: u32,
    pub dpi: u32,
    pub io_surface_id: u64,
}

impl LayerReady {
    pub fn new(
        id: u64,
        layer: u64,
        width_px: u32,
        height_px: u32,
        dpi: u32,
        io_surface_id: u64,
    ) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            layer,
            width_px,
            height_px,
            dpi,
            io_surface_id,
        }
    }
}

/// 插件→宿主：本帧画完，请合成。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerPresent {
    pub v: u32,
    pub layer: u64,
}

impl LayerPresent {
    pub fn new(layer: u64) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            layer,
        }
    }
}

/// 双向：关层。Esc 关层 / 宿主收回都走它。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerClose {
    pub v: u32,
    pub layer: u64,
}

impl LayerClose {
    pub fn new(layer: u64) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            layer,
        }
    }
}

// ---------------------------------------------------------------------------
// input：快捷键申请 + 层前台时的键盘事件
// ---------------------------------------------------------------------------

/// 插件→宿主：申请全局快捷键。`key` 见 crate 文档命名集。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputHotkey {
    pub v: u32,
    pub id: u64,
    pub key: String,
    pub modifiers: Vec<Modifier>,
}

impl InputHotkey {
    pub fn new(id: u64, key: impl Into<String>, modifiers: Vec<Modifier>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            key: key.into(),
            modifiers,
        }
    }
}

/// 宿主→插件：快捷键已授予。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputHotkeyGranted {
    pub v: u32,
    pub id: u64,
}

impl InputHotkeyGranted {
    pub fn new(id: u64) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
        }
    }
}

/// 宿主→插件：快捷键被拒（冲突等）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputHotkeyDenied {
    pub v: u32,
    pub id: u64,
    pub reason: String,
}

impl InputHotkeyDenied {
    pub fn new(id: u64, reason: impl Into<String>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            reason: reason.into(),
        }
    }
}

/// 宿主→插件：层在前台时的键盘事件。`text` 是提交文本（IME 产物），
/// 无则 `""`。`layer == 0` 表示无层上下文（宿主授予的全局热键触发，
/// 适配器语义，见宿主 plugins.rs；线上仍是本消息）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputKey {
    pub v: u32,
    pub layer: u64,
    pub key: String,
    pub text: String,
    pub modifiers: Vec<Modifier>,
}

impl InputKey {
    pub fn new(
        layer: u64,
        key: impl Into<String>,
        text: impl Into<String>,
        modifiers: Vec<Modifier>,
    ) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            layer,
            key: key.into(),
            text: text.into(),
            modifiers,
        }
    }
}

// ---------------------------------------------------------------------------
// spawn：辅助进程由宿主代拉、宿主管生命周期与内存上限
// ---------------------------------------------------------------------------

/// 插件→宿主：请求辅助进程。`memory_limit_bytes == 0` = 用宿主默认。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpawnRequest {
    pub v: u32,
    pub id: u64,
    pub argv: Vec<String>,
    pub memory_limit_bytes: u64,
}

impl SpawnRequest {
    pub fn new(id: u64, argv: Vec<String>, memory_limit_bytes: u64) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            argv,
            memory_limit_bytes,
        }
    }
}

/// 宿主→插件：进程已拉起。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpawnStarted {
    pub v: u32,
    pub id: u64,
    pub pid: u32,
}

impl SpawnStarted {
    pub fn new(id: u64, pid: u32) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            pid,
        }
    }
}

/// 宿主→插件：请求被拒（超上限、命令不允许等）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpawnDenied {
    pub v: u32,
    pub id: u64,
    pub reason: String,
}

impl SpawnDenied {
    pub fn new(id: u64, reason: impl Into<String>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            reason: reason.into(),
        }
    }
}

/// 宿主→插件：进程退出（正常退出 `code` 为退出码；信号致死由宿主
/// 归一成负值）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpawnExited {
    pub v: u32,
    pub id: u64,
    pub pid: u32,
    pub code: i32,
}

impl SpawnExited {
    pub fn new(id: u64, pid: u32, code: i32) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            pid,
            code,
        }
    }
}

// ---------------------------------------------------------------------------
// theme：插件换全色板
// ---------------------------------------------------------------------------

/// 插件→宿主：换当前生效色板（完整覆盖：背景/前景/光标/选区/分隔条/
/// ANSI 16）。宿主内置 One Dark Pro 为不可卸基线；本消息是唯一的主题
/// 切换原语入口——插件连接死亡/禁用时宿主回退内置/用户基线。颜色一律
/// `#rrggbb`（6 位十六进制，前缀 `#`）；`selection_alpha` 是选区不透明
/// 度 0-255；`name` 仅用于宿主日志/取证。色值语义非法（格式坏/alpha
/// 越界）由宿主整条忽略（警告，不断连）；字段类型/数量错（如 `ansi`
/// 不是 16 元数组）是解码错误，按契约处置。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThemeSet {
    pub v: u32,
    /// 色板名（宿主日志/取证用；不参与匹配）。
    pub name: String,
    /// 默认背景（OSC 11 查询应答的就是它）。
    pub bg: String,
    /// 默认前景（OSC 10 查询应答的就是它）。
    pub fg: String,
    /// 光标色。
    pub cursor: String,
    /// 选区背景色。
    pub selection_bg: String,
    /// 选区不透明度（0-255）。
    pub selection_alpha: u32,
    /// 分隔条/边框色。
    pub divider: String,
    /// ANSI 16 色（含 bright），下标即调色板 0-15；必须恰好 16 个。
    pub ansi: [String; 16],
}

impl ThemeSet {
    pub fn new(
        name: impl Into<String>,
        bg: impl Into<String>,
        fg: impl Into<String>,
        cursor: impl Into<String>,
        selection_bg: impl Into<String>,
        selection_alpha: u32,
        divider: impl Into<String>,
        ansi: [String; 16],
    ) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            name: name.into(),
            bg: bg.into(),
            fg: fg.into(),
            cursor: cursor.into(),
            selection_bg: selection_bg.into(),
            selection_alpha,
            divider: divider.into(),
            ansi,
        }
    }
}

// ---------------------------------------------------------------------------
// config：只读推送给插件
// ---------------------------------------------------------------------------

/// 宿主→插件：当前启用插件列表 / 宿主键位（动作名→绑定串，如
/// `"new_window" → "cmd+n"`）/ 内存上限。`keys` 是 BTreeMap：线上
/// 键排序输出，字节形态确定（golden 可钉）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigPush {
    pub v: u32,
    pub enabled: Vec<String>,
    pub keys: BTreeMap<String, String>,
    pub memory_limit_bytes: u64,
}

impl ConfigPush {
    pub fn new(
        enabled: Vec<String>,
        keys: BTreeMap<String, String>,
        memory_limit_bytes: u64,
    ) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            enabled,
            keys,
            memory_limit_bytes,
        }
    }
}

// ---------------------------------------------------------------------------
// 顶层枚举
// ---------------------------------------------------------------------------

/// 方向（谁发给谁）。文档/测试用；线格式里不存在。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    HostToPlugin,
    PluginToHost,
    Both,
}

/// 消息本体。序列化时 serde 把判别字段并入顶层：`{"v":0,"type":"hit",...}`。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Message {
    #[serde(rename = "hit")]
    Hit(Hit),
    #[serde(rename = "hit.claim")]
    HitClaim(HitClaim),
    #[serde(rename = "hit.ignore")]
    HitIgnore(HitIgnore),
    #[serde(rename = "layer.open")]
    LayerOpen(LayerOpen),
    #[serde(rename = "layer.ready")]
    LayerReady(LayerReady),
    #[serde(rename = "layer.present")]
    LayerPresent(LayerPresent),
    #[serde(rename = "layer.close")]
    LayerClose(LayerClose),
    #[serde(rename = "input.hotkey")]
    InputHotkey(InputHotkey),
    #[serde(rename = "input.hotkey.granted")]
    InputHotkeyGranted(InputHotkeyGranted),
    #[serde(rename = "input.hotkey.denied")]
    InputHotkeyDenied(InputHotkeyDenied),
    #[serde(rename = "input.key")]
    InputKey(InputKey),
    #[serde(rename = "spawn.request")]
    SpawnRequest(SpawnRequest),
    #[serde(rename = "spawn.started")]
    SpawnStarted(SpawnStarted),
    #[serde(rename = "spawn.denied")]
    SpawnDenied(SpawnDenied),
    #[serde(rename = "spawn.exited")]
    SpawnExited(SpawnExited),
    #[serde(rename = "config.push")]
    ConfigPush(ConfigPush),
    #[serde(rename = "theme.set")]
    ThemeSet(ThemeSet),
}

/// 本版本全部 type 字符串（顺序与 [`Message`] 变体一致）。
pub const KNOWN_TYPES: &[&str] = &[
    "hit",
    "hit.claim",
    "hit.ignore",
    "layer.open",
    "layer.ready",
    "layer.present",
    "layer.close",
    "input.hotkey",
    "input.hotkey.granted",
    "input.hotkey.denied",
    "input.key",
    "spawn.request",
    "spawn.started",
    "spawn.denied",
    "spawn.exited",
    "config.push",
    "theme.set",
];

impl Message {
    /// 信封版本（恒 [`PROTOCOL_VERSION`]，除非手工构造错值）。
    pub fn v(&self) -> u32 {
        match self {
            Message::Hit(m) => m.v,
            Message::HitClaim(m) => m.v,
            Message::HitIgnore(m) => m.v,
            Message::LayerOpen(m) => m.v,
            Message::LayerReady(m) => m.v,
            Message::LayerPresent(m) => m.v,
            Message::LayerClose(m) => m.v,
            Message::InputHotkey(m) => m.v,
            Message::InputHotkeyGranted(m) => m.v,
            Message::InputHotkeyDenied(m) => m.v,
            Message::InputKey(m) => m.v,
            Message::SpawnRequest(m) => m.v,
            Message::SpawnStarted(m) => m.v,
            Message::SpawnDenied(m) => m.v,
            Message::SpawnExited(m) => m.v,
            Message::ConfigPush(m) => m.v,
            Message::ThemeSet(m) => m.v,
        }
    }

    /// type 字符串（与 [`KNOWN_TYPES`] 对应）。
    pub fn message_type(&self) -> &'static str {
        match self {
            Message::Hit(_) => "hit",
            Message::HitClaim(_) => "hit.claim",
            Message::HitIgnore(_) => "hit.ignore",
            Message::LayerOpen(_) => "layer.open",
            Message::LayerReady(_) => "layer.ready",
            Message::LayerPresent(_) => "layer.present",
            Message::LayerClose(_) => "layer.close",
            Message::InputHotkey(_) => "input.hotkey",
            Message::InputHotkeyGranted(_) => "input.hotkey.granted",
            Message::InputHotkeyDenied(_) => "input.hotkey.denied",
            Message::InputKey(_) => "input.key",
            Message::SpawnRequest(_) => "spawn.request",
            Message::SpawnStarted(_) => "spawn.started",
            Message::SpawnDenied(_) => "spawn.denied",
            Message::SpawnExited(_) => "spawn.exited",
            Message::ConfigPush(_) => "config.push",
            Message::ThemeSet(_) => "theme.set",
        }
    }

    /// 所属六类之一：`hit` / `layer` / `input` / `spawn` / `config` /
    /// `theme`（type 的第一个 `.` 前段）。
    pub fn class(&self) -> &'static str {
        self.message_type().split('.').next().unwrap_or("")
    }

    /// 方向。`layer.close` 双向。
    pub fn direction(&self) -> Direction {
        match self {
            Message::Hit(_)
            | Message::LayerReady(_)
            | Message::InputHotkeyGranted(_)
            | Message::InputHotkeyDenied(_)
            | Message::InputKey(_)
            | Message::SpawnStarted(_)
            | Message::SpawnDenied(_)
            | Message::SpawnExited(_)
            | Message::ConfigPush(_) => Direction::HostToPlugin,
            Message::HitClaim(_)
            | Message::HitIgnore(_)
            | Message::LayerOpen(_)
            | Message::LayerPresent(_)
            | Message::InputHotkey(_)
            | Message::SpawnRequest(_)
            | Message::ThemeSet(_) => Direction::PluginToHost,
            Message::LayerClose(_) => Direction::Both,
        }
    }

    /// 每条消息一份真实样例（契约/golden/往返测试共用；也是第二语言
    /// 实现者抄字段的参考）。顺序与 [`KNOWN_TYPES`] 一致。
    pub fn sample_messages() -> Vec<Message> {
        use std::collections::BTreeMap;
        vec![
            Message::Hit(Hit::new(
                7,
                HitKind::Path,
                "src/main.rs:42:13",
                "/Users/jal/demo",
                41,
                0,
                2,
                vec![Modifier::Cmd],
            )),
            Message::HitClaim(HitClaim::new(7, 10)),
            Message::HitIgnore(HitIgnore::new(7)),
            Message::LayerOpen(LayerOpen::new(8, Placement::Overlay, 41, 0)),
            Message::LayerReady(LayerReady::new(8, 3, 640, 480, 144, 123456)),
            Message::LayerPresent(LayerPresent::new(3)),
            Message::LayerClose(LayerClose::new(3)),
            Message::InputHotkey(InputHotkey::new(9, "p", vec![Modifier::Cmd])),
            Message::InputHotkeyGranted(InputHotkeyGranted::new(9)),
            Message::InputHotkeyDenied(InputHotkeyDenied::new(9, "已被另一个插件占用")),
            Message::InputKey(InputKey::new(3, "esc", "", vec![])),
            Message::SpawnRequest(SpawnRequest::new(
                10,
                vec!["rg".into(), "-n".into(), "你好".into()],
                268_435_456,
            )),
            Message::SpawnStarted(SpawnStarted::new(10, 4242)),
            Message::SpawnDenied(SpawnDenied::new(10, "argv 为空")),
            Message::SpawnExited(SpawnExited::new(10, 4242, 0)),
            Message::ConfigPush(ConfigPush::new(
                vec!["preview".into()],
                BTreeMap::from([("new_window".into(), "cmd+n".into())]),
                536_870_912,
            )),
            Message::ThemeSet(ThemeSet::new(
                "solarized-dark",
                "#002b36",
                "#839496",
                "#93a1a1",
                "#073642",
                102,
                "#586e75",
                [
                    "#073642".into(),
                    "#dc322f".into(),
                    "#859900".into(),
                    "#b58900".into(),
                    "#268bd2".into(),
                    "#d33682".into(),
                    "#2aa198".into(),
                    "#eee8d5".into(),
                    "#002b36".into(),
                    "#cb4b16".into(),
                    "#586e75".into(),
                    "#657b83".into(),
                    "#839496".into(),
                    "#6c71c4".into(),
                    "#93a1a1".into(),
                    "#fdf6e3".into(),
                ],
            )),
        ]
    }
}

/// type 字符串是否属于本版本（版本内 type 集冻结）。
pub fn is_known_type(type_: &str) -> bool {
    KNOWN_TYPES.contains(&type_)
}
