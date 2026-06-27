//! # agent-core
//! Agent 核心循环

pub mod confirm;

pub use confirm::{
    describe_tool, AllowAllConfirmer, ConfirmRequest, Confirmer, Decision, DenyAllConfirmer,
    StdinConfirmer,
};

use config_system::ConfigSystem;
use context_engine::cache::ResponseCache;
use context_engine::checkpoint::CheckpointManager;
use context_engine::{ContextManager, ContextStats};
use model_router::Router;
use raven_types::*;
use std::collections::HashSet;
use std::sync::{Arc, RwLock as StdRwLock};
use tokio::sync::{mpsc, Mutex, RwLock};
use tool_system::git_first::GitFirst;
use tool_system::mcp::McpManager;
use tool_system::Registry;
use tracing::{debug, info, warn};

/// Agent 实例
pub struct Agent {
    /// 配置快照，用 `Arc<RwLock>` 持有以支持运行时热重载（见 `apply_config`）。
    /// 读取都是短暂的字段 clone，不跨 await，故用 std 同步锁即可，
    /// 同步方法（`config()`/`doctor()`）和异步方法都能直接用。
    config: Arc<StdRwLock<Config>>,
    router: Arc<Router>,
    tools: Arc<Registry>,
    context: Arc<ContextManager>,
    permission: PermissionChecker,
    cache: ResponseCache,
    git_first: GitFirst,
    /// 交互式确认回调（由 UI 层注入）。为 None 时 `ask` 模式回退为"默认拒绝"。
    confirmer: Option<Arc<dyn Confirmer>>,
    /// 崩溃恢复 checkpoint 管理器。创建失败时为 None，不阻断 Agent。
    checkpoint: Option<Arc<Mutex<CheckpointManager>>>,
    /// MCP 工具管理器。无配置或全部连接失败时为 None。
    mcp: Option<Arc<Mutex<McpManager>>>,
}

/// 权限门控决定
#[derive(Debug, Clone, PartialEq, Eq)]
enum Gate {
    /// 直接放行
    Allow,
    /// 直接拒绝，附原因
    Deny(String),
    /// 需要向用户实时确认
    NeedConfirm,
}

/// 权限检查器
#[derive(Clone)]
struct PermissionChecker {
    /// 用共享锁持有，使配置热重载能在运行时切换权限模式，
    /// 且变更对已 clone 到流式任务中的副本同样可见。
    mode: Arc<StdRwLock<String>>,
    allowed: Arc<StdRwLock<Vec<String>>>,
    denied: Arc<StdRwLock<Vec<String>>>,
    /// 本会话内"始终允许"的工具（用户选择 AllowAlways 后写入），避免反复打扰。
    session_allow: Arc<RwLock<HashSet<String>>>,
}

