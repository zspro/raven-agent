//! # agent-types
//!
//! 跨模块共享的核心类型定义。
//! 所有 crate 都依赖此模块，避免循环依赖。

use serde::{Deserialize, Serialize};
use std::fmt;

#[cfg(test)]
mod tests;

// =============================================================================
// 消息类型
// =============================================================================

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

// =============================================================================
// 模型响应类型
// =============================================================================

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

// =============================================================================
// 提供商相关类型
// =============================================================================

/// 提供商配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub headers: std::collections::HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
}

/// 模型信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub provider: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,
    #[serde(default)]
    pub supports_tools: bool,
    #[serde(default)]
    pub supports_vision: bool,
}

fn default_max_tokens() -> usize {
    128_000
}

/// 提供商验证结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderVerification {
    pub provider: String,
    pub verified: bool,
    pub features: ProviderFeatures,
    pub latency_ms: u64,
    pub models: Vec<String>,
    pub fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 提供商功能
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderFeatures {
    pub streaming: bool,
    pub tool_calling: bool,
    pub json_mode: bool,
    pub vision: bool,
    pub system_prompt: bool,
}

// =============================================================================
// 配置类型
// =============================================================================

/// 主配置（所有字段都有默认值 = 零配置可用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default)]
    pub permission: PermissionConfig,
    #[serde(default)]
    pub context: ContextConfig,
    #[serde(default)]
    pub token_budget: usize,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub git_first: GitFirstConfig,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: default_model(),
            api_key: None,
            base_url: None,
            permission: PermissionConfig::default(),
            context: ContextConfig::default(),
            token_budget: 0,
            tools: ToolsConfig::default(),
            log_level: default_log_level(),
            providers: Vec::new(),
            server: ServerConfig::default(),
            git_first: GitFirstConfig::default(),
            mcp_servers: Vec::new(),
        }
    }
}

fn default_model() -> String {
    "gpt-4o".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

/// 权限配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionConfig {
    #[serde(default = "default_permission_mode")]
    pub mode: String,
    #[serde(default = "default_allowed_tools")]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub denied_tools: Vec<String>,
}

impl Default for PermissionConfig {
    fn default() -> Self {
        Self {
            mode: default_permission_mode(),
            allowed_tools: default_allowed_tools(),
            denied_tools: Vec::new(),
        }
    }
}

fn default_permission_mode() -> String {
    "ask".to_string()
}

fn default_allowed_tools() -> Vec<String> {
    vec![
        "file_read".to_string(),
        "file_write".to_string(),
        "file_edit".to_string(),
        "view".to_string(),
        "search".to_string(),
        "list_dir".to_string(),
        "git".to_string(),
    ]
}

/// 上下文配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextConfig {
    #[serde(default = "default_max_context_tokens")]
    pub max_tokens: usize,
    #[serde(default = "default_compact_threshold")]
    pub compact_threshold: usize,
    #[serde(default = "default_keep_rounds")]
    pub keep_rounds: usize,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            max_tokens: default_max_context_tokens(),
            compact_threshold: default_compact_threshold(),
            keep_rounds: default_keep_rounds(),
        }
    }
}

fn default_max_context_tokens() -> usize {
    128_000
}

fn default_compact_threshold() -> usize {
    100_000
}

fn default_keep_rounds() -> usize {
    6
}

/// 工具配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsConfig {
    #[serde(default = "default_enabled_tools")]
    pub enabled: Vec<String>,
    #[serde(default)]
    pub shell: ShellConfig,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled_tools(),
            shell: ShellConfig::default(),
        }
    }
}

fn default_enabled_tools() -> Vec<String> {
    vec![
        "file_read".to_string(),
        "file_write".to_string(),
        "shell".to_string(),
        "search".to_string(),
        "list_dir".to_string(),
        "git".to_string(),
    ]
}

/// Shell 工具配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_shell_timeout")]
    pub timeout: u64,
    #[serde(default = "default_allowed_shell_commands")]
    pub allowed: Vec<String>,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout: default_shell_timeout(),
            allowed: default_allowed_shell_commands(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_shell_timeout() -> u64 {
    30
}

fn default_allowed_shell_commands() -> Vec<String> {
    vec![
        "ls".to_string(),
        "cat".to_string(),
        "grep".to_string(),
        "find".to_string(),
        "git".to_string(),
        "go".to_string(),
        "npm".to_string(),
        "node".to_string(),
        "echo".to_string(),
        "pwd".to_string(),
        "head".to_string(),
        "tail".to_string(),
        "wc".to_string(),
        "mkdir".to_string(),
        "touch".to_string(),
        "cp".to_string(),
        "mv".to_string(),
        "curl".to_string(),
    ]
}

/// HTTP 服务器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
        }
    }
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    8080
}

