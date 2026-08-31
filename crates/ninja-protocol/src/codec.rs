//! JSON 编解码 + 双侧策略入口。
//!
//! - 宿主侧 [`Message::decode_host`]：lenient——未知 JSON 字段忽略
//!   （serde 默认，无 `deny_unknown_fields`）；`v` 与 `type` 仍强校验。
//! - 插件侧 [`Message::decode_plugin`]：同一解码，但版本门是硬规则——
//!   返回 [`DecodeError::UnsupportedVersion`] 时**插件必须立即退出**
//!   （stderr 一行 + 非零退出码 78），不许降级、不许猜。
//!
//! 两入口返回同一错误集；区别在嵌入方的处置策略（宿主断开连接，
//! 插件退出进程）。编码侧 [`Message::to_json`] 同样门版本：构造错 `v`
//! 的消息编码失败，而不是带着错版本上线。

use serde::Deserialize;

use crate::message::{Message, PROTOCOL_VERSION, is_known_type};

/// 解码错误。前四类都是「不能猜」的硬错误。
#[derive(Clone, Debug, PartialEq)]
pub enum DecodeError {
    /// 不是合法 JSON / 字段类型对不上 / 必填字段缺失（v、type 之外）。
    InvalidJson(String),
    /// 顶层没有 `v`。信封不完整。
    MissingVersion,
    /// `v` 不是本实现说的版本。插件必须退出；宿主断开连接。
    UnsupportedVersion { got: u32, supported: u32 },
    /// 顶层没有 `type`。
    MissingType,
    /// `type` 不在本版本集合内（同 v 内 type 集冻结，见 crate 文档）。
    UnknownType(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::InvalidJson(e) => write!(f, "无效 JSON：{e}"),
            DecodeError::MissingVersion => write!(f, "信封缺 v"),
            DecodeError::UnsupportedVersion { got, supported } => write!(
                f,
                "协议版本不符：收到 v={got}，本实现只说 v={supported}（不猜，插件须退出）"
            ),
            DecodeError::MissingType => write!(f, "信封缺 type"),
            DecodeError::UnknownType(t) => write!(f, "未知 type {t:?}（同 v 内 type 集冻结）"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// 编码错误：只有「手工构造了错版本的消息」一种。
#[derive(Clone, Debug, PartialEq)]
pub enum EncodeError {
    WrongVersion { got: u32, supported: u32 },
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::WrongVersion { got, supported } => write!(
                f,
                "拒发错版本消息：v={got}，只发 v={supported}（用 new() 构造可避免）"
            ),
        }
    }
}

impl std::error::Error for EncodeError {}

/// 信封探针：先只取 `v` / `type` 判版本与类型，再做全量解析。
/// 未知字段（及未知 type）在此层被忽略——真正的分派在下面。
#[derive(Deserialize)]
struct EnvelopeProbe {
    v: Option<u32>,
    #[serde(rename = "type")]
    type_: Option<String>,
}

impl Message {
    /// 编码为 JSON 文本（紧凑、单行）。版本门：`v` 必须等于
    /// [`PROTOCOL_VERSION`]，否则 [`EncodeError::WrongVersion`]。
    pub fn to_json(&self) -> Result<String, EncodeError> {
        if self.v() != PROTOCOL_VERSION {
            return Err(EncodeError::WrongVersion {
                got: self.v(),
                supported: PROTOCOL_VERSION,
            });
        }
        serde_json::to_string(self).map_err(|_| EncodeError::WrongVersion {
            got: PROTOCOL_VERSION,
            supported: PROTOCOL_VERSION,
        })
    }

    /// 从 JSON 文本解码（策略中性；见 [`Message::decode_host`] /
    /// [`Message::decode_plugin`]）。顺序：先信封（v → type）后全量，
    /// 保证版本错误永远优先于字段错误——版本都不对，字段报错没有意义。
    pub fn from_json(text: &str) -> Result<Message, DecodeError> {
        let probe: EnvelopeProbe =
            serde_json::from_str(text).map_err(|e| DecodeError::InvalidJson(e.to_string()))?;
        match probe.v {
            None => return Err(DecodeError::MissingVersion),
            Some(v) if v != PROTOCOL_VERSION => {
                return Err(DecodeError::UnsupportedVersion {
                    got: v,
                    supported: PROTOCOL_VERSION,
                });
            }
            Some(_) => {}
        }
        match &probe.type_ {
            None => return Err(DecodeError::MissingType),
            Some(t) if !is_known_type(t) => return Err(DecodeError::UnknownType(t.clone())),
            Some(_) => {}
        }
        serde_json::from_str(text).map_err(|e| DecodeError::InvalidJson(e.to_string()))
    }

    /// **宿主侧**入口（bytes = 帧载荷 JSON 字节）。lenient：未知字段
    /// 忽略；`v` 不符 / 未知 `type` / 坏 JSON → 错误（宿主处置：断开
    /// 该插件连接）。
    pub fn decode_host(bytes: &[u8]) -> Result<Message, DecodeError> {
        let text = std::str::from_utf8(bytes)
            .map_err(|e| DecodeError::InvalidJson(format!("帧载荷不是 UTF-8：{e}")))?;
        Message::from_json(text)
    }

    /// **插件侧**入口。解码同上；不同在处置契约：任何
    /// [`DecodeError::UnsupportedVersion`] 都意味着双方说的不是同一种
    /// 协议——**插件必须立即退出**（stderr 一行 + 非零退出码），不降级、
    /// 不猜旧格式。这是 PLAN「插件遇不支持的 v 必须退出」的代码落点。
    pub fn decode_plugin(bytes: &[u8]) -> Result<Message, DecodeError> {
        // 解码与宿主侧一致；差异是嵌入方对错误的处置（退出 vs 断连）。
        Message::decode_host(bytes)
    }
}
