//! Agent 对话主循环：同步 `run`、流式 `run_stream`、工具调度与执行、崩溃恢复 checkpoint。

use crate::Agent;
use context_engine::cache::ResponseCache;
use raven_types::*;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

impl Agent {
    /// 写入崩溃恢复 checkpoint（每轮对话后调用，失败仅记日志不阻断）
    async fn write_checkpoint(&self) {
        let Some(cp) = &self.checkpoint else { return };
        let messages = self.context.messages().await;
        let stats = self.context.stats().await;
        let session_id = self
            .context
            .current_session_id()
            .await
            .unwrap_or_else(|| "default".to_string());
        let mut mgr = cp.lock().await;
        if let Err(e) = mgr.write(
            &session_id,
            &messages,
            None,
            stats.total_input_tokens,
            stats.total_output_tokens,
        ) {
            warn!("写入 checkpoint 失败: {}", e);
        }
    }

    /// 清除崩溃恢复 checkpoint（会话正常结束后调用）
    async fn clear_checkpoint(&self) {
        let Some(cp) = &self.checkpoint else { return };
        let mgr = cp.lock().await;
        if let Err(e) = mgr.clear() {
            debug!("清除 checkpoint 失败: {}", e);
        }
    }

    /// 收集全部可用工具 schema：内置工具 + 已连接的 MCP 工具。
    /// readonly 模式由调用方判断是否调用本方法。
    pub(crate) async fn collect_tool_schemas(&self) -> Vec<ToolSchema> {
        let mut schemas = self.tools.list_schemas().await;
        if let Some(mcp) = &self.mcp {
            let mgr = mcp.lock().await;
            schemas.extend(mgr.all_tool_schemas());
        }
        // task 工具：派生并行子 agent。仅在主 agent 暴露（子 agent 用 list_schemas 拿不到它，禁递归）。
        schemas.push(Self::task_tool_schema());
        // ask_user 工具：模型主动向用户提问。
        schemas.push(Self::ask_user_tool_schema());
        schemas
    }

    /// 执行单个工具调用，自动区分 MCP 工具（`server__tool`）与内置工具。
    pub(crate) async fn dispatch_tool(&self, call: &ToolCall) -> ToolResult {
        // MCP 工具名形如 `server__tool`，含 "__" 且非内置工具名
        if let Some(mcp) = &self.mcp {
            if call.function.name.contains("__") {
                let args: serde_json::Value =
                    serde_json::from_str(&call.function.arguments).unwrap_or_default();
                let mut mgr = mcp.lock().await;
                return match mgr.execute(&call.function.name, args).await {
                    Ok(content) => ToolResult {
                        tool_call_id: call.id.clone(),
                        name: call.function.name.clone(),
                        content,
                        is_error: false,
                    },
                    Err(e) => ToolResult {
                        tool_call_id: call.id.clone(),
                        name: call.function.name.clone(),
                        content: format!("MCP 工具执行失败: {}", e),
                        is_error: true,
                    },
                };
            }
        }
        self.tools.execute(call).await
    }

    /// 是否存在可恢复的未完成会话
    pub async fn has_recoverable(&self) -> bool {
        match &self.checkpoint {
            Some(cp) => cp.lock().await.recover().is_some(),
            None => false,
        }
    }

    /// 从 checkpoint 恢复上次未完成的会话。
    /// 成功返回恢复的消息条数；无 checkpoint 时返回 0。
    pub async fn recover_checkpoint(&self) -> usize {
        let Some(cp) = &self.checkpoint else { return 0 };
        let recovered = { cp.lock().await.recover() };
        match recovered {
            Some(checkpoint) => {
                let n = checkpoint.messages.len();
                self.context.restore_messages(checkpoint.messages).await;
                info!("已从 checkpoint #{} 恢复 {} 条消息", checkpoint.seq, n);
                n
            }
            None => 0,
        }
    }

