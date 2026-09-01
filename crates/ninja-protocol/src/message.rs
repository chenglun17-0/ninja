//! 七类消息的 Rust 类型（线格式 schema 的权威定义）。
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

/// 层的位置。线上：`"overlay"`（盖在命中 cell 上）/
/// `"side"`（侧开）/ `"tab"`（新开一个标签，层铺满）。
/// 只描述放哪，不描述怎么画——画法见 [`Surface`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    Overlay,
    Side,
    Tab,
}

/// 层的表面。线上：`"pixels"`（宿主建 IOSurface，插件写入）/
/// `"html"`（宿主建 WKWebView，插件发 [`LayerHtml`] / [`LayerMsg`]）。
/// 缺省 [`Surface::Pixels`]（旧 golden 不出该字段）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Surface {
    #[default]
    Pixels,
    Html,
}

fn surface_is_pixels(s: &Surface) -> bool {
    *s == Surface::Pixels
}

/// [`InputMouse`] 的键。线上：`"left"` / `"right"` / `"middle"`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// [`InputMouse`] 的动作。线上：`"down"` / `"up"` / `"move"`。
/// `move` 目前是拖动（按下后移动）；自由悬停宿主可不发。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MouseAction {
    Down,
    Up,
    Move,
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
/// side 的参考行）；`tab` 忽略锚点，层铺满新标签。
/// `title` 给标签栏用（空 = 宿主用通用 "Tab"）；缺省且空串不出线，旧 golden 不变。
/// `surface` 声明怎么画（[`Surface::Pixels`] 缺省且不出线，旧 golden 不变）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerOpen {
    pub v: u32,
    pub id: u64,
    pub placement: Placement,
    pub anchor_row: u32,
    pub anchor_col: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "surface_is_pixels")]
    pub surface: Surface,
}

impl LayerOpen {
    pub fn new(id: u64, placement: Placement, anchor_row: u32, anchor_col: u32) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            placement,
            anchor_row,
            anchor_col,
            title: String::new(),
            surface: Surface::Pixels,
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    pub fn with_surface(mut self, surface: Surface) -> Self {
        self.surface = surface;
        self
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

/// 插件→宿主：HTML 文档，宿主在 [`Surface::Html`] 层用 WKWebView 加载。
/// `html` 是完整文档（含 CSS）；单帧仍受 8MiB 上限。像素层收到即忽略。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerHtml {
    pub v: u32,
    pub layer: u64,
    pub html: String,
}

impl LayerHtml {
    pub fn new(layer: u64, html: impl Into<String>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            layer,
            html: html.into(),
        }
    }
}

/// 双向：html 表面的不透明邮箱。`name` / `body` 是插件与它自己页面的约定，
/// 宿主原样转发，不做名字分派（没有 save/dirty 等内核名词）。
/// 像素层：插件→宿主忽略；宿主不会从像素层发出本消息。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerMsg {
    pub v: u32,
    pub layer: u64,
    pub name: String,
    pub body: String,
}

impl LayerMsg {
    pub fn new(layer: u64, name: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            layer,
            name: name.into(),
            body: body.into(),
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

/// 宿主→插件：像素层内的鼠标。坐标是层视图像素，原点左上，与
/// [`LayerReady`] 的 `width_px`/`height_px` 同一空间。html 表面鼠标留在
/// WebKit，不发本消息。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputMouse {
    pub v: u32,
    pub layer: u64,
    pub button: MouseButton,
    pub action: MouseAction,
    pub x_px: u32,
    pub y_px: u32,
    pub modifiers: Vec<Modifier>,
}

impl InputMouse {
    pub fn new(
        layer: u64,
        button: MouseButton,
        action: MouseAction,
        x_px: u32,
        y_px: u32,
        modifiers: Vec<Modifier>,
    ) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            layer,
            button,
            action,
            x_px,
            y_px,
            modifiers,
        }
    }
}

