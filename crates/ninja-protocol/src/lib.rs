//! ninja-protocol：ADE 协议 v0（q3 按 PRODUCT/PLAN 重钉；消息集与线语义
//! 对照旧树 1240428:crates/ninja-protocol 移植）。
//!
//! 进程外、版本化、六类消息（hit / layer / input / spawn / config / theme）。
//! 宿主与插件是两个进程，只经 Unix socket 交换字节，**永远不共享地址
//! 空间**——本 crate 只是把线格式钉成 Rust 类型 + 编解码，不携带任何
//! 宿主内部 API。第二个实现可以不用 Rust：只认下述 JSON。
//!
//! # 线格式（wire format）
//!
//! 每条消息是一帧：`u32le 长度前缀 + UTF-8 JSON`。
//!
//! ```text
//! ┌────────────┬──────────────────────────────┐
//! │ u32 LE     │ JSON 字节（UTF-8，无 BOM）    │
//! │ len(JSON)  │ 即消息本体                    │
//! └────────────┴──────────────────────────────┘
//! ```
//!
//! - 前缀只计 JSON 字节数，不含前缀自身（4 字节）。
//! - 单帧上限 [`frame::MAX_FRAME_BYTES`]（8 MiB）：超限即协议违规，
//!   收方关连接，不试图续读。
//! - JSON 是对象；字段顺序与空白对解码无意义；宿主侧编码按结构体
//!   声明顺序输出（golden 测试钉死字节形态）。
//! - 数值类型只有 u64/u32/i32 与字符串，无浮点、无 NaN。
//!
//! # 信封不变量
//!
//! 每条消息的顶层对象必含两个字段：
//!
//! - `v`：协议版本，u32。本版本恒为 [`PROTOCOL_VERSION`]（0）。
//! - `type`：消息类型字符串（见下表）。
//!
//! # 版本与演化规则
//!
//! 1. 同一 `v` 内消息类型集合**冻结**：要加新消息类型必须升 `v`。
//! 2. 同一 `v` 内已有类型的字段集也视为冻结；但收方对未知字段一律
//!    忽略（容忍实现漂移，不依赖）。
//! 3. `v` 不符 = 两种实现说的不是同一种协议，**禁止猜测**：
//!    - 插件侧：必须立即退出（stderr 打一行原因 + 退出码 78），
//!      见 [`Message::decode_plugin`]。不能降级、不能猜旧格式。
//!    - 宿主侧：记录并断开该连接（[`Message::decode_host`] 返回
//!      [`DecodeError::UnsupportedVersion`]）。
//! 4. `type` 不认识 = 同版本协议违规：解码错误。不猜。
//! 5. **v0 内字段集修订记录**：`hit` 含 `cwd` 字段（string，缺省
//!    `""`）。v0 尚未对外发布（仓库未公开、无第二实现部署），字段集
//!    直接钉进 v0：新增字段只增不删不改，配合规则 2 的「未知字段忽略」
//!    与反序列化缺省，旧实现在新线格式上行为不变。原因：进程外插件
//!    无法访问宿主的 OSC-7 pwd 状态，没有 `cwd` 就永远无法认领相对
//!    路径（golden 样例 `src/main.rs:42:13` 正是相对路径）。此条不并
//!    成通例：仓库公开后再改字段集必须升 `v`。
//! 6. **v0 内消息类型增补记录**：`theme.set`（插件→宿主，携带完整
//!    色板）。依据规则 1 本应升 `v`，但 v0 尚未对外发布（同第 5 条
//!    前提），增补直接钉进 v0；只增不删不改。引入原因是 PRODUCT 的
//!    「颜色」原语：宿主内置 One Dark Pro 为不可卸基线配色，主题切换
//!    走插件原语——宿主不做内置主题系统/切换 UI，换色是插件经协议推
//!    完整色板。旧实现（不认识 `theme.set`）在新线格式上行为不变：
//!    插件侧旧实现收不到这条消息（只有插件发）；宿主侧旧实现会按未
//!    知 type 拒收（规则 4），可接受的迁移面。此条同样不并成通例。
//!
//! # 六类消息总表
//!
//! `方向`：宿主→插件 / 插件→宿主 / 双向。除公共 `v`/`type` 外的字段：
//!
//! | type | 方向 | 字段 |
//! |---|---|---|
//! | [`hit`](Message::Hit) | 宿主→插件 | `id` u64、`kind`（"path"/"url"/"osc8"）、`text` string、`cwd` string（相对路径解析基，空 = 未知；见规则 5）、`row` u32、`col` u32、`pane` u32、`modifiers` \[[`Modifier`]\] |
//! | [`hit.claim`](Message::HitClaim) | 插件→宿主 | `id` u64（回执）、`priority` u32（多插件竞争，大者胜） |
//! | [`hit.ignore`](Message::HitIgnore) | 插件→宿主 | `id` u64 |
//! | [`layer.open`](Message::LayerOpen) | 插件→宿主 | `id` u64、`placement`（"overlay"/"side"）、`anchor_row` u32、`anchor_col` u32 |
//! | [`layer.ready`](Message::LayerReady) | 宿主→插件 | `id` u64（回执）、`layer` u64（层句柄）、`width_px` u32、`height_px` u32、`dpi` u32、`io_surface_id` u64 |
//! | [`layer.present`](Message::LayerPresent) | 插件→宿主 | `layer` u64 |
//! | [`layer.close`](Message::LayerClose) | 双向 | `layer` u64 |
//! | [`input.hotkey`](Message::InputHotkey) | 插件→宿主 | `id` u64、`key` string、`modifiers` \[[`Modifier`]\] |
//! | [`input.hotkey.granted`](Message::InputHotkeyGranted) | 宿主→插件 | `id` u64 |
//! | [`input.hotkey.denied`](Message::InputHotkeyDenied) | 宿主→插件 | `id` u64、`reason` string |
//! | [`input.key`](Message::InputKey) | 宿主→插件 | `layer` u64（0 = 无层上下文的全局热键触发，宿主适配器语义）、`key` string、`text` string（""=无）、`modifiers` \[[`Modifier`]\] |
//! | [`spawn.request`](Message::SpawnRequest) | 插件→宿主 | `id` u64、`argv` \[string\]、`memory_limit_bytes` u64（0=宿主默认） |
//! | [`spawn.started`](Message::SpawnStarted) | 宿主→插件 | `id` u64、`pid` u32 |
//! | [`spawn.denied`](Message::SpawnDenied) | 宿主→插件 | `id` u64、`reason` string |
//! | [`spawn.exited`](Message::SpawnExited) | 宿主→插件 | `id` u64、`pid` u32、`code` i32 |
//! | [`config.push`](Message::ConfigPush) | 宿主→插件 | `enabled` \[string\]、`keys` map&lt;string,string&gt;、`memory_limit_bytes` u64 |
//! | [`theme.set`](Message::ThemeSet) | 插件→宿主 | `name` string、`bg`/`fg`/`cursor`/`selection_bg`/`divider` string（`#rrggbb`）、`selection_alpha` u32（0-255）、`ansi` \[string;16\]（恰好 16 个 `#rrggbb`；见规则 6） |
//!
//! 语义要点：
//!
//! - `hit`：宿主在 vt cell 上认出路径/URL/OSC-8 后广播；插件回
//!   `claim`/`ignore`；全 `ignore` 或无插件 → 系统默认打开。
//! - `layer`：插件 `layer.open` 要层 → 宿主回尺寸/DPI/IOSurface
//!   （`io_surface_id` 是 IOSurface global ID）→ 插件画完发
//!   `layer.present`。
//! - `input`：插件申请全局快捷键；层在前台时键盘事件先发该插件。
//! - `spawn`：辅助进程由宿主代拉、宿主管生命周期与内存上限。
//! - `config`：启用列表/键位/内存上限，只读推送。
//! - `theme`：插件推完整色板换宿主当前生效配色；宿主内置基线色板
//!   （One Dark Pro）不可卸——插件连接死亡/禁用时宿主回退基线。色值
//!   一律 `#rrggbb`；语义坏值（格式/alpha 越界）由宿主整条忽略（不断
//!   连），类型/数量错按解码错误处置。
//!
//! # 枚举与命名集
//!
//! - `modifiers` 数组元素：`"shift"` / `"ctrl"` / `"alt"` / `"cmd"`
//!   （[`Modifier`]）。
//! - `hit.kind`：`"path"` / `"url"` / `"osc8"`（[`HitKind`]）。
//! - `placement`：`"overlay"`（盖在 cell 上）/ `"side"`（侧开）（[`Placement`]）。
//! - `theme.set` 的颜色字段：`#` + 恰好 6 位十六进制（大小写均可，
//!   golden 钉小写）；不收 `#abc` 短写/`0x` 前缀（宿主解析失败即整条
//!   忽略，见 [`Message::ThemeSet`] 文档）。
//! - `key` 字符串：单字符（如 `"p"`）或命名键 `left` `right` `up` `down`
//!   `home` `end` `pageup` `pagedown` `delete` `backspace` `tab` `enter`
//!   `esc` `f1`…`f12`。集合冻结；新键名升 `v`。
//!
//! # Socket 约定（macOS）
//!
//! - 路径：`${TMPDIR:-/tmp}/ninja-ade-{pid}.sock`（宿主侧见
//!   `ninja::plugins`；`NINJA_ADE_SOCK` 可覆盖，测试钩子）。
//! - 宿主拉起插件进程时通过环境变量 `NINJA_ADE_SOCK` 告知路径。
//! - 空载（无插件启用）不创建 socket、不拉任何插件进程。
//!
//! # 第二语言实现指南
//!
//! 只靠本文档 + `tests/golden/*.json`（每条消息一个钉死的字节形态）
//! 即可写出解码器：读 4 字节小端长度 → 读等长字节 → 按 `v` 门（不符
//! 即退出）→ 按 `type` 分派 → 取字段，未知字段丢弃。
//! `tests/second_language_decode.py` 是最小 Python 参考解码器（验证用，
//! 不进产品）。Rust 侧入口：
//!
//! ```
//! use ninja_protocol::*;
//!
//! // new() 钉 v=PROTOCOL_VERSION，忘不了。
//! let msg = Message::Hit(Hit::new(
//!     7, HitKind::Path, "src/main.rs:42", "/Users/jal/demo", 41, 0, 2,
//!     vec![Modifier::Cmd],
//! ));
//! let json = msg.to_json().unwrap();
//! assert!(json.contains(r#""v":0"#) && json.contains(r#""type":"hit""#));
//!
//! // 往返：帧编码 → 流式喂入 → 逐帧弹出 → 解码。
//! let frame = frame::encode_frame(&msg).unwrap();
//! let mut dec = frame::FrameDecoder::new();
//! dec.extend(&frame).unwrap();
//! let payload = dec.pop().unwrap().unwrap();
//! assert_eq!(Message::decode_host(&payload).unwrap(), msg);
//! ```
//!
//! 依赖方向：宿主 `ninja` 与示例插件 `ninja-preview`/`ninja-theme`
//! 可以依赖本 crate（纯 serde 数据类型），本 crate 永不依赖它们；
//! 插件永不依赖宿主 `ninja`（验收：`cargo tree -p <插件>` 无宿主
//! crate、无 ghostty-sys）。

pub mod codec;
pub mod frame;
pub mod message;

pub use codec::{DecodeError, EncodeError};
pub use frame::{FrameDecoder, FrameError, MAX_FRAME_BYTES, encode_frame};
pub use message::{
    ConfigPush, Direction, Hit, HitClaim, HitIgnore, HitKind, InputHotkey, InputHotkeyDenied,
    InputHotkeyGranted, InputKey, KNOWN_TYPES, LayerClose, LayerOpen, LayerPresent, LayerReady,
    Message, Modifier, PROTOCOL_VERSION, Placement, SpawnDenied, SpawnExited, SpawnRequest,
    SpawnStarted, ThemeSet, is_known_type,
};
