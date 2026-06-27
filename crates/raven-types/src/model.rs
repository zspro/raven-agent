//! 模型响应与流式事件类型

use crate::ToolCall;
use serde::{Deserialize, Serialize};

/// 模型 API 响应
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: TokenUsage,
    pub model: String,
    pub finish_reason: String,
}

/// Token 使用统计
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(rename = "input_tokens")]
    pub input: usize,
    #[serde(rename = "output_tokens")]
    pub output: usize,
    #[serde(rename = "total_tokens")]
    pub total: usize,
    #[serde(rename = "cached_tokens", skip_serializing_if = "Option::is_none")]
    pub cached: Option<usize>,
}

/// 流式事件（SSE）
/// 使用 flat 结构以便 SSE 序列化
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
}

impl StreamEvent {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            event_type: "text".to_string(),
            content: Some(content.into()),
            usage: None,
        }
    }
    pub fn tool_call(content: impl Into<String>) -> Self {
        Self {
            event_type: "tool_call".to_string(),
            content: Some(content.into()),
            usage: None,
        }
    }
    pub fn tool_result(content: impl Into<String>) -> Self {
        Self {
            event_type: "tool_result".to_string(),
            content: Some(content.into()),
            usage: None,
        }
    }
    pub fn usage(u: TokenUsage) -> Self {
        Self {
            event_type: "usage".to_string(),
            content: None,
            usage: Some(u),
        }
    }
    pub fn done() -> Self {
        Self {
            event_type: "done".to_string(),
            content: None,
            usage: None,
        }
    }
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            event_type: "error".to_string(),
            content: Some(content.into()),
            usage: None,
        }
    }
}
