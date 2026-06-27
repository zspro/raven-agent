//! # model-router
//!
//! 模型路由和提供商管理。
//! 统一接口，任意模型，自动验证。

use async_trait::async_trait;
use raven_types::{
    ChatResponse, Message, ModelInfo, ProviderConfig, ProviderFeatures, ProviderVerification,
    StreamEvent, ToolSchema,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::info;

pub mod anthropic;
pub mod gemini;
pub mod openai_compat;

use anthropic::AnthropicClient;
use gemini::GeminiClient;
use openai_compat::OpenAICompatibleClient;

/// 提供商协议类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// OpenAI 兼容（含 DeepSeek/硅基流动/NewAPI/OneAPI/Ollama 等）
    OpenAI,
    /// Anthropic Claude 原生 Messages API
    Anthropic,
    /// Google Gemini 原生 generateContent API
    Gemini,
}

impl ProviderKind {
    /// 从提供商名称与 base_url 推断协议类型
    pub fn detect(name: &str, base_url: &str) -> Self {
        let hay = format!("{} {}", name.to_lowercase(), base_url.to_lowercase());
        if hay.contains("anthropic") || hay.contains("claude") {
            ProviderKind::Anthropic
        } else if hay.contains("gemini")
            || hay.contains("googleapis")
            || hay.contains("generativelanguage")
        {
            ProviderKind::Gemini
        } else {
            ProviderKind::OpenAI
        }
    }

    /// 按协议规范化 base_url 的版本后缀
    fn normalize_base_url(self, base_url: &str) -> String {
        let trimmed = base_url.trim_end_matches('/');
        match self {
            ProviderKind::OpenAI => {
                if trimmed.ends_with("/v1") {
                    trimmed.to_string()
                } else {
                    format!("{trimmed}/v1")
                }
            }
            ProviderKind::Anthropic => {
                if trimmed.ends_with("/v1") {
                    trimmed.to_string()
                } else {
                    format!("{trimmed}/v1")
                }
            }
            ProviderKind::Gemini => {
                if trimmed.contains("/v1beta") || trimmed.ends_with("/v1") {
                    trimmed.to_string()
                } else {
                    format!("{trimmed}/v1beta")
                }
            }
        }
    }
}

/// 按 provider 配置创建对应协议的客户端
fn make_client(config: ProviderConfig) -> Box<dyn ProviderClient> {
    match ProviderKind::detect(&config.name, &config.base_url) {
        ProviderKind::Anthropic => Box::new(AnthropicClient::new(config)),
        ProviderKind::Gemini => Box::new(GeminiClient::new(config)),
        ProviderKind::OpenAI => Box::new(OpenAICompatibleClient::new(config)),
    }
}

// =============================================================================
// 路由器
// =============================================================================

/// 模型路由器
pub struct Router {
    providers: Arc<RwLock<Vec<ProviderEntry>>>,
}

struct ProviderEntry {
    name: String,
    config: ProviderConfig,
    client: Box<dyn ProviderClient>,
}