    /// 运行一次对话（同步）
    pub async fn run(&self, user_input: impl Into<String>) -> Result<String, AgentError> {
        let input = user_input.into();

        // 检查是否有可用模型
        let models = self.router.list_models().await;
        if models.is_empty() && self.config.read().unwrap().api_key.is_none() {
            return Err(AgentError::config(
                "没有可用的模型提供商",
                "请设置 RAVEN_API_KEY 环境变量或在配置文件中指定 api_key",
            ));
        }

        // 添加用户消息
        self.context.add_user_message(input).await;

        // 基于「将要发送的完整消息列表」计算缓存键。
        // 仅用最新输入会导致不同对话因末轮 user 文本相同而误命中，
        // 故序列化全部消息（含历史）参与哈希。
        let messages_for_key = self.context.messages().await;
        let cache_key = ResponseCache::make_key(
            &self.config.read().unwrap().model,
            &serde_json::to_string(&messages_for_key).unwrap_or_default(),
        );
        if let Some(cached) = self.cache.get(&cache_key).await {
            debug!("缓存命中，跳过 API 调用");
            self.context
                .add_assistant_message(&cached.content, Vec::new())
                .await;
            self.context.record_usage_async(&cached.usage).await;
            return Ok(cached.content);
        }

        loop {
            // 检查预算
            self.context.check_budget()?;

            // 压缩上下文
            if self.context.should_compact().await {
                self.context.compact().await.map_err(AgentError::Internal)?;
            }

            // 获取工具 schema（内置 + MCP，序列化为 JSON 值发送给 LLM）
            let tool_schemas = if self.permission.is_readonly() {
                Vec::new()
            } else {
                self.collect_tool_schemas().await
            };

            // 调用 LLM
            let model = self.config.read().unwrap().model.clone();
            let messages = self.context.messages().await;
            let resp = if tool_schemas.is_empty() {
                self.router.chat(&model, &messages, None).await?
            } else {
                self.router
                    .chat(&model, &messages, Some(&tool_schemas))
                    .await?
            };

            // 记录使用
            self.context.record_usage_async(&resp.usage).await;

            // 处理工具调用
            if !resp.tool_calls.is_empty() {
                // 添加助手消息
                self.context
                    .add_assistant_message(&resp.content, resp.tool_calls.clone())
                    .await;

                // 执行工具
                let results = self.execute_tools(&resp.tool_calls).await;

                // 添加结果
                self.context.add_tool_results(results).await;

                // 工具执行后写 checkpoint（崩溃风险点，便于恢复）
                self.write_checkpoint().await;

                continue; // 继续循环
            }

            // 没有工具调用，返回结果
            self.context
                .add_assistant_message(&resp.content, Vec::new())
                .await;
            // 存入缓存
            self.cache.put(cache_key, &resp).await;
            // 会话正常完成，清除 checkpoint
            self.clear_checkpoint().await;
            return Ok(resp.content);
        }
    }

