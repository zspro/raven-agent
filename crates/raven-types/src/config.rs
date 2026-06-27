//! 配置类型（主配置 + 各子配置 + Git-first + MCP）

use crate::ProviderConfig;
use serde::{Deserialize, Serialize};

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

/// 默认允许的 shell 命令集（按平台区分）。
///
/// Windows 与类 Unix 的常用命令不同（如 Windows 用 `dir`/`type`，
/// Unix 用 `ls`/`cat`），这里按编译目标平台给出对应默认集，
/// 避免在 Windows 上把一堆 Unix 命令塞进白名单导致全部不可用。
fn default_allowed_shell_commands() -> Vec<String> {
    #[cfg(windows)]
    let cmds: &[&str] = &[
        "dir", "type", "findstr", "where", "git", "go", "npm", "node", "echo", "cd", "more",
        "tree", "curl", "python", "cargo",
    ];
    #[cfg(not(windows))]
    let cmds: &[&str] = &[
        "ls", "cat", "grep", "find", "git", "go", "npm", "node", "echo", "pwd", "head", "tail",
        "wc", "mkdir", "touch", "cp", "mv", "curl",
    ];
    cmds.iter().map(|s| s.to_string()).collect()
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