// =============================================================================
// Git-first 配置
// =============================================================================

/// Git-first 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitFirstConfig {
    /// 是否启用 Git-first 设计
    #[serde(default = "default_git_first_enabled")]
    pub enabled: bool,
    /// 是否自动提交（false=只add不commit，手动确认后再commit）
    #[serde(default = "default_git_first_auto")]
    pub auto_commit: bool,
    /// 提交信息前缀
    #[serde(default = "default_git_first_prefix")]
    pub commit_prefix: String,
}

impl Default for GitFirstConfig {
    fn default() -> Self {
        Self {
            enabled: default_git_first_enabled(),
            auto_commit: default_git_first_auto(),
            commit_prefix: default_git_first_prefix(),
        }
    }
}

fn default_git_first_enabled() -> bool {
    true
}

fn default_git_first_auto() -> bool {
    true
}

fn default_git_first_prefix() -> String {
    "raven".to_string()
}

/// MCP Server 配置（外部 Model Context Protocol 服务，通过 stdio 通信）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Server 名称（作为工具前缀：`<name>__<tool>`）
    pub name: String,
    /// 启动命令（如 npx）
    pub command: String,
    /// 命令参数
    #[serde(default)]
    pub args: Vec<String>,
    /// 额外环境变量
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::HashMap<String, String>>,
}

// =============================================================================
// 错误类型
// =============================================================================

/// Agent 框架错误
/// 所有错误都必须分类，提供用户友好的消息和修复建议
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("[配置] {message}\n修复: {fix}")]
    Config { message: String, fix: String },

    #[error("[网络] {message}\n修复: {fix}")]
    Network { message: String, fix: String },

    #[error("[模型] {message}\n修复: {fix}")]
    Model { message: String, fix: String },

    #[error("[权限] {message}\n修复: {fix}")]
    Permission { message: String, fix: String },

    #[error("[预算] {message}\n修复: {fix}")]
    Budget { message: String, fix: String },

    #[error("[内部] {0}")]
    Internal(String),

    #[error("已取消")]
    Cancelled,
}

impl AgentError {
    pub fn config(message: impl Into<String>, fix: impl Into<String>) -> Self {
        Self::Config {
            message: message.into(),
            fix: fix.into(),
        }
    }

    pub fn network(message: impl Into<String>, fix: impl Into<String>) -> Self {
        Self::Network {
            message: message.into(),
            fix: fix.into(),
        }
    }

    pub fn model(message: impl Into<String>, fix: impl Into<String>) -> Self {
        Self::Model {
            message: message.into(),
            fix: fix.into(),
        }
    }

    pub fn permission(tool: impl Into<String>) -> Self {
        let tool = tool.into();
        Self::Permission {
            message: format!("工具 '{}' 未被授权", tool),
            fix: format!(
                "在配置中 '{}.allowed_tools' 添加 '{}' 或切换到 'yes' 模式",
                tool, tool
            ),
        }
    }

    pub fn budget(used: usize, limit: usize) -> Self {
        Self::Budget {
            message: format!("Token 预算已用完: {}/{} (100%)", used, limit),
            fix: "在配置中增加 'token_budget' 或开启新会话".to_string(),
        }
    }
}

// =============================================================================
// 工具函数
// =============================================================================

/// 简单估算文本的 token 数
/// 中文 ≈ 1 字 1 token，英文 ≈ 4 字符 1 token
pub fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }

    let (chinese, other) = text.chars().fold((0, 0), |(c, o), ch| {
        if ch as u32 > 127 {
            (c + 1, o)
        } else {
            (c, o + 1)
        }
    });

    chinese + other / 4 + 1
}

/// 截断过长字符串
pub fn truncate(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        text.to_string()
    } else {
        format!(
            "{}\n... [已截断，共 {} 字符]",
            &text[..max_chars],
            text.len()
        )
    }
}
