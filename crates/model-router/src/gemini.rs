//! Google Gemini 原生客户端
//! generateContent / streamGenerateContent
//! https://ai.google.dev/api/generate-content
//!
//! 与 OpenAI 的差异（在本模块内做转换，对上层透明）：
//! - 鉴权用 URL query `?key=API_KEY`，不是 Authorization header
//! - 端点形如 `{base}/models/{model}:generateContent` / `:streamGenerateContent?alt=sse`
//! - 角色 user/model（assistant→model）；system 走顶层 `system_instruction`
//! - 消息是 `contents:[{role, parts:[{text}|{functionCall}|{functionResponse}]}]`
//! - 工具调用 `functionCall.args` 是完整 JSON 对象（流式也不分块），无需拼接
//! - tool schema 走 `tools:[{function_declarations:[...]}]`

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use raven_types::{
    ChatResponse, Message, ModelInfo, ProviderConfig, Role, StreamEvent, TokenUsage, ToolCall,
    ToolCallFunction, ToolSchema,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tracing::{debug, error, trace};

pub struct GeminiClient {
    config: ProviderConfig,
    http: reqwest::Client,
}

impl GeminiClient {
    pub fn new(config: ProviderConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap(),
        }
    }

    fn base(&self) -> String {
        self.config.base_url.trim_end_matches('/').to_string()
    }

    fn key(&self) -> &str {
        self.config.api_key.as_deref().unwrap_or("")
    }

    /// 把 OpenAI 风格 messages 转成 Gemini 格式
    /// 返回 (system_instruction, contents)
    fn convert_messages(messages: &[Message]) -> (Option<Value>, Vec<Value>) {
        let mut system_parts: Vec<String> = Vec::new();
        let mut contents: Vec<Value> = Vec::new();

        for msg in messages {
            match msg.role {
                Role::System => {
                    if !msg.content.is_empty() {
                        system_parts.push(msg.content.clone());
                    }
                }
                Role::User => {
                    contents.push(json!({
                        "role": "user",
                        "parts": [{ "text": msg.content }],
                    }));
                }
                Role::Assistant => {
                    let mut parts: Vec<Value> = Vec::new();
                    if !msg.content.is_empty() {
                        parts.push(json!({ "text": msg.content }));
                    }
                    if let Some(tcs) = &msg.tool_calls {
                        for tc in tcs {
                            let args: Value = serde_json::from_str(&tc.function.arguments)
                                .unwrap_or_else(|_| json!({}));
                            parts.push(json!({
                                "functionCall": { "name": tc.function.name, "args": args },
                            }));
                        }
                    }
                    if parts.is_empty() {
                        parts.push(json!({ "text": "" }));
                    }
                    contents.push(json!({ "role": "model", "parts": parts }));
                }
                Role::Tool => {
                    // tool 结果 → functionResponse part（role=user）
                    let name = msg.name.clone().unwrap_or_default();
                    // content 尽量解析成对象，否则包成 {result: "..."}
                    let response: Value = serde_json::from_str(&msg.content)
                        .unwrap_or_else(|_| json!({ "result": msg.content }));
                    let part = json!({
                        "functionResponse": { "name": name, "response": response },
                    });
                    if let Some(last) = contents.last_mut() {
                        if last["role"] == "user" {
                            let all_fr = last["parts"]
                                .as_array()
                                .map(|a| a.iter().all(|p| p.get("functionResponse").is_some()))
                                .unwrap_or(false);
                            if all_fr {
                                last["parts"].as_array_mut().unwrap().push(part);
                                continue;
                            }
                        }
                    }
                    contents.push(json!({ "role": "user", "parts": [part] }));
                }
            }
        }

        let system = if system_parts.is_empty() {
            None
        } else {
            Some(json!({ "parts": [{ "text": system_parts.join("\n\n") }], "role": "user" }))
        };
        (system, contents)
    }

    fn convert_tools(tools: &[ToolSchema]) -> Value {
        let decls: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.function.name,
                    "description": t.function.description,
                    "parameters": t.function.parameters,
                })
            })
            .collect();
        json!([{ "function_declarations": decls }])
    }

    fn build_body(&self, messages: &[Message], tools: Option<&[ToolSchema]>) -> Value {
        let (system, contents) = Self::convert_messages(messages);
        let mut body = json!({ "contents": contents });
        if let Some(sys) = system {
            body["system_instruction"] = sys;
        }
        if let Some(ts) = tools {
            if !ts.is_empty() {
                body["tools"] = Self::convert_tools(ts);
            }
        }
        body
    }

    /// 从 candidate parts 提取 text 与 functionCall
    fn extract_parts(parts: &[Part], text_out: &mut String, tcs_out: &mut Vec<ToolCall>) {
        for part in parts {
            if let Some(t) = &part.text {
                text_out.push_str(t);
            }
            if let Some(fc) = &part.function_call {
                let idx = tcs_out.len();
                let call_id = if fc.id.is_empty() {
                    format!("call_{}_{}", fc.name, idx)
                } else {
                    fc.id.clone()
                };
                tcs_out.push(ToolCall {
                    index: idx,
                    id: call_id,
                    call_type: "function".to_string(),
                    function: ToolCallFunction {
                        name: fc.name.clone(),
                        arguments: fc.args.to_string(),
                    },
                });
            }
        }
    }
}

