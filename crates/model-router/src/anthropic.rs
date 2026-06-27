//! Anthropic Claude 原生客户端
//! Messages API: https://docs.anthropic.com/en/api/messages
//!
//! 与 OpenAI 的差异（在本模块内做转换，对上层透明）：
//! - system 不在 messages 数组里，而是顶层 `system` 字段
//! - 角色只有 user/assistant；tool 结果是 user 消息里的 tool_result block
//! - assistant 的工具调用是 tool_use content block（input 为 JSON 对象）
//! - tool schema 用 `input_schema` 而非 `function.parameters`
//! - 流式：content_block_start/delta(input_json_delta)/stop + message_delta(usage)

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use raven_types::{
    ApiConfig, ChatResponse, Message, ModelConfig, ModelInfo, ProviderConfig, Role, StreamEvent,
    TokenUsage, ToolCall, ToolCallFunction, ToolSchema,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tracing::{debug, error, trace};

const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 8192;

pub struct AnthropicClient {
    config: ProviderConfig,
    params: ModelConfig,
    max_retries: u32,
    http: reqwest::Client,
}

impl AnthropicClient {
    pub fn new(config: ProviderConfig, params: ModelConfig, api: ApiConfig) -> Self {
        Self {
            config,
            params,
            max_retries: api.max_retries,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(api.timeout))
                .build()
                .unwrap(),
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/messages", self.config.base_url.trim_end_matches('/'))
    }

    /// 把 OpenAI 风格的 messages 转成 Anthropic 格式
    /// 返回 (system_prompt, anthropic_messages)
    fn convert_messages(messages: &[Message]) -> (Option<String>, Vec<Value>) {
        let mut system_parts: Vec<String> = Vec::new();
        let mut out: Vec<Value> = Vec::new();

        for msg in messages {
            match msg.role {
                Role::System => {
                    if !msg.content.is_empty() {
                        system_parts.push(msg.content.clone());
                    }
                }
                Role::User => {
                    out.push(json!({
                        "role": "user",
                        "content": [{ "type": "text", "text": msg.content }],
                    }));
                }
                Role::Assistant => {
                    let mut blocks: Vec<Value> = Vec::new();
                    if !msg.content.is_empty() {
                        blocks.push(json!({ "type": "text", "text": msg.content }));
                    }
                    if let Some(tcs) = &msg.tool_calls {
                        for tc in tcs {
                            // arguments 是 JSON 字符串，转成对象
                            let input: Value = serde_json::from_str(&tc.function.arguments)
                                .unwrap_or_else(|_| json!({}));
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": tc.function.name,
                                "input": input,
                            }));
                        }
                    }
                    if blocks.is_empty() {
                        blocks.push(json!({ "type": "text", "text": "" }));
                    }
                    out.push(json!({ "role": "assistant", "content": blocks }));
                }
                Role::Tool => {
                    // tool 结果 → user 消息里的 tool_result block
                    let block = json!({
                        "type": "tool_result",
                        "tool_use_id": msg.tool_call_id.clone().unwrap_or_default(),
                        "content": msg.content,
                    });
                    // 若上一条已是 tool_result 的 user 消息，合并（Anthropic 要求连续 tool_result 合并）
                    if let Some(last) = out.last_mut() {
                        if last["role"] == "user" {
                            let is_tool_result = last["content"]
                                .as_array()
                                .map(|arr| arr.iter().all(|b| b["type"] == "tool_result"))
                                .unwrap_or(false);
                            if is_tool_result {
                                last["content"].as_array_mut().unwrap().push(block);
                                continue;
                            }
                        }
                    }
                    out.push(json!({ "role": "user", "content": [block] }));
                }
            }
        }

        let system = if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n\n"))
        };
        (system, out)
    }

    /// OpenAI tool schema → Anthropic tool schema
    fn convert_tools(tools: &[ToolSchema]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.function.name,
                    "description": t.function.description,
                    "input_schema": t.function.parameters,
                })
            })
            .collect()
    }

    fn build_body(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
        stream: bool,
    ) -> Value {
        let (system, msgs) = Self::convert_messages(messages);
        let mut body = json!({
            "model": model,
            "max_tokens": self.params.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            "messages": msgs,
            "stream": stream,
        });
        if let Some(t) = self.params.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(p) = self.params.top_p {
            body["top_p"] = json!(p);
        }
        if let Some(sys) = system {
            body["system"] = json!(sys);
        }
        if let Some(ts) = tools {
            if !ts.is_empty() {
                body["tools"] = json!(Self::convert_tools(ts));
            }
        }
        body
    }
}

#[async_trait]
impl super::ProviderClient for AnthropicClient {
    fn name(&self) -> &str {
        &self.config.name
    }