/// 宿主→插件：像素层滚轮。`dx`/`dy` 是整型 delta（正 `dy` = 手指向上/
/// 内容向下的宿主原样符号，插件自己解释）；html 表面不发。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputScroll {
    pub v: u32,
    pub layer: u64,
    pub dx: i32,
    pub dy: i32,
    pub modifiers: Vec<Modifier>,
}

impl InputScroll {
    pub fn new(layer: u64, dx: i32, dy: i32, modifiers: Vec<Modifier>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            layer,
            dx,
            dy,
            modifiers,
        }
    }
}

/// 宿主→插件：层焦点变化。`focused` 是 JSON boolean。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputFocus {
    pub v: u32,
    pub layer: u64,
    pub focused: bool,
}

impl InputFocus {
    pub fn new(layer: u64, focused: bool) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            layer,
            focused,
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
// pane：宿主推终端面快照；插件把文本写入 PTY
// ---------------------------------------------------------------------------

/// 一个终端叶子（PTY 面）的快照。`window`/`tab`/`leaf` 是与
/// `window-save-state` 恢复顺序一致的槽位（只计带 PTY 的标签）。
/// `fg_pid == 0` 表示前台进程未知。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane: u32,
    pub window: u32,
    pub tab: u32,
    pub leaf: u32,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub fg_pid: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
}

impl PaneInfo {
    pub fn new(
        pane: u32,
        window: u32,
        tab: u32,
        leaf: u32,
        cwd: impl Into<String>,
        fg_pid: u32,
    ) -> Self {
        Self {
            pane,
            window,
            tab,
            leaf,
            cwd: cwd.into(),
            fg_pid,
            title: String::new(),
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }
}

/// 宿主→插件：当前所有终端 pane。活面的 pane/前台 pid/cwd 变了才推；
/// 宿主退出前再推一次。插件据此记录各窗正在跑的 CLI agent，并在
/// 宿主重启后按槽位把 resume 命令写回 PTY。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneSnapshot {
    pub v: u32,
    pub panes: Vec<PaneInfo>,
}

impl PaneSnapshot {
    pub fn new(panes: Vec<PaneInfo>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            panes,
        }
    }
}

/// 插件→宿主：把 `text` 写入指定 pane 的 PTY（如同用户键入）。
/// 宿主找不到 pane 时忽略（不断连）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneInput {
    pub v: u32,
    pub pane: u32,
    pub text: String,
}

