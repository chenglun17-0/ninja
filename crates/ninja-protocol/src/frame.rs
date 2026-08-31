//! 帧编解码：`u32le 长度 + UTF-8 JSON`（长度只计 JSON 字节）。
//!
//! socket 读侧用 [`FrameDecoder`] 增量喂数据：流式到达、半帧挂起、
//! 背靠背多帧都正确；写侧 [`encode_frame`] 一条消息一帧。

use crate::codec::EncodeError;
use crate::message::Message;

/// 单帧 JSON 载荷上限（8 MiB）。协议消息都是小对象，超限必是恶意或
/// 错乱流：收方报 [`FrameError::FrameTooLarge`]，嵌入方关连接。
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// 长度前缀宽度（u32le = 4 字节）。
pub const FRAME_PREFIX_BYTES: usize = 4;

/// 帧层错误。
#[derive(Clone, Debug, PartialEq)]
pub enum FrameError {
    /// 声明长度超过 [`MAX_FRAME_BYTES`]（或累计缓冲超限）。
    FrameTooLarge { declared: usize, max: usize },
    /// 长度为 0：空载荷不是合法 JSON 帧。
    EmptyPayload,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::FrameTooLarge { declared, max } => {
                write!(f, "帧长 {declared} 超上限 {max}，关连接")
            }
            FrameError::EmptyPayload => write!(f, "帧长为 0（空载荷非法）"),
        }
    }
}

impl std::error::Error for FrameError {}

/// 编码一条消息为一帧：`u32le(len(json)) || json`。
pub fn encode_frame(msg: &Message) -> Result<Vec<u8>, EncodeError> {
    let json = msg.to_json()?;
    let mut out = Vec::with_capacity(FRAME_PREFIX_BYTES + json.len());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(json.as_bytes());
    Ok(out)
}

/// 增量帧解码器。socket 每读到一段字节就 [`extend`](FrameDecoder::extend)，
/// 每次 [`pop`](FrameDecoder::pop) 弹出一个完整帧的 JSON 载荷（无前缀）；
/// 半帧留在缓冲里等下一段。
///
/// ```
/// use ninja_protocol::{Message, frame};
///
/// let msgs = Message::sample_messages();
/// let mut dec = frame::FrameDecoder::new();
/// let mut wire = Vec::new();
/// for m in &msgs {
///     wire.extend_from_slice(&frame::encode_frame(m).unwrap());
/// }
/// dec.extend(&wire).unwrap();
/// for m in &msgs {
///     assert_eq!(dec.pop().unwrap().unwrap(), m.to_json().unwrap().into_bytes());
/// }
/// assert!(dec.pop().is_none()); // 无残留
/// ```
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一段 socket 字节。累计缓冲超上限即报
    /// [`FrameError::FrameTooLarge`]（防内存炸弹：不等到前缀声明才判）。
    pub fn extend(&mut self, bytes: &[u8]) -> Result<(), FrameError> {
        self.buf.extend_from_slice(bytes);
        if self.buf.len() > MAX_FRAME_BYTES + FRAME_PREFIX_BYTES {
            let declared = self.buf.len();
            self.buf.clear();
            return Err(FrameError::FrameTooLarge {
                declared,
                max: MAX_FRAME_BYTES,
            });
        }
        Ok(())
    }

    /// 弹出下一个完整帧的 JSON 载荷；数据不足（半帧）返回 `None`。
    /// `Some(Err(..))` = 帧级违规（超长 / 空载荷），嵌入方应关连接。
    pub fn pop(&mut self) -> Option<Result<Vec<u8>, FrameError>> {
        if self.buf.len() < FRAME_PREFIX_BYTES {
            return None;
        }
        let len = u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if len == 0 {
            // 丢掉这 4 字节前缀防死循环；嵌入方拿到 Err 后应关连接。
            self.buf.drain(..FRAME_PREFIX_BYTES);
            return Some(Err(FrameError::EmptyPayload));
        }
        if len > MAX_FRAME_BYTES {
            return Some(Err(FrameError::FrameTooLarge {
                declared: len,
                max: MAX_FRAME_BYTES,
            }));
        }
        if self.buf.len() < FRAME_PREFIX_BYTES + len {
            return None; // 半帧，等更多字节
        }
        let payload = self.buf[FRAME_PREFIX_BYTES..FRAME_PREFIX_BYTES + len].to_vec();
        self.buf.drain(..FRAME_PREFIX_BYTES + len);
        Some(Ok(payload))
    }
}