    /// 流式运行对话
    pub async fn run_stream(
        &self,
        user_input: impl Into<String>,
    ) -> Result<mpsc::Receiver<StreamEvent>, AgentError> {
        let input = user_input.into();
        let models = self.router.list_models().await;

        if models.is_empty() && self.config.read().unwrap().api_key.is_none() {
            return Err(AgentError::config(
                "没有可用的模型提供商",
                "请设置 RAVEN_API_KEY 环境变量",
            ));
        }

        // 添加用户消息
        self.context.add_user_message(input).await;

        let (tx, rx) = mpsc::channel(32);
        let router = self.router.clone();
        let tools = self.tools.clone();
        let context = self.context.clone();
        let permission = self.permission.clone();
        let confirmer = self.confirmer.clone();
        let model = self.config.read().unwrap().model.clone();
        let mcp = self.mcp.clone();
        let config = self.config.clone();
        let stream_enabled = self.config.read().unwrap().api.stream;

        tokio::spawn(async move {
            loop {
                // 下游接收端已关闭（SSE 客户端断开）时尽早退出，
                // 避免继续空跑 LLM 调用与工具执行、白白消耗 API。
                if tx.is_closed() {
                    debug!("流式接收端已关闭，停止生成");
                    return;
                }

                // 检查预算
                if let Err(e) = context.check_budget() {
                    let _ = tx.send(StreamEvent::error(e.to_string())).await;
                    return;
                }

                // 压缩
                if context.should_compact().await {
                    let _ = context.compact().await;
                }

                // 获取工具（内置 + MCP）
                let tool_schemas = if permission.is_readonly() {
                    Vec::new()
                } else {
                    let mut s = tools.list_schemas().await;
                    if let Some(m) = &mcp {
                        s.extend(m.lock().await.all_tool_schemas());
                    }
                    s.push(Self::task_tool_schema());
                    s.push(Self::ask_user_tool_schema());
                    s
                };

                // 调用 LLM：按配置走流式或非流式。
                let messages = context.messages().await;
                let tools_opt = if tool_schemas.is_empty() {
                    None
                } else {
                    Some(tool_schemas.as_slice())
                };

                let mut assistant_text = String::new();
                let mut tool_calls: Vec<ToolCall> = Vec::new();

                if stream_enabled {
                    let stream_result = router.chat_stream(&model, &messages, tools_opt).await;
                    let mut stream = match stream_result {
                        Ok(s) => s,
                        Err(e) => {
                            let _ = tx.send(StreamEvent::error(e.to_string())).await;
                            return;
                        }
                    };

                    // 转发流事件
                    let mut errored = false;
                    while let Some(event) = stream.recv().await {
                        match event.event_type.as_str() {
                            "text" => {
                                if let Some(ref text) = event.content {
                                    assistant_text.push_str(text);
                                }
                                let _ = tx.send(event).await;
                            }
                            "tool_call" => {
                                if let Some(ref tc_json) = event.content {
                                    if let Ok(tc) = serde_json::from_str::<ToolCall>(tc_json) {
                                        tool_calls.push(tc);
                                    }
                                }
                                let _ = tx.send(event).await;
                            }
                            "usage" => {
                                if let Some(ref usage) = event.usage {
                                    context.record_usage_async(usage).await;
                                }
                            }
                            "done" => break,
                            "error" => {
                                let _ = tx.send(event).await;
                                errored = true;
                                break;
                            }
                            _ => {}
                        }
                    }
                    if errored {
                        return;
                    }
                } else {
                    // 非流式：一次拿到完整响应，再把文本/工具调用按流式事件格式补发，
                    // 使下游（SSE/TUI）渲染逻辑无需区分两种模式。
                    match router.chat(&model, &messages, tools_opt).await {
                        Ok(resp) => {
                            assistant_text = resp.content.clone();
                            tool_calls = resp.tool_calls.clone();
                            if !resp.content.is_empty() {
                                let _ = tx.send(StreamEvent::text(resp.content)).await;
                            }
                            for tc in &resp.tool_calls {
                                if let Ok(json) = serde_json::to_string(tc) {
                                    let _ = tx.send(StreamEvent::tool_call(json)).await;
                                }
                            }
                            context.record_usage_async(&resp.usage).await;
                        }
                        Err(e) => {
                            let _ = tx.send(StreamEvent::error(e.to_string())).await;
                            return;
                        }
                    }
                }

                // 没有工具调用，结束
                if tool_calls.is_empty() {
                    context
                        .add_assistant_message(&assistant_text, Vec::new())
                        .await;
                    let _ = tx.send(StreamEvent::done()).await;
                    return;
                }

                // 有工具调用
                context
                    .add_assistant_message(&assistant_text, tool_calls.clone())
                    .await;

                // 执行工具
                // task 工具：先并发跑完所有子 agent（限流），结果按 id 索引；
                // 其余工具仍逐个权限门控 + 执行。最后按原始顺序发 tool_result 事件。
                let mut task_results: std::collections::HashMap<String, ToolResult> =
                    std::collections::HashMap::new();
                let task_calls: Vec<&ToolCall> = tool_calls
                    .iter()
                    .filter(|c| c.function.name == "task")
                    .collect();
                if !task_calls.is_empty() {
                    let prompts: Vec<(String, String)> = task_calls
                        .iter()
                        .map(|c| {
                            let args: serde_json::Value =
                                serde_json::from_str(&c.function.arguments).unwrap_or_default();
                            let desc = args
                                .get("description")
                                .and_then(|v| v.as_str())
                                .unwrap_or("子任务")
                                .to_string();
                            let prompt = args
                                .get("prompt")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            (desc, prompt)
                        })
                        .collect();
                    let outputs = Self::run_parallel_tasks(
                        config.clone(),
                        router.clone(),
                        tools.clone(),
                        prompts,
                    )
                    .await;
                    for (call, (_desc, text, is_error)) in task_calls.iter().zip(outputs) {
                        task_results.insert(
                            call.id.clone(),
                            ToolResult {
                                tool_call_id: call.id.clone(),
                                name: "task".to_string(),
                                content: text,
                                is_error,
                            },
                        );
                    }
                }

                let mut results = Vec::new();
                // 本轮「允许本轮全部」标志（同步路径一致）
                let round_allow = std::sync::atomic::AtomicBool::new(false);
                for tc in &tool_calls {
                    // task：取并发结果，直接发事件
                    if tc.function.name == "task" {
                        if let Some(result) = task_results.remove(&tc.id) {
                            results.push(result.clone());
                            if let Ok(json) = serde_json::to_string(&result) {
                                let _ = tx.send(StreamEvent::tool_result(json)).await;
                            }
                        }
                        continue;
                    }
                    // ask_user：直接调 confirmer 提问，不走权限门控
                    if tc.function.name == "ask_user" {
                        let result = Self::run_ask_user(confirmer.as_ref(), tc).await;
                        results.push(result.clone());
                        if let Ok(json) = serde_json::to_string(&result) {
                            let _ = tx.send(StreamEvent::tool_result(json)).await;
                        }
                        continue;
                    }
                    // 权限门控 + 交互式确认（与同步路径共用同一逻辑）
                    let result = match Self::gate_and_confirm(
                        &permission,
                        confirmer.as_ref(),
                        tc,
                        &round_allow,
                    )
                    .await
                    {
                        Ok(()) => {
                            // MCP 工具（server__tool）路由到 McpManager，其余走内置注册表
                            if let Some(m) =
                                mcp.as_ref().filter(|_| tc.function.name.contains("__"))
                            {
                                let args: serde_json::Value =
                                    serde_json::from_str(&tc.function.arguments)
                                        .unwrap_or_default();
                                match m.lock().await.execute(&tc.function.name, args).await {
                                    Ok(content) => ToolResult {
                                        tool_call_id: tc.id.clone(),
                                        name: tc.function.name.clone(),
                                        content,
                                        is_error: false,
                                    },
                                    Err(e) => ToolResult {
                                        tool_call_id: tc.id.clone(),
                                        name: tc.function.name.clone(),
                                        content: format!("MCP 工具执行失败: {}", e),
                                        is_error: true,
                                    },
                                }
                            } else {
                                tools.execute(tc).await
                            }
                        }
                        Err(reason) => ToolResult {
                            tool_call_id: tc.id.clone(),
                            name: tc.function.name.clone(),
                            content: reason,
                            is_error: true,
                        },
                    };
                    results.push(result.clone());

                    if let Ok(json) = serde_json::to_string(&result) {
                        let _ = tx.send(StreamEvent::tool_result(json)).await;
                    }
                }

                context.add_tool_results(results).await;
            }
        });

        Ok(rx)
    }

