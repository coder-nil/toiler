//! v3 会话文件的错误类型。
//!
//! 刻意做得很小：v3 兼容层的失败面只有「行的形状不对」和「IO 出错」两类，
//! 都能带上足够定位的行号信息。

use std::fmt;

/// v3 编解码错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V3Error {
    /// 不是合法 JSON。
    Syntax { line: usize, message: String },
    /// 是 JSON，但形状不符合 v3。
    Schema { line: usize, message: String },
    /// 时间戳不是 `...Z` 形状。
    Timestamp { line: usize, value: String },
    /// 该条目类型无法用 v3 表达（例如 rpi 的 active_tools_change）。
    Unrepresentable { entry_type: String },
}

impl V3Error {
    pub(crate) fn syntax(line: usize, message: impl Into<String>) -> Self {
        Self::Syntax {
            line,
            message: message.into(),
        }
    }

    pub(crate) fn schema(line: usize, message: impl Into<String>) -> Self {
        Self::Schema {
            line,
            message: message.into(),
        }
    }
}

impl fmt::Display for V3Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            V3Error::Syntax { line, message } => {
                write!(f, "v3 第 {line} 行不是合法 JSON：{message}")
            }
            V3Error::Schema { line, message } => {
                write!(f, "v3 第 {line} 行形状不对：{message}")
            }
            V3Error::Timestamp { line, value } => {
                write!(f, "v3 第 {line} 行时间戳无法解析：{value}")
            }
            V3Error::Unrepresentable { entry_type } => {
                write!(f, "v3 没有 {entry_type} 这种条目类型，无法写入")
            }
        }
    }
}

impl std::error::Error for V3Error {}
