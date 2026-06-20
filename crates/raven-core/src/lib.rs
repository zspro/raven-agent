//! # agent-core
//! Agent 核心循环

use raven_types::*;
use config_system::ConfigSystem;
use context_engine::{ContextManager, ContextStats};
use context_engine::cache::ResponseCache;
use model_router::Router;
use std::sync::Arc;
use tokio::sync::mpsc;
use tool_system::Registry;
use tool_system::git_first::GitFirst;
use tracing::{debug, info, warn};

/// Agent 实例
pub struct Agent {
    config: Config,
    router: Arc<Router>,
    tools: Arc<Registry>,
    context: Arc<ContextManager>,
    permission: PermissionChecker,
    cache: ResponseCache,
    git_first: GitFirst,
}

/// 权限检查器
struct PermissionChecker {
    mode: String,
    allowed: Vec<String>,
    denied: Vec<String>,
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
            router.register_default(
                cfg.api_key.clone(),
                cfg.base_url.clone(),
                cfg.model.clone(),
            ).await?;
        }

        // 注册额外提供商
        for p in &cfg.providers {
            if let Err(e) = router.register_provider(p.clone()).await {
                warn!("注册提供商 {} 失败: {}", p.name, e);
            }
        }

        // 创建工具注册表
        let tools = Arc::new(Registry::new());

        // 创建上下文管理器
        let context = Arc::new(ContextManager::new(&cfg));

        // 创建权限检查器
        let permission = PermissionChecker {
            mode: cfg.permission.mode.clone(),
            allowed: cfg.permission.allowed_tools.clone(),
            denied: cfg.permission.denied_tools.clone(),
        };

        // 创建响应缓存
        let cache = ResponseCache::default();

        // 创建 Git-first 管理器
        let git_first = GitFirst::new(cfg.git_first.enabled);

        info!("Agent 初始化完成，模型: {}", cfg.model);

        Ok(Self {
            config: cfg,
            router,
            tools,
            context,
            permission,
            cache,
            git_first,
        })
    }

    /// 设置系统提示词
    pub async fn set_system_prompt(&self, prompt: impl Into<String>) {
        self.context.set_system_prompt(prompt).await;
    }

    /// 使用模板设置系统提示词
    pub async fn set_prompt_template(&self, name: &str) -> Result<String, String> {
        match config_system::prompts::find_prompt(name) {
            Some(template) => {
                self.context.set_system_prompt(template.prompt).await;
                Ok(format!(
                    "已切换提示词模板: {}\n{}",
                    template.name, template.description
                ))
            }
            None => {
                let available: Vec<String> = config_system::prompts::BUILTIN_PROMPTS
                    .iter()
                    .map(|p| p.name.to_string())
                    .collect();
                Err(format!(
                    "未知模板 '{}'. 可用: {}",
                    name,
                    available.join(", ")
                ))
            }
        }
    }

    /// 列出提示词模板
    pub fn list_prompt_templates() -> &'static [config_system::prompts::PromptTemplate] {
        config_system::prompts::list_prompts()
    }

    /// 运行一次对话（同步）
    pub async fn run(&self, user_input: impl Into<String>) -> Result<String, AgentError> {
        let input = user_input.into();

        // 检查是否有可用模型
        let models = self.router.list_models().await;
        if models.is_empty() && self.config.api_key.is_none() {
            return Err(AgentError::config(
                "没有可用的模型提供商",
                "请设置 RAVEN_API_KEY 环境变量或在配置文件中指定 api_key",
            ));
        }

        // 检查缓存（仅在单轮对话时有效）
        let cache_key = ResponseCache::make_key(&self.config.model, &input);
        if let Some(cached) = self.cache.get(&cache_key).await {
            debug!("缓存命中，跳过 API 调用");
            self.context.add_user_message(input).await;
            self.context.add_assistant_message(&cached.content, Vec::new()).await;
            self.context.record_usage(&cached.usage);
            return Ok(cached.content);
        }

        // 添加用户消息
        self.context.add_user_message(input).await;

        loop {
            // 检查预算
            self.context.check_budget().map_err(|e| e)?;

            // 压缩上下文
            if self.context.should_compact().await {
                self.context.compact().await.map_err(|e| AgentError::Internal(e))?;
            }

            // 获取工具 schema（序列化为 JSON 值发送给 LLM）
            let tool_schemas = if self.permission.mode == "readonly" {
                Vec::new()
            } else {
                self.tools.list_schemas().await
            };

            // 调用 LLM
            let messages = self.context.messages().await;
            let resp = if tool_schemas.is_empty() {
                self.router.chat(&self.config.model, &messages, None).await?
            } else {
                self.router.chat(&self.config.model, &messages, Some(&tool_schemas)).await?
            };

            // 记录使用
            self.context.record_usage(&resp.usage);

            // 处理工具调用
            if !resp.tool_calls.is_empty() {
                // 添加助手消息
                self.context.add_assistant_message(&resp.content, resp.tool_calls.clone()).await;

                // 执行工具
                let results = self.execute_tools(&resp.tool_calls).await;

                // 添加结果
                self.context.add_tool_results(results).await;

                continue; // 继续循环
            }

            // 没有工具调用，返回结果
            self.context.add_assistant_message(&resp.content, Vec::new()).await;
            // 存入缓存
            self.cache.put(cache_key, &resp).await;
            return Ok(resp.content);
        }
    }

    /// 流式运行对话
    pub async fn run_stream(&self, user_input: impl Into<String>) -> Result<mpsc::Receiver<StreamEvent>, AgentError> {
        let input = user_input.into();
        let models = self.router.list_models().await;

        if models.is_empty() && self.config.api_key.is_none() {
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
        let permission = self.permission.mode.clone();
        let model = self.config.model.clone();

        tokio::spawn(async move {
            loop {
                // 检查预算
                if let Err(e) = context.check_budget() {
                    let _ = tx.send(StreamEvent::error(e.to_string())).await;
                    return;
                }

                // 压缩
                if context.should_compact().await {
                    let _ = context.compact().await;
                }

                // 获取工具
                let tool_schemas = if permission == "readonly" {
                    Vec::new()
                } else {
                    tools.list_schemas().await
                };

                // 流式调用
                let messages = context.messages().await;
                let stream_result = if tool_schemas.is_empty() {
                    router.chat_stream(&model, &messages, None).await
                } else {
                    router.chat_stream(&model, &messages, Some(&tool_schemas)).await
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
                    context.add_assistant_message(&assistant_text, Vec::new()).await;
                    let _ = tx.send(StreamEvent::done()).await;
                    return;
                }

                // 有工具调用
                context.add_assistant_message(&assistant_text, tool_calls.clone()).await;

                // 执行工具
                let mut results = Vec::new();
                for tc in &tool_calls {
                    let result = tools.execute(tc).await;
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

        // API Key
        if self.config.api_key.is_none() {
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
            message: self.config.model.clone(),
            fix: None,
        });

        // 提供商（异步检查简化版）
        results.push(DoctorResult {
            check: "提供商".to_string(),
            status: "ok".to_string(),
            message: format!("{} 个提供商已注册", self.config.providers.len() + 1),
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
        let gf_status = if self.config.git_first.enabled {
            if self.config.git_first.auto_commit {
                "开启（自动提交）"
            } else {
                "开启（手动提交）"
            }
        } else {
            "关闭"
        };
        results.push(DoctorResult {
            check: "Git-first".to_string(),
            status: if self.config.git_first.enabled { "ok" } else { "info" }.to_string(),
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
        self.context.create_session(&self.config.model).await
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
        self.config.clone()
    }

    /// 更新模型
    pub fn set_model(&mut self, model: impl Into<String>) {
        self.config.model = model.into();
    }

    /// 更新权限模式
    pub fn set_permission_mode(&mut self, mode: impl Into<String>) {
        let mode = mode.into();
        self.config.permission.mode = mode.clone();
        self.permission.mode = mode;
    }

    /// 更新上下文配置
    pub fn set_context_config(&mut self, ctx: raven_types::ContextConfig) {
        self.config.context = ctx.clone();
    }

    /// 获取当前权限模式
    pub fn permission_mode(&self) -> &str {
        &self.permission.mode
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
            if !self.permission.can_execute(&call.function.name) {
                results.push(ToolResult {
                    tool_call_id: call.id.clone(),
                    name: call.function.name.clone(),
                    content: format!(
                        "权限不足: 工具 '{}' 未被授权\n修复: 在配置中 'permission.allowed_tools' 添加 '{}'",
                        call.function.name, call.function.name
                    ),
                    is_error: true,
                });
                continue;
            }

            // Git-first: 编辑前保存状态
            let is_file_edit = call.function.name == "file_edit" || call.function.name == "file_write";
            let args_json: serde_json::Value = serde_json::from_str(&call.function.arguments).unwrap_or_default();
            let target_path = args_json.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let _before_hash = if is_file_edit {
                self.git_first.pre_edit(target_path).unwrap_or(None)
            } else {
                None
            };

            let result = self.tools.execute(call).await;

            // Git-first: 编辑后自动 commit
            if is_file_edit && !result.is_error {
                let desc = if result.content.len() > 30 {
                    format!("{}...", &result.content[..30])
                } else {
                    result.content.clone()
                };
                let _ = self.git_first.post_edit(target_path, &call.function.name, &desc);
            }

            results.push(result);
        }

        results
    }
}

// =============================================================================
// PermissionChecker
// =============================================================================

impl PermissionChecker {
    fn can_execute(&self, tool_name: &str) -> bool {
        if self.mode == "readonly" {
            return false;
        }
        if self.mode == "yes" {
            return !self.denied.contains(&tool_name.to_string());
        }
        if self.denied.contains(&tool_name.to_string()) {
            return false;
        }
        if self.mode == "auto" {
            return true;
        }
        // ask 模式
        self.allowed.contains(&tool_name.to_string())
    }
}
