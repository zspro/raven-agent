//! 提供商相关类型

use serde::{Deserialize, Serialize};

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