impl PaneInput {
    pub fn new(pane: u32, text: impl Into<String>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            pane,
            text: text.into(),
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
    #[serde(rename = "layer.html")]
    LayerHtml(LayerHtml),
    #[serde(rename = "layer.msg")]
    LayerMsg(LayerMsg),
    #[serde(rename = "input.hotkey")]
    InputHotkey(InputHotkey),
    #[serde(rename = "input.hotkey.granted")]
    InputHotkeyGranted(InputHotkeyGranted),
    #[serde(rename = "input.hotkey.denied")]
    InputHotkeyDenied(InputHotkeyDenied),
    #[serde(rename = "input.key")]
    InputKey(InputKey),
    #[serde(rename = "input.mouse")]
    InputMouse(InputMouse),
    #[serde(rename = "input.scroll")]
    InputScroll(InputScroll),
    #[serde(rename = "input.focus")]
    InputFocus(InputFocus),
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
    #[serde(rename = "pane.snapshot")]
    PaneSnapshot(PaneSnapshot),
    #[serde(rename = "pane.input")]
    PaneInput(PaneInput),
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
    "layer.html",
    "layer.msg",
    "input.hotkey",
    "input.hotkey.granted",
    "input.hotkey.denied",
    "input.key",
    "input.mouse",
    "input.scroll",
    "input.focus",
    "spawn.request",
    "spawn.started",
    "spawn.denied",
    "spawn.exited",
    "config.push",
    "theme.set",
    "pane.snapshot",
    "pane.input",
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
            Message::LayerHtml(m) => m.v,
            Message::LayerMsg(m) => m.v,
            Message::InputHotkey(m) => m.v,
            Message::InputHotkeyGranted(m) => m.v,
            Message::InputHotkeyDenied(m) => m.v,
            Message::InputKey(m) => m.v,
            Message::InputMouse(m) => m.v,
            Message::InputScroll(m) => m.v,
            Message::InputFocus(m) => m.v,
            Message::SpawnRequest(m) => m.v,
            Message::SpawnStarted(m) => m.v,
            Message::SpawnDenied(m) => m.v,
            Message::SpawnExited(m) => m.v,
            Message::ConfigPush(m) => m.v,
            Message::ThemeSet(m) => m.v,
            Message::PaneSnapshot(m) => m.v,
            Message::PaneInput(m) => m.v,
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
            Message::LayerHtml(_) => "layer.html",
            Message::LayerMsg(_) => "layer.msg",
            Message::InputHotkey(_) => "input.hotkey",
            Message::InputHotkeyGranted(_) => "input.hotkey.granted",
            Message::InputHotkeyDenied(_) => "input.hotkey.denied",
            Message::InputKey(_) => "input.key",
            Message::InputMouse(_) => "input.mouse",
            Message::InputScroll(_) => "input.scroll",
            Message::InputFocus(_) => "input.focus",
            Message::SpawnRequest(_) => "spawn.request",
            Message::SpawnStarted(_) => "spawn.started",
            Message::SpawnDenied(_) => "spawn.denied",
            Message::SpawnExited(_) => "spawn.exited",
            Message::ConfigPush(_) => "config.push",
            Message::ThemeSet(_) => "theme.set",
            Message::PaneSnapshot(_) => "pane.snapshot",
            Message::PaneInput(_) => "pane.input",
        }
    }

    /// 所属七类之一：`hit` / `layer` / `input` / `spawn` / `config` /
    /// `theme` / `pane`（type 的第一个 `.` 前段）。
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
            | Message::InputMouse(_)
            | Message::InputScroll(_)
            | Message::InputFocus(_)
            | Message::SpawnStarted(_)
            | Message::SpawnDenied(_)
            | Message::SpawnExited(_)
            | Message::ConfigPush(_)
            | Message::PaneSnapshot(_) => Direction::HostToPlugin,
            Message::HitClaim(_)
            | Message::HitIgnore(_)
            | Message::LayerOpen(_)
            | Message::LayerPresent(_)
            | Message::LayerHtml(_)
            | Message::InputHotkey(_)
            | Message::SpawnRequest(_)
            | Message::ThemeSet(_)
            | Message::PaneInput(_) => Direction::PluginToHost,
            Message::LayerClose(_) | Message::LayerMsg(_) => Direction::Both,
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
            Message::LayerHtml(LayerHtml::new(3, "<p>hi</p>")),
            Message::LayerMsg(LayerMsg::new(3, "ping", "{}")),
            Message::InputHotkey(InputHotkey::new(9, "p", vec![Modifier::Cmd])),
            Message::InputHotkeyGranted(InputHotkeyGranted::new(9)),
            Message::InputHotkeyDenied(InputHotkeyDenied::new(9, "已被另一个插件占用")),
            Message::InputKey(InputKey::new(3, "esc", "", vec![])),
            Message::InputMouse(InputMouse::new(
                3,
                MouseButton::Left,
                MouseAction::Down,
                12,
                34,
                vec![],
            )),
            Message::InputScroll(InputScroll::new(3, 0, 1, vec![])),
            Message::InputFocus(InputFocus::new(3, true)),
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
            Message::PaneSnapshot(PaneSnapshot::new(vec![PaneInfo::new(
                2,
                0,
                1,
                0,
                "/Users/jal/demo",
                4242,
            )
            .with_title("ninja")])),
            Message::PaneInput(PaneInput::new(2, "pi --session 01a02485\n")),
        ]
    }
}

/// type 字符串是否属于本版本（版本内 type 集冻结）。
pub fn is_known_type(type_: &str) -> bool {
    KNOWN_TYPES.contains(&type_)
}
