//! # agent-core
//! Agent 核心循环

pub mod confirm;

pub use confirm::{
    describe_tool, AllowAllConfirmer, ConfirmRequest, Confirmer, Decision, DenyAllConfirmer,
    StdinConfirmer,
};

use raven_types::*;
use config_system::ConfigSystem;
use context_engine::{ContextManager, ContextStats};
use context_engine::cache::ResponseCache;
use model_router::Router;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
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
    /// 交互式确认回调（由 UI 层注入）。为 None 时 `ask` 模式回退为"默认拒绝"。
    confirmer: Option<Arc<dyn Confirmer>>,
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
    mode: String,
    allowed: Vec<String>,
    denied: Vec<String>,
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

        // 创建工具注册表（shell 白名单 / 超时来自配置）
        let tools = Arc::new(Registry::with_shell_config(
            cfg.tools.shell.allowed.clone(),
            cfg.tools.shell.timeout,
        ));

        // 创建上下文管理器
        let context = Arc::new(ContextManager::new(&cfg));

        // 创建权限检查器
        let permission = PermissionChecker {
            mode: cfg.permission.mode.clone(),
            allowed: cfg.permission.allowed_tools.clone(),
            denied: cfg.permission.denied_tools.clone(),
            session_allow: Arc::new(RwLock::new(HashSet::new())),
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
            confirmer: None,
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
        let permission = self.permission.clone();
        let confirmer = self.confirmer.clone();
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
                let tool_schemas = if permission.mode == "readonly" {
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
                    // 权限门控 + 交互式确认（与同步路径共用同一逻辑）
                    let result = match Self::gate_and_confirm(&permission, confirmer.as_ref(), tc).await {
                        Ok(()) => tools.execute(tc).await,
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

    /// 注入交互式确认回调（由 UI 层 CLI/TUI 提供）。
    ///
    /// 注入后，`ask` 模式下不在白名单的工具会触发实时确认；
    /// 未注入时 `ask` 模式对这些工具回退为"默认拒绝"以保证安全。
    pub fn set_confirmer(&mut self, confirmer: Arc<dyn Confirmer>) {
        self.confirmer = Some(confirmer);
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
            // 权限门控 + 交互式确认
            if let Err(reason) = Self::gate_and_confirm(
                &self.permission,
                self.confirmer.as_ref(),
                call,
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
                    Decision::Deny => {
                        Err(format!("用户拒绝执行工具 '{tool}'"))
                    }
                }
            }
        }
    }
}

// =============================================================================
// PermissionChecker
// =============================================================================

impl PermissionChecker {
    /// 三态权限门控。
    ///
    /// - `readonly`：写工具一律拒绝（schema 已不下发，这里再兜底）。
    /// - `yes`：除显式 denied 外全部放行。
    /// - `auto`：除 denied 外全部放行（与 yes 类似，但保留语义区分）。
    /// - `ask`：denied → 拒绝；白名单或本会话已"始终允许" → 放行；其余 → 需确认。
    async fn gate(&self, tool_name: &str) -> Gate {
        let name = tool_name.to_string();

        if self.denied.contains(&name) {
            return Gate::Deny(format!(
                "工具 '{tool_name}' 在 'permission.denied_tools' 黑名单中，已拒绝。"
            ));
        }

        match self.mode.as_str() {
            "readonly" => Gate::Deny(format!(
                "只读模式（readonly）下不允许执行工具 '{tool_name}'。"
            )),
            "yes" | "auto" => Gate::Allow,
            // ask 模式（默认）
            _ => {
                if self.allowed.contains(&name) {
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
        self.session_allow.write().await.insert(tool_name.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker(mode: &str) -> PermissionChecker {
        PermissionChecker {
            mode: mode.to_string(),
            allowed: vec!["file_read".to_string(), "search".to_string()],
            denied: vec!["dangerous".to_string()],
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
        assert!(Agent::gate_and_confirm(&perm, Some(&confirmer), &call).await.is_ok());
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
        assert!(Agent::gate_and_confirm(&perm, Some(&confirmer), &call).await.is_err());
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
}