#[async_trait]
impl super::ProviderClient for GeminiClient {
    fn name(&self) -> &str {
        &self.config.name
    }

    async fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatResponse, raven_types::AgentError> {
        let body = self.build_body(messages, tools);
        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base(),
            model,
            self.key()
        );
        debug!("Gemini 请求 model={}", model);

        let resp = self
            .http
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
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

        let api: GenerateResponse = resp.json().await.map_err(|e| {
            raven_types::AgentError::network(format!("解析响应失败: {}", e), "API 返回了非预期格式")
        })?;

        let mut content = String::new();
        let mut tool_calls = Vec::new();
        let mut finish_reason = String::new();
        if let Some(cand) = api.candidates.into_iter().next() {
            finish_reason = cand.finish_reason.unwrap_or_default();
            Self::extract_parts(&cand.content.parts, &mut content, &mut tool_calls);
        }

        let usage = api.usage_metadata.unwrap_or_default();
        Ok(ChatResponse {
            content,
            tool_calls,
            model: model.to_string(),
            finish_reason,
            usage: TokenUsage {
                input: usage.prompt_token_count as usize,
                output: usage.candidates_token_count as usize,
                total: usage.total_token_count as usize,
                cached: usage.cached_content_token_count.map(|t| t as usize),
            },
        })
    }

    async fn chat_stream(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<mpsc::Receiver<StreamEvent>, raven_types::AgentError> {
        let body = self.build_body(messages, tools);
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse&key={}",
            self.base(),
            model,
            self.key()
        );

        let resp = self
            .http
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
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
            let mut tc_index: usize = 0;
            while let Some(event) = stream.next().await {
                match event {
                    Ok(ev) => {
                        if ev.data == "[DONE]" {
                            let _ = tx.send(StreamEvent::done()).await;
                            break;
                        }
                        match serde_json::from_str::<GenerateResponse>(&ev.data) {
                            Ok(chunk) => {
                                if let Some(cand) = chunk.candidates.into_iter().next() {
                                    for part in &cand.content.parts {
                                        if let Some(t) = &part.text {
                                            if !t.is_empty() {
                                                let _ = tx.send(StreamEvent::text(t.clone())).await;
                                            }
                                        }
                                        if let Some(fc) = &part.function_call {
                                            // Gemini 流式中 functionCall 是完整对象，直接发
                                            let call_id = if fc.id.is_empty() {
                                                format!("call_{}_{}", fc.name, tc_index)
                                            } else {
                                                fc.id.clone()
                                            };
                                            let tc = ToolCall {
                                                index: tc_index,
                                                id: call_id,
                                                call_type: "function".to_string(),
                                                function: ToolCallFunction {
                                                    name: fc.name.clone(),
                                                    arguments: fc.args.to_string(),
                                                },
                                            };
                                            tc_index += 1;
                                            if let Ok(j) = serde_json::to_string(&tc) {
                                                let _ = tx.send(StreamEvent::tool_call(j)).await;
                                            }
                                        }
                                    }
                                }
                                if let Some(usage) = chunk.usage_metadata {
                                    let _ = tx
                                        .send(StreamEvent::usage(TokenUsage {
                                            input: usage.prompt_token_count as usize,
                                            output: usage.candidates_token_count as usize,
                                            total: usage.total_token_count as usize,
                                            cached: usage
                                                .cached_content_token_count
                                                .map(|t| t as usize),
                                        }))
                                        .await;
                                }
                            }
                            Err(e) => trace!("跳过无法解析的 Gemini SSE: {}", e),
                        }
                    }
                    Err(e) => {
                        error!("Gemini SSE 流错误: {}", e);
                        let _ = tx.send(StreamEvent::error(e.to_string())).await;
                        break;
                    }
                }
            }
            // Gemini SSE 结束不一定发 [DONE]，流自然结束时补一个 done
            let _ = tx.send(StreamEvent::done()).await;
        });

        Ok(rx)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, raven_types::AgentError> {
        Ok(self
            .config
            .models
            .iter()
            .map(|m| ModelInfo {
                id: format!("{}/{}", self.config.name, m),
                name: m.clone(),
                provider: self.config.name.clone(),
                max_tokens: 1_000_000,
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
struct GenerateResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(default, rename = "usageMetadata")]
    usage_metadata: Option<UsageMetadata>,
}

#[derive(Debug, Deserialize)]
struct Candidate {
    #[serde(default)]
    content: Content,
    #[serde(default, rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct Content {
    #[serde(default)]
    parts: Vec<Part>,
}

#[derive(Debug, Deserialize)]
struct Part {
    #[serde(default)]
    text: Option<String>,
    #[serde(default, rename = "functionCall")]
    function_call: Option<FunctionCall>,
}

#[derive(Debug, Deserialize)]
struct FunctionCall {
    #[serde(default)]
    id: String,
    name: String,
    #[serde(default)]
    args: Value,
}

#[derive(Debug, Deserialize, Default)]
struct UsageMetadata {
    #[serde(default, rename = "promptTokenCount")]
    prompt_token_count: i64,
    #[serde(default, rename = "candidatesTokenCount")]
    candidates_token_count: i64,
    #[serde(default, rename = "totalTokenCount")]
    total_token_count: i64,
    #[serde(default, rename = "cachedContentTokenCount")]
    cached_content_token_count: Option<i64>,
}