    async fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatResponse, raven_types::AgentError> {
        let body = self.build_body(model, messages, tools, false);
        debug!("Anthropic 请求 {}, model={}", self.endpoint(), model);

        let resp = self
            .http
            .post(self.endpoint())
            .header("x-api-key", self.config.api_key.as_deref().unwrap_or(""))
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body);
        let resp = super::send_with_retry(resp, self.max_retries)
            .await
            .map_err(|e| {
                raven_types::AgentError::network(
                    format!("请求失败: {}", e),
                    "检查网络连接和 API Key",
                )
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(raven_types::AgentError::network(
                format!("API 返回错误 {}: {}", status, text),
                "检查 API Key 是否有效，或模型是否可用",
            ));
        }

        let api: MessagesResponse = resp.json().await.map_err(|e| {
            raven_types::AgentError::network(format!("解析响应失败: {}", e), "API 返回了非预期格式")
        })?;

        let mut content = String::new();
        let mut tool_calls = Vec::new();
        for (i, block) in api.content.into_iter().enumerate() {
            match block {
                ContentBlock::Text { text } => content.push_str(&text),
                ContentBlock::ToolUse { id, name, input } => {
                    tool_calls.push(ToolCall {
                        index: i,
                        id,
                        call_type: "function".to_string(),
                        function: ToolCallFunction {
                            name,
                            arguments: input.to_string(),
                        },
                    });
                }
            }
        }

        Ok(ChatResponse {
            content,
            tool_calls,
            model: api.model,
            finish_reason: api.stop_reason.unwrap_or_default(),
            usage: TokenUsage {
                input: api.usage.input_tokens as usize,
                output: api.usage.output_tokens as usize,
                total: (api.usage.input_tokens + api.usage.output_tokens) as usize,
                cached: api.usage.cache_read_input_tokens.map(|t| t as usize),
            },
        })
    }

    async fn chat_stream(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<mpsc::Receiver<StreamEvent>, raven_types::AgentError> {
        let body = self.build_body(model, messages, tools, true);

        let resp = self
            .http
            .post(self.endpoint())
            .header("x-api-key", self.config.api_key.as_deref().unwrap_or(""))
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body);
        let resp = super::send_with_retry(resp, self.max_retries)
            .await
            .map_err(|e| {
                raven_types::AgentError::network(format!("请求失败: {}", e), "检查网络连接")
            })?;

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
            // content block index → (id, name, accumulated_json)，仅 tool_use 块
            let mut tc_accum: std::collections::HashMap<usize, (String, String, String)> =
                std::collections::HashMap::new();
            let mut input_tokens: usize = 0;

            while let Some(event) = stream.next().await {
                match event {
                    Ok(ev) => match serde_json::from_str::<StreamEventRaw>(&ev.data) {
                        Ok(raw) => match raw {
                            StreamEventRaw::MessageStart { message } => {
                                input_tokens = message.usage.input_tokens as usize;
                            }
                            StreamEventRaw::ContentBlockStart {
                                index,
                                content_block,
                            } => {
                                if let ContentBlock::ToolUse { id, name, .. } = content_block {
                                    tc_accum.insert(index, (id, name, String::new()));
                                }
                            }
                            StreamEventRaw::ContentBlockDelta { index, delta } => match delta {
                                BlockDelta::TextDelta { text } => {
                                    if !text.is_empty() {
                                        let _ = tx.send(StreamEvent::text(text)).await;
                                    }
                                }
                                BlockDelta::InputJsonDelta { partial_json } => {
                                    if let Some(entry) = tc_accum.get_mut(&index) {
                                        entry.2.push_str(&partial_json);
                                    }
                                }
                                BlockDelta::Other => {}
                            },
                            StreamEventRaw::ContentBlockStop { index } => {
                                if let Some((id, name, mut args)) = tc_accum.remove(&index) {
                                    if args.trim().is_empty() {
                                        args = "{}".to_string();
                                    }
                                    let tc = ToolCall {
                                        index,
                                        id,
                                        call_type: "function".to_string(),
                                        function: ToolCallFunction {
                                            name,
                                            arguments: args,
                                        },
                                    };
                                    if let Ok(j) = serde_json::to_string(&tc) {
                                        let _ = tx.send(StreamEvent::tool_call(j)).await;
                                    }
                                }
                            }
                            StreamEventRaw::MessageDelta { usage, .. } => {
                                let _ = tx
                                    .send(StreamEvent::usage(TokenUsage {
                                        input: input_tokens,
                                        output: usage.output_tokens as usize,
                                        total: input_tokens + usage.output_tokens as usize,
                                        cached: None,
                                    }))
                                    .await;
                            }
                            StreamEventRaw::MessageStop => {
                                let _ = tx.send(StreamEvent::done()).await;
                                break;
                            }
                            StreamEventRaw::Other => {}
                        },
                        Err(e) => trace!("跳过无法解析的 Anthropic SSE: {}", e),
                    },
                    Err(e) => {
                        error!("Anthropic SSE 流错误: {}", e);
                        let _ = tx.send(StreamEvent::error(e.to_string())).await;
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, raven_types::AgentError> {
        // 用配置里的模型列表回退（避免额外鉴权调用）
        Ok(self
            .config
            .models
            .iter()
            .map(|m| ModelInfo {
                id: format!("{}/{}", self.config.name, m),
                name: m.clone(),
                provider: self.config.name.clone(),
                max_tokens: 200_000,
                supports_tools: true,
                supports_vision: true,
            })
            .collect())
    }
}

// =============================================================================
// API 类型
// =============================================================================

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    model: String,
    content: Vec<ContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    usage: UsageRaw,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
}

#[derive(Debug, Deserialize, Default)]
struct UsageRaw {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
    #[serde(default)]
    cache_read_input_tokens: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StreamEventRaw {
    MessageStart {
        message: MessageStartInner,
    },
    ContentBlockStart {
        index: usize,
        content_block: ContentBlock,
    },
    ContentBlockDelta {
        index: usize,
        delta: BlockDelta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        #[allow(dead_code)]
        delta: Value,
        usage: UsageRaw,
    },
    MessageStop,
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct MessageStartInner {
    usage: UsageRaw,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BlockDelta {
    TextDelta {
        text: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    #[serde(other)]
    Other,
}
