//! Agent 框架错误类型

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
