//! # agent-core
//! Agent 核心循环

mod ask_user;
pub mod confirm;
mod doctor;
mod permission;
mod prompt;
mod run;
mod subagent;

pub use confirm::{
    describe_tool, AllowAllConfirmer, AskRequest, ConfirmRequest, Confirmer, Decision,
    DenyAllConfirmer, StdinConfirmer,
};
pub use doctor::DoctorResult;

use config_system::ConfigSystem;
use context_engine::cache::ResponseCache;
use context_engine::checkpoint::CheckpointManager;
use context_engine::{ContextManager, ContextStats};
use model_router::Router;
use permission::PermissionChecker;
use raven_types::*;
use std::collections::HashSet;
use std::sync::{Arc, RwLock as StdRwLock};
use tokio::sync::{Mutex, RwLock};
use tool_system::git_first::GitFirst;
use tool_system::mcp::McpManager;
use tool_system::Registry;
use tracing::{info, warn};

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

impl Agent {
    /// 从配置系统创建 Agent
    pub async fn from_config(cfg_sys: &ConfigSystem) -> Result<Self, AgentError> {
        let cfg = cfg_sys.config().clone();

        // 创建路由器
        let router = Arc::new(Router::new());

        // 注入模型推理参数（在注册提供商前设置，使客户端创建即带上参数）
        router.set_model_params(cfg.model_params.clone()).await;

        // 注入 API 调用层设置（超时 / 重试 / 流式开关），同样在注册前设置
        router.set_api_config(cfg.api.clone()).await;

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
        // 模型推理参数也一并热更新（在重新注册前设置，新客户端即带新参数）
        self.router
            .set_model_params(new_cfg.model_params.clone())
            .await;
        // API 调用层设置（超时/重试/流式开关）一并热更新
        self.router.set_api_config(new_cfg.api.clone()).await;
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
}
