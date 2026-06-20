//! OpenAI 兼容客户端
//! 支持所有 OpenAI API 兼容的提供商

use raven_types::{ChatResponse, Message, ModelInfo, ProviderConfig, StreamEvent, TokenUsage, ToolCall, ToolCallFunction, ToolSchema};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, error, trace};

/// OpenAI 兼容 API 客户端
pub struct OpenAICompatibleClient {
    config: ProviderConfig,
    http: reqwest::Client,
}

impl OpenAICompatibleClient {
    pub fn new(config: ProviderConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap(),
        }
    }
}

#[async_trait]
impl super::ProviderClient for OpenAICompatibleClient {
    fn name(&self) -> &str {
        &self.config.name
    }

    async fn chat(&self, model: &str, messages: &[Message], tools: Option<&[ToolSchema]>) -> Result<ChatResponse, raven_types::AgentError> {
        let body = ChatRequestBody {
            model: model.to_string(),
            messages: messages.to_vec(),
            stream: false,
            tools: tools.map(|t| t.to_vec()),
            temperature: Some(0.7),
        };

        debug!("发送请求到 {}, model={}", self.config.base_url, model);

        let resp = self.http
            .post(format!("{}/chat/completions", self.config.base_url))
            .header("Authorization", format!("Bearer {}", self.config.api_key.as_deref().unwrap_or("")))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| raven_types::AgentError::network(
                format!("请求失败: {}", e),
                "检查网络连接和 API Key 是否正确",
            ))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(raven_types::AgentError::network(
                format!("API 返回错误 {}: {}", status, text),
                "检查 API Key 是否有效，或模型是否可用",
            ));
        }

        let api_resp: ChatCompletionResponse = resp.json().await.map_err(|e| raven_types::AgentError::network(
            format!("解析响应失败: {}", e),
            "API 返回了非预期的格式",
        ))?;

        if api_resp.choices.is_empty() {
            return Err(raven_types::AgentError::model(
                "API 返回空 choices",
                "尝试切换模型或重试",
            ));
        }

        let choice = &api_resp.choices[0];

        Ok(ChatResponse {
            content: choice.message.content.clone().unwrap_or_default(),
            tool_calls: choice.message.tool_calls.clone().unwrap_or_default(),
            model: api_resp.model,
            finish_reason: choice.finish_reason.clone().unwrap_or_default(),
            usage: api_resp.usage.map_or_else(TokenUsage::default, |u| TokenUsage {
                input: u.prompt_tokens as usize,
                output: u.completion_tokens as usize,
                total: u.total_tokens as usize,
                cached: u.prompt_cache_hit_tokens.map(|t| t as usize),
            }),
        })
    }

    async fn chat_stream(&self, model: &str, messages: &[Message], tools: Option<&[ToolSchema]>) -> Result<mpsc::Receiver<StreamEvent>, raven_types::AgentError> {
        let body = ChatRequestBody {
            model: model.to_string(),
            messages: messages.to_vec(),
            stream: true,
            tools: tools.map(|t| t.to_vec()),
            temperature: Some(0.7),
        };

        let resp = self.http
            .post(format!("{}/chat/completions", self.config.base_url))
            .header("Authorization", format!("Bearer {}", self.config.api_key.as_deref().unwrap_or("")))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| raven_types::AgentError::network(
                format!("请求失败: {}", e),
                "检查网络连接",
            ))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(raven_types::AgentError::network(
                format!("API 返回错误 {}: {}", status, text),
                "检查 API Key 是否有效",
            ));
        }

        let (tx, rx) = mpsc::channel(32);
        let mut stream = resp.bytes_stream().eventsource();

        tokio::spawn(async move {
            // 积累流式 tool call chunks（OpenAI streaming API 分块发送参数）
            let mut tc_accum: std::collections::HashMap<usize, (String, String, String)> =
                std::collections::HashMap::new(); // index → (id, name, arguments)

            while let Some(event) = stream.next().await {
                match event {
                    Ok(ev) => {
                        if ev.data == "[DONE]" {
                            let _ = tx.send(StreamEvent::done()).await;
                            break;
                        }

                        match serde_json::from_str::<StreamChunk>(&ev.data) {
                            Ok(chunk) => {
                                if let Some(choice) = chunk.choices.first() {
                                    // 文本增量
                                    if let Some(content) = &choice.delta.content {
                                        if !content.is_empty() {
                                            let _ = tx.send(StreamEvent::text(content.clone())).await;
                                        }
                                    }

                                    // 工具调用（积累 chunks，不立即发送）
                                    if let Some(tcs) = &choice.delta.tool_calls {
                                        for tc in tcs {
                                            let idx = tc.index;
                                            let entry = tc_accum.entry(idx).or_insert_with(|| {
                                                (tc.id.clone(), tc.function.name.clone(), String::new())
                                            });
                                            // 更新 id 和 name（首个 chunk 带这些字段）
                                            if !tc.id.is_empty() { entry.0 = tc.id.clone(); }
                                            if !tc.function.name.is_empty() { entry.1 = tc.function.name.clone(); }
                                            // arguments 是增量，追加
                                            entry.2.push_str(&tc.function.arguments);
                                        }
                                    }

                                    // Usage
                                    if let Some(usage) = &chunk.usage {
                                        let _ = tx.send(StreamEvent::usage(TokenUsage {
                                            input: usage.prompt_tokens as usize,
                                            output: usage.completion_tokens as usize,
                                            total: usage.total_tokens as usize,
                                            cached: usage.prompt_cache_hit_tokens.map(|t| t as usize),
                                        })).await;
                                    }

                                    // 结束
                                    if choice.finish_reason.is_some() {
                                        // 发送积累的完整 tool calls
                                        let mut indices: Vec<usize> = tc_accum.keys().copied().collect();
                                        indices.sort();
                                        for idx in indices {
                                            if let Some((id, name, arguments)) = tc_accum.remove(&idx) {
                                                let tc = ToolCall {
                                                    index: idx,
                                                    id,
                                                    call_type: "function".to_string(),
                                                    function: ToolCallFunction {
                                                        name,
                                                        arguments,
                                                    },
                                                };
                                                if let Ok(json) = serde_json::to_string(&tc) {
                                                    let _ = tx.send(StreamEvent::tool_call(json)).await;
                                                }
                                            }
                                        }
                                        let _ = tx.send(StreamEvent::done()).await;
                                        break;
                                    }
                                }
                            }
                            Err(e) => {
                                trace!("跳过无法解析的 SSE 行: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        error!("SSE 流错误: {}", e);
                        let _ = tx.send(StreamEvent::error(e.to_string())).await;
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, raven_types::AgentError> {
        let resp = self.http
            .get(format!("{}/models", self.config.base_url))
            .header("Authorization", format!("Bearer {}", self.config.api_key.as_deref().unwrap_or("")))
            .send()
            .await
            .map_err(|e| raven_types::AgentError::network(
                format!("请求失败: {}", e),
                "检查网络连接",
            ))?;

        if !resp.status().is_success() {
            // 某些提供商不支持 /models 端点
            return Ok(Vec::new());
        }

        let api_resp: ModelsResponse = resp.json().await.map_err(|e| raven_types::AgentError::network(
            format!("解析失败: {}", e),
            "",
        ))?;

        let mut models = Vec::new();
        for m in api_resp.data {
            if m.object == "model" || m.object.is_empty() {
                models.push(ModelInfo {
                    id: format!("{}/{}", self.config.name, m.id),
                    name: m.id.clone(),
                    provider: self.config.name.clone(),
                    max_tokens: 128000,
                    supports_tools: true,
                    supports_vision: false,
                });
            }
        }

        Ok(models)
    }
}

// =============================================================================
// API 类型
// =============================================================================

#[derive(Debug, Serialize)]
struct ChatRequestBody {
    model: String,
    messages: Vec<Message>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolSchema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
    model: String,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Deserialize)]
struct Usage {
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    #[serde(default)]
    prompt_cache_hit_tokens: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelData>,
}

#[derive(Debug, Deserialize)]
struct ModelData {
    id: String,
    #[serde(default)]
    object: String,
}