/// 诊断结果
#[derive(Debug, serde::Serialize)]
pub struct DoctorResult {
    pub check: String,
    pub status: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Agent {
    /// 从配置系统创建 Agent
    pub async fn from_config(cfg_sys: &ConfigSystem) -> Result<Self, AgentError> {
        let cfg = cfg_sys.config().clone();

        // 创建路由器
        let router = Arc::new(Router::new());

        // 注册默认提供商
        if cfg.api_key.is_some() {
            router
                .register_default(cfg.api_key.clone(), cfg.base_url.clone(), cfg.model.clone())
                .await?;
        }

        // 注册额外提供商
        for p in &cfg.providers {
            if let Err(e) = router.register_provider(p.clone()).await {
                warn!("注册提供商 {} 失败: {}", p.name, e);
            }
        }

        // 创建工具注册表（shell 白名单 / 超时来自配置）
        let tools = Arc::new(Registry::with_shell_config(
            cfg.tools.shell.allowed.clone(),
            cfg.tools.shell.timeout,
        ));

        // 创建上下文管理器
        let context = Arc::new(ContextManager::new(&cfg));

        // 创建权限检查器
        let permission = PermissionChecker {
            mode: Arc::new(StdRwLock::new(cfg.permission.mode.clone())),
            allowed: Arc::new(StdRwLock::new(cfg.permission.allowed_tools.clone())),
            denied: Arc::new(StdRwLock::new(cfg.permission.denied_tools.clone())),
            session_allow: Arc::new(RwLock::new(HashSet::new())),
        };

        // 创建响应缓存
        let cache = ResponseCache::default();

        // 创建 Git-first 管理器
        let git_first = GitFirst::new(cfg.git_first.enabled);

        // 创建崩溃恢复 checkpoint 管理器（失败不阻断启动）
        let checkpoint = match CheckpointManager::default() {
            Ok(cm) => Some(Arc::new(Mutex::new(cm))),
            Err(e) => {
                warn!("Checkpoint 系统初始化失败，崩溃恢复不可用: {}", e);
                None
            }
        };

        // 连接 MCP Server（逐个连接，失败的跳过不阻断启动）
        let mcp = if cfg.mcp_servers.is_empty() {
            None
        } else {
            let mut manager = McpManager::new();
            for server in &cfg.mcp_servers {
                if let Err(e) = manager.add_server(server).await {
                    warn!("MCP Server '{}' 连接失败，已跳过: {}", server.name, e);
                }
            }
            if manager.connection_count() > 0 {
                info!("MCP 已连接 {} 个 Server", manager.connection_count());
                Some(Arc::new(Mutex::new(manager)))
            } else {
                warn!("所有 MCP Server 连接失败，MCP 不可用");
                None
            }
        };

        info!("Agent 初始化完成，模型: {}", cfg.model);

        Ok(Self {
            config: Arc::new(StdRwLock::new(cfg)),
            router,
            tools,
            context,
            permission,
            cache,
            git_first,
            confirmer: None,
            checkpoint,
            mcp,
        })
    }

    /// 设置系统提示词
    pub async fn set_system_prompt(&self, prompt: impl Into<String>) {
        self.context.set_system_prompt(prompt).await;
    }

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
    async fn collect_tool_schemas(&self) -> Vec<ToolSchema> {
        let mut schemas = self.tools.list_schemas().await;
        if let Some(mcp) = &self.mcp {
            let mgr = mcp.lock().await;
            schemas.extend(mgr.all_tool_schemas());
        }
        schemas
    }

    /// 执行单个工具调用，自动区分 MCP 工具（`server__tool`）与内置工具。
    async fn dispatch_tool(&self, call: &ToolCall) -> ToolResult {
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

    /// 应用环境感知的系统提示词。
    ///
    /// 以 `template`（模板名，None 用默认模板）为角色设定基底，
    /// 自动拼接当前运行环境（OS / Shell / 工作目录）和可用工具清单，
    /// 让模型据此选择平台正确的命令（如 Windows 用 `dir` 而非 `ls`）。
    /// readonly 模式不列出工具（纯对话，模型拿不到工具）。
    pub async fn apply_system_prompt(&self, template: Option<&str>) {
        let base = template
            .and_then(config_system::prompts::find_prompt)
            .map(|t| t.prompt)
            .unwrap_or_else(config_system::prompts::default_prompt);
        let schemas = if self.permission.is_readonly() {
            Vec::new()
        } else {
            self.collect_tool_schemas().await
        };
        let full = build_system_prompt(&base, &schemas);
        self.context.set_system_prompt(full).await;
    }

    /// 使用模板设置系统提示词（同样附带环境与工具上下文）
    pub async fn set_prompt_template(&self, name: &str) -> Result<String, String> {
        match config_system::prompts::find_prompt(name) {
            Some(template) => {
                let schemas = if self.permission.is_readonly() {
                    Vec::new()
                } else {
                    self.collect_tool_schemas().await
                };
                let full = build_system_prompt(&template.prompt, &schemas);
                self.context.set_system_prompt(full).await;
                Ok(format!(
                    "已切换提示词模板: {}\n{}",
                    template.name, template.description
                ))
            }
            None => {
                let available: Vec<String> = config_system::prompts::list_prompts()
                    .into_iter()
                    .map(|p| p.name)
                    .collect();
                Err(format!(
                    "未知模板 '{}'. 可用: {}",
                    name,
                    available.join(", ")
                ))
            }
        }
    }

    /// 列出提示词模板（内置 + 用户自定义）
    pub fn list_prompt_templates() -> Vec<config_system::prompts::PromptTemplate> {
        config_system::prompts::list_prompts()
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
                    s
                };

                // 流式调用
                let messages = context.messages().await;
                let stream_result = if tool_schemas.is_empty() {
                    router.chat_stream(&model, &messages, None).await
                } else {
                    router
                        .chat_stream(&model, &messages, Some(&tool_schemas))
                        .await
                };
                let mut stream = match stream_result {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = tx.send(StreamEvent::error(e.to_string())).await;
                        return;
                    }
                };

                let mut assistant_text = String::new();
                let mut tool_calls: Vec<ToolCall> = Vec::new();

                // 转发流事件
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
                            return;
                        }
                        _ => {}
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
                let mut results = Vec::new();
                for tc in &tool_calls {
                    // 权限门控 + 交互式确认（与同步路径共用同一逻辑）
                    let result =
                        match Self::gate_and_confirm(&permission, confirmer.as_ref(), tc).await {
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

    /// 手动压缩上下文
    pub async fn compact(&self) -> Result<(), String> {
        self.context.compact().await
    }

    /// 清空上下文
    pub async fn clear(&self) {
        self.context.clear().await;
    }

    /// 获取统计
    pub async fn stats(&self) -> ContextStats {
        self.context.stats().await
    }

    /// 列出模型
    pub async fn list_models(&self) -> Vec<ModelInfo> {
        self.router.list_models().await
    }

    /// 验证提供商
    pub async fn verify_providers(&self) -> Vec<ProviderVerification> {
        self.router.verify_all().await
    }

    /// 运行诊断
    pub fn doctor(&self) -> Vec<DoctorResult> {
        let mut results = Vec::new();
        let cfg = self.config.read().unwrap();

        // API Key
        if cfg.api_key.is_none() {
            results.push(DoctorResult {
                check: "API Key".to_string(),
                status: "fail".to_string(),
                message: "未设置 API Key".to_string(),
                fix: Some("设置 RAVEN_API_KEY 环境变量或在配置文件中指定".to_string()),
            });
        } else {
            results.push(DoctorResult {
                check: "API Key".to_string(),
                status: "ok".to_string(),
                message: "已设置".to_string(),
                fix: None,
            });
        }

        // 模型
        results.push(DoctorResult {
            check: "模型".to_string(),
            status: "ok".to_string(),
            message: cfg.model.clone(),
            fix: None,
        });

        // 提供商（异步检查简化版）
        results.push(DoctorResult {
            check: "提供商".to_string(),
            status: "ok".to_string(),
            message: format!("{} 个提供商已注册", cfg.providers.len() + 1),
            fix: None,
        });

        // 工具
        results.push(DoctorResult {
            check: "工具".to_string(),
            status: "ok".to_string(),
            message: "10 个内置工具可用".to_string(),
            fix: None,
        });

        // 平台信息
        let platform = config_system::Platform::detect();
        results.push(DoctorResult {
            check: "平台".to_string(),
            status: "ok".to_string(),
            message: platform.name().to_string(),
            fix: None,
        });

        // Git-first
        let gf_status = if cfg.git_first.enabled {
            if cfg.git_first.auto_commit {
                "开启（自动提交）"
            } else {
                "开启（手动提交）"
            }
        } else {
            "关闭"
        };
        results.push(DoctorResult {
            check: "Git-first".to_string(),
            status: if cfg.git_first.enabled { "ok" } else { "info" }.to_string(),
            message: gf_status.to_string(),
            fix: None,
        });

        results
    }

    /// 获取对话历史
    pub async fn messages(&self) -> Vec<Message> {
        self.context.messages().await
    }

    // ===================================================================
    // 会话管理
    // ===================================================================

    /// 创建新会话
    pub async fn create_session(&self) -> String {
        let model = self.config.read().unwrap().model.clone();
        self.context.create_session(&model).await
    }

    /// 加载会话
    pub async fn load_session(&self, session_id: &str) -> Result<Vec<Message>, String> {
        self.context.load_session(session_id).await
    }

    /// 列出所有会话
    pub fn list_sessions(&self) -> Vec<context_engine::session::SessionMeta> {
        self.context.list_sessions()
    }

    /// 删除会话
    pub fn delete_session(&self, session_id: &str) -> Result<(), String> {
        self.context.delete_session(session_id)
    }

    /// 获取当前会话 ID
    pub async fn current_session_id(&self) -> Option<String> {
        self.context.current_session_id().await
    }

    /// 获取当前配置
    pub fn config(&self) -> Config {
        self.config.read().unwrap().clone()
    }

    /// 更新模型（运行时生效，写回共享配置）
    pub fn set_model(&self, model: impl Into<String>) {
        self.config.write().unwrap().model = model.into();
    }

    /// 更新权限模式（运行时生效，同步到权限检查器）
    pub fn set_permission_mode(&self, mode: impl Into<String>) {
        let mode = mode.into();
        self.config.write().unwrap().permission.mode = mode.clone();
        *self.permission.mode.write().unwrap() = mode;
    }

    /// 注入交互式确认回调（由 UI 层 CLI/TUI 提供）。
    ///
    /// 注入后，`ask` 模式下不在白名单的工具会触发实时确认；
    /// 未注入时 `ask` 模式对这些工具回退为"默认拒绝"以保证安全。
    pub fn set_confirmer(&mut self, confirmer: Arc<dyn Confirmer>) {
        self.confirmer = Some(confirmer);
    }

    /// 更新上下文配置
    pub fn set_context_config(&self, ctx: raven_types::ContextConfig) {
        self.config.write().unwrap().context = ctx;
    }

    /// 应用一份新配置（配置热重载入口）。
    ///
    /// 把磁盘上重新加载的配置应用到运行中的 Agent，让以下场景无需重启即生效：
    /// - 切换模型 / Base URL / API Key（重建 Router 的提供商）
    /// - 调整权限模式与白/黑名单
    /// - 开关 Git-first
    ///
    /// 不在覆盖范围（仍需重启）：MCP Server 重连、上下文预算/压缩阈值（已建对象不重置）。
    pub async fn apply_config(&self, new_cfg: Config) {
        // 1) 权限：模式 + 白/黑名单
        *self.permission.mode.write().unwrap() = new_cfg.permission.mode.clone();
        *self.permission.allowed.write().unwrap() = new_cfg.permission.allowed_tools.clone();
        *self.permission.denied.write().unwrap() = new_cfg.permission.denied_tools.clone();

        // 2) Git-first 开关
        self.git_first
            .reconfigure(new_cfg.git_first.enabled, new_cfg.git_first.auto_commit);

        // 3) 提供商：清空后按新配置重新注册（覆盖 api_key/base_url/model 变更）
        self.router.clear().await;
        if new_cfg.api_key.is_some() {
            if let Err(e) = self
                .router
                .register_default(
                    new_cfg.api_key.clone(),
                    new_cfg.base_url.clone(),
                    new_cfg.model.clone(),
                )
                .await
            {
                warn!("热重载重新注册默认提供商失败: {}", e);
            }
        }
        for p in &new_cfg.providers {
            if let Err(e) = self.router.register_provider(p.clone()).await {
                warn!("热重载重新注册提供商 {} 失败: {}", p.name, e);
            }
        }

        // 4) 落盘到共享配置快照
        let model = new_cfg.model.clone();
        let mode = new_cfg.permission.mode.clone();
        *self.config.write().unwrap() = new_cfg;
        info!("配置已热重载生效: model={}, permission={}", model, mode);
    }

    /// 获取当前权限模式
    pub fn permission_mode(&self) -> String {
        self.permission.mode.read().unwrap().clone()
    }

    /// 获取可用工具列表
    pub fn available_tools(&self) -> Vec<String> {
        vec![
            "file_read".to_string(),
            "file_write".to_string(),
            "shell".to_string(),
            "search".to_string(),
            "list_dir".to_string(),
            "git".to_string(),
        ]
    }

    // ===================================================================
    // 内部方法
    // ===================================================================

    async fn execute_tools(&self, calls: &[ToolCall]) -> Vec<ToolResult> {
        let mut results = Vec::new();

        for call in calls {
            // 权限门控 + 交互式确认
            if let Err(reason) =
                Self::gate_and_confirm(&self.permission, self.confirmer.as_ref(), call).await
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

    /// 工具执行前的权限门控 + 交互式确认（CLI 同步路径与流式路径共用）。
    ///
    /// 返回 `Ok(())` 表示放行，`Err(reason)` 表示拒绝（reason 作为工具错误结果回传给模型）。
    async fn gate_and_confirm(
        permission: &PermissionChecker,
        confirmer: Option<&Arc<dyn Confirmer>>,
        call: &ToolCall,
    ) -> Result<(), String> {
        let tool = &call.function.name;
        match permission.gate(tool).await {
            Gate::Allow => Ok(()),
            Gate::Deny(reason) => Err(reason),
            Gate::NeedConfirm => {
                let Some(confirmer) = confirmer else {
                    // 无确认回调（如非交互场景）：默认拒绝，保证安全底线。
                    return Err(format!(
                        "需要确认但当前环境无法交互，已拒绝工具 '{tool}'\n\
                         修复: 在交互终端中运行，或在配置 'permission.allowed_tools' 添加 '{tool}'，或切到 'yes' 模式。"
                    ));
                };
                let args: serde_json::Value =
                    serde_json::from_str(&call.function.arguments).unwrap_or_default();
                let detail = confirm::describe_tool(tool, &args);
                let req = ConfirmRequest {
                    tool: tool.clone(),
                    detail,
                };
                match confirmer.confirm(&req).await {
                    Decision::Allow => Ok(()),
                    Decision::AllowAlways => {
                        permission.remember_allow(tool).await;
                        Ok(())
                    }
                    Decision::Deny => Err(format!("用户拒绝执行工具 '{tool}'")),
                }
            }
        }
    }
}

// =============================================================================
// PermissionChecker
// =============================================================================

impl PermissionChecker {
    /// 当前是否只读模式。
    fn is_readonly(&self) -> bool {
        self.mode.read().unwrap().as_str() == "readonly"
    }

    /// 三态权限门控。
    ///
    /// - `readonly`：写工具一律拒绝（schema 已不下发，这里再兜底）。
    /// - `yes`：除显式 denied 外全部放行。
    /// - `auto`：除 denied 外全部放行（与 yes 类似，但保留语义区分）。
    /// - `ask`：denied → 拒绝；白名单或本会话已"始终允许" → 放行；其余 → 需确认。
    async fn gate(&self, tool_name: &str) -> Gate {
        let name = tool_name.to_string();

        if self.denied.read().unwrap().contains(&name) {
            return Gate::Deny(format!(
                "工具 '{tool_name}' 在 'permission.denied_tools' 黑名单中，已拒绝。"
            ));
        }

        // 先取出模式字符串再释放锁，避免在 await 点持有 std 锁
        let mode = self.mode.read().unwrap().clone();
        match mode.as_str() {
            "readonly" => Gate::Deny(format!(
                "只读模式（readonly）下不允许执行工具 '{tool_name}'。"
            )),
            "yes" | "auto" => Gate::Allow,
            // ask 模式（默认）
            _ => {
                if self.allowed.read().unwrap().contains(&name) {
                    return Gate::Allow;
                }
                if self.session_allow.read().await.contains(&name) {
                    return Gate::Allow;
                }
                Gate::NeedConfirm
            }
        }
    }

    /// 记录"本会话始终允许"该工具（用户选择 AllowAlways 后）。
    async fn remember_allow(&self, tool_name: &str) {
        self.session_allow
            .write()
            .await
            .insert(tool_name.to_string());
    }
}

/// 构建完整系统提示词：角色基底 + 运行环境上下文 + 可用工具清单。
///
/// 解决的问题：模型默认不知道自己跑在什么系统上，会在 Windows 下
/// 调用 `pwd`/`ls` 这类 Unix 命令导致失败。这里把平台、Shell、工作目录、
/// 以及每个工具的原始 name/description/参数拼进去，让模型据此决策。
fn build_system_prompt(base: &str, schemas: &[ToolSchema]) -> String {
    use config_system::platform;

    let p = platform::current();
    let cwd = std::env::current_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|_| "(未知)".to_string());

    // 平台特定的命令约束，避免跨平台命令误用。给出"该用什么/不该用什么"的对照。
    let shell_hint = match p {
        platform::Platform::Windows => {
            "本机是 Windows，shell 工具走 cmd.exe，只能用 Windows 命令，不要用 Unix 命令：\n\
             - 列目录: 用 `dir`，不要用 `ls`\n\
             - 当前目录: 用 `cd`，不要用 `pwd`\n\
             - 删除文件: 用 `del`，不要用 `rm`\n\
             - 查看文件: 用 `type`，不要用 `cat`\n\
             - 路径分隔符是 `\\`（如 `crates\\cli\\src`）\n\
             更推荐：读文件用 view，列目录用 list_dir，搜索用 search，改文件用 file_edit——\
             这些内置工具跨平台一致，应优先于 shell 命令。"
        }
        _ => {
            "本机是类 Unix 系统，shell 工具走 bash，可用 ls/cat/pwd/grep 等常见命令，\
             路径分隔符是 `/`。更推荐：读文件用 view，列目录用 list_dir，搜索用 search，\
             改文件用 file_edit——这些内置工具跨平台一致，应优先于 shell 命令。"
        }
    };

    let mut out = String::with_capacity(base.len() + 768 + schemas.len() * 128);
    // 第一段：基础提示词（先展开用户模板里的 {{os}}/{{shell}}/{{cwd}} 等环境占位符）
    out.push_str(&config_system::prompts::expand_placeholders(base));

    // 第二段：运行环境 + 命令约束
    out.push_str("\n\n# 运行环境\n");
    out.push_str("你正运行在以下环境中，所有命令和路径都必须与之匹配：\n");
    out.push_str(&format!(
        "- 操作系统: {} ({})\n",
        p.name(),
        platform::arch()
    ));
    out.push_str(&format!("- 默认 Shell: {}\n", p.default_shell()));
    out.push_str(&format!("- 路径分隔符: {}\n", p.path_sep()));
    out.push_str(&format!("- 工作目录: {}\n", cwd));
    out.push('\n');
    out.push_str(shell_hint);

    // 第三段：可用工具
    if !schemas.is_empty() {
        out.push_str("\n\n# 可用工具\n");
        out.push_str(
            "你可以调用以下工具。每个工具的完整参数 schema 已随请求下发，\
             调用时严格按 schema 提供 JSON 参数：\n\n",
        );
        for s in schemas {
            let f = &s.function;
            out.push_str(&format!("## {}\n{}\n", f.name, f.description));
            // 列出参数名，方便模型直接对齐（完整 schema 已在 tool definition 中下发）
            if let Some(props) = f.parameters.get("properties").and_then(|v| v.as_object()) {
                let names: Vec<&str> = props.keys().map(|k| k.as_str()).collect();
                if !names.is_empty() {
                    out.push_str(&format!("参数: {}\n", names.join(", ")));
                }
            }
            out.push('\n');
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker(mode: &str) -> PermissionChecker {
        PermissionChecker {
            mode: Arc::new(StdRwLock::new(mode.to_string())),
            allowed: Arc::new(StdRwLock::new(vec![
                "file_read".to_string(),
                "search".to_string(),
            ])),
            denied: Arc::new(StdRwLock::new(vec!["dangerous".to_string()])),
            session_allow: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    #[tokio::test]
    async fn readonly_denies_everything() {
        let c = checker("readonly");
        assert!(matches!(c.gate("file_read").await, Gate::Deny(_)));
        assert!(matches!(c.gate("shell").await, Gate::Deny(_)));
    }

    #[tokio::test]
    async fn yes_mode_allows_except_denied() {
        let c = checker("yes");
        assert_eq!(c.gate("shell").await, Gate::Allow);
        assert!(matches!(c.gate("dangerous").await, Gate::Deny(_)));
    }

    #[tokio::test]
    async fn ask_allows_whitelist_confirms_others() {
        let c = checker("ask");
        // 白名单内静默放行
        assert_eq!(c.gate("file_read").await, Gate::Allow);
        // 黑名单直接拒绝
        assert!(matches!(c.gate("dangerous").await, Gate::Deny(_)));
        // 其余需确认
        assert_eq!(c.gate("shell").await, Gate::NeedConfirm);
    }

    #[tokio::test]
    async fn ask_remembers_allow_always() {
        let c = checker("ask");
        assert_eq!(c.gate("shell").await, Gate::NeedConfirm);
        c.remember_allow("shell").await;
        // AllowAlways 后本会话不再询问
        assert_eq!(c.gate("shell").await, Gate::Allow);
    }

    #[tokio::test]
    async fn gate_and_confirm_allow_all() {
        let perm = checker("ask");
        let confirmer: Arc<dyn Confirmer> = Arc::new(AllowAllConfirmer);
        let call = ToolCall {
            index: 0,
            id: "1".to_string(),
            call_type: "function".to_string(),
            function: ToolCallFunction {
                name: "shell".to_string(),
                arguments: r#"{"command":"ls"}"#.to_string(),
            },
        };
        assert!(Agent::gate_and_confirm(&perm, Some(&confirmer), &call)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn gate_and_confirm_deny_when_user_rejects() {
        let perm = checker("ask");
        let confirmer: Arc<dyn Confirmer> = Arc::new(DenyAllConfirmer);
        let call = ToolCall {
            index: 0,
            id: "1".to_string(),
            call_type: "function".to_string(),
            function: ToolCallFunction {
                name: "shell".to_string(),
                arguments: r#"{"command":"ls"}"#.to_string(),
            },
        };
        assert!(Agent::gate_and_confirm(&perm, Some(&confirmer), &call)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn gate_and_confirm_no_confirmer_denies() {
        let perm = checker("ask");
        let call = ToolCall {
            index: 0,
            id: "1".to_string(),
            call_type: "function".to_string(),
            function: ToolCallFunction {
                name: "shell".to_string(),
                arguments: r#"{"command":"ls"}"#.to_string(),
            },
        };
        // 无确认回调（非交互场景）→ 默认拒绝
        assert!(Agent::gate_and_confirm(&perm, None, &call).await.is_err());
    }

    #[test]
    fn describe_tool_renders_summary() {
        let args = serde_json::json!({"command": "rm file.txt"});
        assert!(confirm::describe_tool("shell", &args).contains("rm file.txt"));
        let args = serde_json::json!({"path": "a.txt", "append": false});
        assert!(confirm::describe_tool("file_write", &args).contains("a.txt"));
    }

    #[test]
    fn build_system_prompt_includes_env_and_tools() {
        let schema = ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "list_dir".to_string(),
                description: "列出目录内容".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } }
                }),
            },
        };
        let out = build_system_prompt("你是助手", &[schema]);
        assert!(out.starts_with("你是助手"));
        assert!(out.contains("# 运行环境"));
        assert!(out.contains("操作系统:"));
        assert!(out.contains("# 可用工具"));
        assert!(out.contains("list_dir"));
        assert!(out.contains("参数: path"));
    }

    #[test]
    fn build_system_prompt_omits_tools_when_empty() {
        let out = build_system_prompt("base", &[]);
        assert!(out.contains("# 运行环境"));
        assert!(!out.contains("# 可用工具"));
    }
}
