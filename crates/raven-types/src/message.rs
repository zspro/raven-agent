//! 消息与工具调用类型

use crate::estimate_tokens;
use serde::{Deserialize, Serialize};
use std::fmt;

/// 消息角色
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Role::System => write!(f, "system"),
            Role::User => write!(f, "user"),
            Role::Assistant => write!(f, "assistant"),
            Role::Tool => write!(f, "tool"),
        }
    }
}

/// 对话中的一条消息
/// 兼容 OpenAI API 格式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn tool_result(
        call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(call_id.into()),
            name: Some(name.into()),
        }
    }

    /// 估算此消息的 token 数
    pub fn estimate_tokens(&self) -> usize {
        estimate_tokens(&self.content)
            + self.tool_calls.as_ref().map_or(0, |tc| {
                tc.iter()
                    .map(|t| {
                        estimate_tokens(&t.function.name) + estimate_tokens(&t.function.arguments)
                    })
                    .sum()
            })
    }
}

// =============================================================================
// 工具调用类型
// =============================================================================

/// 模型请求调用的工具
///
/// 注意：`id` / `call_type` / `function.name` 都有 `#[serde(default)]`，
/// 因为 OpenAI streaming 的 tool call delta 分块只在首包包含这些字段，
/// 后续增量包只有 `index` + `function.arguments`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(default)]
    pub index: usize,
    #[serde(default)]
    pub id: String,
    #[serde(default, rename = "type")]
    pub call_type: String,
    pub function: ToolCallFunction,
}

/// 工具调用的函数信息
#[derive(Debug, Clone, Serialize)]
pub struct ToolCallFunction {
    pub name: String,
    /// 工具参数 JSON。序列化时始终输出为 JSON 字符串；
    /// 反序列化时兼容两种格式：JSON 字符串（OpenAI 标准）和 JSON 对象（NewAPI/OneAPI 等代理）。
    #[serde(deserialize_with = "deserialize_arguments")]
    pub arguments: String,
}

/// 自定义反序列化：arguments 可能是 JSON 字符串，也可能是 JSON 对象
fn deserialize_arguments<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    match v {
        // 标准 OpenAI 格式: "arguments": "{\"command\": \"ls\"}"
        serde_json::Value::String(s) => Ok(s),
        // NewAPI/OneAPI 代理格式: "arguments": {"command": "ls"}
        other => Ok(other.to_string()),
    }
}

// 手动实现 Deserialize（因为自定义字段反序列化器与 derive 冲突）
impl<'de> Deserialize<'de> for ToolCallFunction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Helper {
            #[serde(default)]
            name: String,
            #[serde(deserialize_with = "deserialize_arguments")]
            arguments: String,
        }
        let h = Helper::deserialize(deserializer)?;
        Ok(ToolCallFunction {
            name: h.name,
            arguments: h.arguments,
        })
    }
}

/// 工具执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub name: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

/// 工具定义（发送给模型）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    #[serde(rename = "type")]
    pub schema_type: String,
    pub function: FunctionSchema,
}

/// 函数定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}