    async fn execute_tools(&self, calls: &[ToolCall]) -> Vec<ToolResult> {
        // task 工具特殊路由：一批里的多个 task 并发跑（run_parallel_tasks 限流），
        // 非 task 工具保持原有顺序逐个执行。最后按原始调用顺序重组结果。
        let task_calls: Vec<&ToolCall> =
            calls.iter().filter(|c| c.function.name == "task").collect();

        // 并发执行所有 task 子 agent，结果按 call.id 建索引
        let mut task_results: std::collections::HashMap<String, ToolResult> =
            std::collections::HashMap::new();
        if !task_calls.is_empty() {
            let prompts: Vec<(String, String)> = task_calls
                .iter()
                .map(|c| {
                    let args: serde_json::Value =
                        serde_json::from_str(&c.function.arguments).unwrap_or_default();
                    let desc = args
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("子任务")
                        .to_string();
                    let prompt = args
                        .get("prompt")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    (desc, prompt)
                })
                .collect();
            let outputs = Self::run_parallel_tasks(
                self.config.clone(),
                self.router.clone(),
                self.tools.clone(),
                prompts,
            )
            .await;
            for (call, (_desc, text, is_error)) in task_calls.iter().zip(outputs) {
                task_results.insert(
                    call.id.clone(),
                    ToolResult {
                        tool_call_id: call.id.clone(),
                        name: "task".to_string(),
                        content: text,
                        is_error,
                    },
                );
            }
        }

        let mut results = Vec::new();

        // 本轮「允许本轮全部」标志：用户在某次确认里选「允许本轮全部」后，
        // 本轮剩余待确认工具一律放行（一次性批准多个操作）。
        let round_allow = std::sync::atomic::AtomicBool::new(false);

        for call in calls {
            // task：取并发结果
            if call.function.name == "task" {
                if let Some(r) = task_results.remove(&call.id) {
                    results.push(r);
                }
                continue;
            }

            // ask_user：直接调 confirmer 提问，不走权限门控
            if call.function.name == "ask_user" {
                results.push(Self::run_ask_user(self.confirmer.as_ref(), call).await);
                continue;
            }

            // 权限门控 + 交互式确认
            if let Err(reason) = Self::gate_and_confirm(
                &self.permission,
                self.confirmer.as_ref(),
                call,
                &round_allow,
            )
            .await
            {
                results.push(ToolResult {
                    tool_call_id: call.id.clone(),
                    name: call.function.name.clone(),
                    content: reason,
                    is_error: true,
                });
                continue;
            }

            // Git-first: 编辑前保存状态
            let is_file_edit =
                call.function.name == "file_edit" || call.function.name == "file_write";
            let args_json: serde_json::Value =
                serde_json::from_str(&call.function.arguments).unwrap_or_default();
            let target_path = args_json.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let _before_hash = if is_file_edit {
                self.git_first.pre_edit(target_path).unwrap_or(None)
            } else {
                None
            };

            let result = self.dispatch_tool(call).await;

            // Git-first: 编辑后自动 commit
            if is_file_edit && !result.is_error {
                // 按字符截断，避免在多字节 UTF-8 字符（如中文）中间切断 panic
                let desc = if result.content.chars().count() > 30 {
                    let head: String = result.content.chars().take(30).collect();
                    format!("{}...", head)
                } else {
                    result.content.clone()
                };
                let _ = self
                    .git_first
                    .post_edit(target_path, &call.function.name, &desc);
            }

            results.push(result);
        }

        results
    }
}