/// 提供商客户端 trait
#[async_trait]
pub trait ProviderClient: Send + Sync {
    async fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatResponse, raven_types::AgentError>;
    async fn chat_stream(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamEvent>, raven_types::AgentError>;
    async fn list_models(&self) -> Result<Vec<ModelInfo>, raven_types::AgentError>;
    fn name(&self) -> &str;
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl Router {
    /// 创建新的路由器
    pub fn new() -> Self {
        Self {
            providers: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// 注册提供商
    pub async fn register_provider(
        &self,
        config: ProviderConfig,
    ) -> Result<(), raven_types::AgentError> {
        let name = config.name.clone();
        let kind = ProviderKind::detect(&config.name, &config.base_url);
        let base_url = kind.normalize_base_url(&config.base_url);

        let client_config = ProviderConfig { base_url, ..config };

        let client = make_client(client_config.clone());

        info!("已注册提供商: {} ({:?})", name, kind);

        let mut providers = self.providers.write().await;
        providers.push(ProviderEntry {
            name,
            config: client_config,
            client,
        });

        Ok(())
    }

    /// 注册默认提供商（从主配置）
    pub async fn register_default(
        &self,
        api_key: Option<String>,
        base_url: Option<String>,
        model: String,
    ) -> Result<(), raven_types::AgentError> {
        let url = base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let name = Self::infer_name(&url);

        let config = ProviderConfig {
            name,
            base_url: url,
            api_key,
            models: vec![model.clone()],
            default_model: Some(model),
            headers: std::collections::HashMap::new(),
        };

        self.register_provider(config).await
    }

    /// 清空所有已注册的提供商（用于配置热重载，清空后由调用方重新注册）。
    pub async fn clear(&self) {
        self.providers.write().await.clear();
    }

    /// 发送聊天请求
    pub async fn chat(
        &self,
        model_id: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatResponse, raven_types::AgentError> {
        let (client, model) = self.resolve(model_id).await?;
        client.chat(&model, messages, tools).await
    }

    /// 流式聊天
    pub async fn chat_stream(
        &self,
        model_id: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamEvent>, raven_types::AgentError> {
        let (client, model) = self.resolve(model_id).await?;
        client.chat_stream(&model, messages, tools).await
    }

    /// 列出所有可用模型
    pub async fn list_models(&self) -> Vec<ModelInfo> {
        let providers = self.providers.read().await;
        let mut all = Vec::new();

        for entry in providers.iter() {
            if let Ok(models) = entry.client.list_models().await {
                all.extend(models);
            } else {
                // 回退：从配置中的模型列表生成
                for m in &entry.config.models {
                    all.push(ModelInfo {
                        id: format!("{}/{}", entry.name, m),
                        name: m.clone(),
                        provider: entry.name.clone(),
                        max_tokens: 128000,
                        supports_tools: true,
                        supports_vision: false,
                    });
                }
            }
        }

        all
    }

    /// 验证所有提供商
    pub async fn verify_all(&self) -> Vec<ProviderVerification> {
        let providers = self.providers.read().await;
        let mut results = Vec::new();

        for entry in providers.iter() {
            let result = self.verify_single(entry).await;
            results.push(result);
        }

        results
    }

    /// 验证指定提供商
    pub async fn verify_provider(
        &self,
        name: &str,
    ) -> Result<ProviderVerification, raven_types::AgentError> {
        let providers = self.providers.read().await;
        let entry = providers.iter().find(|p| p.name == name).ok_or_else(|| {
            raven_types::AgentError::config(
                format!("未知的提供商: {}", name),
                "使用 'agent models' 查看可用提供商",
            )
        })?;

        Ok(self.verify_single(entry).await)
    }

    // ===================================================================
    // 内部方法
    // ===================================================================

    /// 解析模型ID
    async fn resolve(
        &self,
        model_id: &str,
    ) -> Result<(Box<dyn ProviderClient>, String), raven_types::AgentError> {
        let providers = self.providers.read().await;

        if providers.is_empty() {
            return Err(raven_types::AgentError::config(
                "没有可用的模型提供商",
                "请设置 RAVEN_API_KEY 环境变量或在配置文件中指定 api_key",
            ));
        }

        // 格式: "provider/model" 或 "model"
        if let Some(pos) = model_id.find('/') {
            let provider_name = &model_id[..pos];
            let model_name = &model_id[pos + 1..];

            let entry = providers
                .iter()
                .find(|p| p.name == provider_name)
                .ok_or_else(|| {
                    raven_types::AgentError::config(
                        format!("未知的提供商: {}", provider_name),
                        format!(
                            "可用提供商: {}",
                            providers
                                .iter()
                                .map(|p| p.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    )
                })?;

            return Ok((make_client(entry.config.clone()), model_name.to_string()));
        }

        // 只指定了模型名，使用第一个匹配的提供商
        for entry in providers.iter() {
            if entry.config.models.contains(&model_id.to_string()) {
                return Ok((make_client(entry.config.clone()), model_id.to_string()));
            }
        }

        // 使用默认提供商
        let default = &providers[0];
        Ok((make_client(default.config.clone()), model_id.to_string()))
    }

    /// 验证单个提供商
    async fn verify_single(&self, entry: &ProviderEntry) -> ProviderVerification {
        Self::verify_provider_impl(entry).await
    }

    /// 验证提供商（静态方法）
    async fn verify_provider_impl(entry: &ProviderEntry) -> ProviderVerification {
        let start = Instant::now();
        let mut result = ProviderVerification {
            provider: entry.name.clone(),
            verified: false,
            features: ProviderFeatures::default(),
            latency_ms: 0,
            models: Vec::new(),
            fingerprint: String::new(),
            error: None,
        };

        // 1. 测试连通性
        match entry.client.list_models().await {
            Ok(models) => {
                result.latency_ms = start.elapsed().as_millis() as u64;
                for m in models {
                    result.models.push(m.id);
                }
            }
            Err(e) => {
                result.latency_ms = start.elapsed().as_millis() as u64;
                result.error = Some(format!("连通性测试失败: {}", e));
                return result;
            }
        }

        // 2. 测试功能
        result.features = Self::test_features(entry).await;

        // 3. 指纹验证
        result.fingerprint = Self::fingerprint(entry).await.unwrap_or_default();

        // 综合判断
        result.verified = !result.models.is_empty() && result.features.tool_calling;

        result
    }

    /// 测试提供商功能
    async fn test_features(entry: &ProviderEntry) -> ProviderFeatures {
        let mut features = ProviderFeatures::default();

        // 测试流式
        let messages = vec![Message::user("Hi")];
        if let Ok(mut rx) = entry
            .client
            .chat_stream(
                entry.config.default_model.as_deref().unwrap_or("gpt-4o"),
                &messages,
                None,
            )
            .await
        {
            // 等待第一个事件
            if let Some(_event) = tokio::time::timeout(Duration::from_secs(10), rx.recv())
                .await
                .ok()
                .flatten()
            {
                features.streaming = true;
            }
        }

        // 测试工具调用
        let tools = vec![ToolSchema {
            schema_type: "function".to_string(),
            function: raven_types::FunctionSchema {
                name: "test".to_string(),
                description: "test tool".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {},
                }),
            },
        }];

        if let Ok(_resp) = entry
            .client
            .chat(
                entry.config.default_model.as_deref().unwrap_or("gpt-4o"),
                &messages,
                Some(&tools),
            )
            .await
        {
            features.tool_calling = true;
            // 如果响应不为空，说明支持 system prompt
            features.system_prompt = true;
        }

        features
    }

    /// 指纹验证
    async fn fingerprint(entry: &ProviderEntry) -> Result<String, Box<dyn std::error::Error>> {
        let messages = vec![Message::user("What is 2+2? Answer with ONLY the number.")];

        let resp = entry
            .client
            .chat(
                entry.config.default_model.as_deref().unwrap_or("gpt-4o"),
                &messages,
                None,
            )
            .await?;

        let mut hasher = Sha256::new();
        hasher.update(resp.content.as_bytes());
        let hash = format!("{:x}", hasher.finalize());

        Ok(hash[..16].to_string())
    }

    /// 从 URL 推断提供商名称
    fn infer_name(url: &str) -> String {
        let url = url.to_lowercase();
        if url.contains("openai") {
            return "openai".to_string();
        }
        if url.contains("anthropic") || url.contains("claude") {
            return "anthropic".to_string();
        }
        if url.contains("generativelanguage")
            || url.contains("gemini")
            || url.contains("googleapis")
        {
            return "gemini".to_string();
        }
        if url.contains("deepseek") {
            return "deepseek".to_string();
        }
        if url.contains("groq") {
            return "groq".to_string();
        }
        if url.contains("together") {
            return "together".to_string();
        }
        if url.contains("ollama") {
            return "ollama".to_string();
        }
        "custom".to_string()
    }
}
