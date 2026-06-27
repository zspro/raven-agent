//! # context-engine
//! 上下文管理和 Token 预算

pub mod budget;
pub mod cache;
pub mod checkpoint;
pub mod session;

pub use budget::ContextStats;
use budget::{TokenBudget, UsageStats};

use raven_types::{Config, ContextConfig, Message, Role, TokenUsage};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// 会话持久化选项
#[derive(Debug, Clone)]
pub struct PersistenceOptions {
    pub enabled: bool,
    pub session_id: Option<String>,
    pub model: String,
}

impl Default for PersistenceOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            session_id: None,
            model: "gpt-4o".to_string(),
        }
    }
}

/// 上下文管理器
pub struct ContextManager {
    messages: Arc<RwLock<Vec<Message>>>,
    system_prompt: Arc<RwLock<Option<String>>>,
    config: ContextConfig,
    budget: TokenBudget,
    stats: Arc<RwLock<UsageStats>>,
    session_store: Option<session::SessionStore>,
    current_session: Arc<RwLock<Option<session::SessionMeta>>>,
    #[allow(dead_code)]
    persistence: PersistenceOptions,
}

impl ContextManager {
    /// 创建新的上下文管理器
    pub fn new(cfg: &Config) -> Self {
        Self::with_persistence(cfg, PersistenceOptions::default())
    }

    /// 创建带持久化的上下文管理器
    pub fn with_persistence(cfg: &Config, opts: PersistenceOptions) -> Self {
        let session_store = if opts.enabled {
            match session::SessionStore::default() {
                Ok(store) => {
                    info!("会话持久化已启用");
                    Some(store)
                }
                Err(e) => {
                    warn!("会话持久化初始化失败: {}", e);
                    None
                }
            }
        } else {
            None
        };

        Self {
            messages: Arc::new(RwLock::new(Vec::new())),
            system_prompt: Arc::new(RwLock::new(None)),
            config: cfg.context.clone(),
            budget: TokenBudget::new(cfg.token_budget),
            stats: Arc::new(RwLock::new(UsageStats::default())),
            session_store,
            current_session: Arc::new(RwLock::new(None)),
            persistence: opts,
        }
    }

    /// 设置系统提示词
    pub async fn set_system_prompt(&self, prompt: impl Into<String>) {
        let mut sp = self.system_prompt.write().await;
        *sp = Some(prompt.into());
    }

    /// 添加用户消息
    pub async fn add_user_message(&self, content: impl Into<String>) {
        let mut msgs = self.messages.write().await;
        msgs.push(Message::user(content));
        drop(msgs);
        self.auto_save().await;
    }

    /// 添加助手消息
    pub async fn add_assistant_message(
        &self,
        content: impl Into<String>,
        tool_calls: Vec<raven_types::ToolCall>,
    ) {
        let mut msgs = self.messages.write().await;
        let mut msg = Message::assistant(content);
        if !tool_calls.is_empty() {
            msg.tool_calls = Some(tool_calls);
        }
        msgs.push(msg);
        drop(msgs);
        self.auto_save().await;
    }

    /// 添加工具结果
    pub async fn add_tool_results(&self, results: Vec<raven_types::ToolResult>) {
        let mut msgs = self.messages.write().await;
        for r in results {
            msgs.push(Message::tool_result(&r.tool_call_id, &r.name, &r.content));
        }
        drop(msgs);
        self.auto_save().await;
    }

    /// 获取完整消息列表（含系统提示词）
    pub async fn messages(&self) -> Vec<Message> {
        let mut result = Vec::new();

        // 系统提示词
        let sp = self.system_prompt.read().await;
        if let Some(prompt) = sp.as_ref() {
            result.push(Message::system(prompt.clone()));
        }

        // 对话历史
        let msgs = self.messages.read().await;
        result.extend(msgs.clone());

        result
    }

    /// 从 checkpoint 恢复消息历史（覆盖当前历史）。
    /// 传入的消息若以 system 消息开头，会被剥离（系统提示词单独管理）。
    /// 例外：压缩产生的「[历史对话摘要]」系统消息是会话内容的一部分，需保留，
    /// 否则恢复会话时会丢失被压缩掉的早期对话。
    pub async fn restore_messages(&self, restored: Vec<Message>) {
        let mut msgs = self.messages.write().await;
        msgs.clear();
        for m in restored {
            if m.role == raven_types::Role::System && !m.content.starts_with("[历史对话摘要]")
            {
                continue;
            }
            msgs.push(m);
        }
    }

    /// 判断是否需要压缩
    pub async fn should_compact(&self) -> bool {
        let tokens = self.estimate_tokens().await;
        tokens > self.config.compact_threshold && self.config.compact_threshold > 0
    }

    /// 压缩上下文
    /// 保留最近的 keep_rounds 轮完整对话，对更早的对话进行摘要
    pub async fn compact(&self) -> Result<(), String> {
        let keep_count = self.config.keep_rounds * 2; // 每轮 = user + assistant

        let mut msgs = self.messages.write().await;

        let msg_len = msgs.len();
        if msg_len <= keep_count {
            return Ok(()); // 对话太少
        }

        // 提取需要压缩的部分
        let mut split_at = msg_len - keep_count;
        // 避免把「带 tool_calls 的 assistant」与其后的 tool 结果切开：
        // 若保留段开头是孤立的 Tool 消息（对应的 assistant 已被压缩），
        // 向后推进切点，把这些孤儿一并纳入压缩段，否则 API 会拒绝
        // 「没有前置 tool_calls 的 tool 消息」。
        while split_at < msg_len && msgs[split_at].role == raven_types::Role::Tool {
            split_at += 1;
        }
        if split_at >= msg_len {
            return Ok(()); // 全是待压缩内容，本轮不动（极少见）
        }
        let to_compress: Vec<Message> = msgs.drain(..split_at).collect();

        // 生成摘要
        let summary = self.summarize(&to_compress);

        // 重建消息列表：摘要 + 保留的消息
        let mut new_msgs = vec![Message::system(format!("[历史对话摘要] {}", summary))];
        new_msgs.extend(std::mem::take(&mut *msgs));

        *msgs = new_msgs;

        info!(
            "上下文已压缩: {} -> {} 条消息",
            to_compress.len() + keep_count,
            msgs.len()
        );

        Ok(())
    }

    /// 记录 token 使用（同步更新预算，异步更新统计）
    pub fn record_usage(&self, usage: &TokenUsage) {
        self.budget.add(usage.input + usage.output);
    }

    /// 记录统计（异步版本，同时更新预算和统计）
    pub async fn record_usage_async(&self, usage: &TokenUsage) {
        self.budget.add(usage.input + usage.output);
        let mut stats = self.stats.write().await;
        stats.total_input += usage.input;
        stats.total_output += usage.output;
    }

    /// 检查 token 预算
    pub fn check_budget(&self) -> Result<(), raven_types::AgentError> {
        self.budget.check()
    }

    /// 获取统计
    pub async fn stats(&self) -> ContextStats {
        let current_tokens = self.estimate_tokens().await;
        let msg_count = self.messages.read().await.len();
        let stats = self.stats.read().await;

        ContextStats {
            current_context_tokens: current_tokens,
            total_input_tokens: stats.total_input,
            total_output_tokens: stats.total_output,
            total_tokens: stats.total_input + stats.total_output,
            message_count: msg_count,
            budget_status: self.budget.status(),
        }
    }

    /// 清空上下文
    pub async fn clear(&self) {
        let mut msgs = self.messages.write().await;
        msgs.clear();
        self.budget.reset();
        // 重置当前会话
        let mut sess = self.current_session.write().await;
        *sess = None;
        debug!("上下文已清空");
    }

    // ===================================================================
    // 会话管理
    // ===================================================================

    /// 创建新会话
    pub async fn create_session(&self, model: impl Into<String>) -> String {
        let model = model.into();
        let mut sess = self.current_session.write().await;
        let store = match &self.session_store {
            Some(s) => s,
            None => {
                let id = session::generate_id();
                *sess = Some(session::SessionMeta {
                    id: id.clone(),
                    title: "新会话".to_string(),
                    created_at: chrono::Local::now(),
                    updated_at: chrono::Local::now(),
                    message_count: 0,
                    model: model.clone(),
                });
                return id;
            }
        };

        let meta = store.create(model);
        let id = meta.id.clone();
        // 清空当前消息
        let mut msgs = self.messages.write().await;
        msgs.clear();
        self.budget.reset();
        *sess = Some(meta);
        info!("新会话已创建: {}", id);
        id
    }

    /// 加载会话
    pub async fn load_session(&self, session_id: &str) -> Result<Vec<Message>, String> {
        let store = self.session_store.as_ref().ok_or("会话持久化未启用")?;

        let session = store.load(session_id)?;

        // 替换当前消息
        let mut msgs = self.messages.write().await;
        *msgs = session.messages.clone();
        self.budget.reset();

        let mut sess = self.current_session.write().await;
        *sess = Some(session.meta);

        info!("会话已加载: {} ({} 条消息)", session_id, msgs.len());
        Ok(msgs.clone())
    }

    /// 列出所有会话
    pub fn list_sessions(&self) -> Vec<session::SessionMeta> {
        match &self.session_store {
            Some(store) => store.list(),
            None => Vec::new(),
        }
    }

    /// 删除会话
    pub fn delete_session(&self, session_id: &str) -> Result<(), String> {
        match &self.session_store {
            Some(store) => store.delete(session_id),
            None => Err("会话持久化未启用".to_string()),
        }
    }

    /// 获取当前会话 ID
    pub async fn current_session_id(&self) -> Option<String> {
        let sess = self.current_session.read().await;
        sess.as_ref().map(|s| s.id.clone())
    }

    /// 自动保存当前会话
    async fn auto_save(&self) {
        let store = match &self.session_store {
            Some(s) => s,
            None => return,
        };

        let mut sess_opt = self.current_session.write().await;
        let sess = match sess_opt.as_mut() {
            Some(s) => s,
            None => return,
        };

        let messages = self.messages.read().await;
        if let Err(e) = store.save(sess, &messages) {
            warn!("自动保存失败: {}", e);
        }
    }

    // ===================================================================
    // 内部方法
    // ===================================================================

    /// 估算当前 token 数
    async fn estimate_tokens(&self) -> usize {
        let msgs = self.messages.read().await;

        let mut total = 0usize;

        // 系统提示词
        let sp = self.system_prompt.read().await;
        if let Some(prompt) = sp.as_ref() {
            total += raven_types::estimate_tokens(prompt);
        }

        // 所有消息
        for msg in msgs.iter() {
            total += msg.estimate_tokens();
        }

        total
    }

    /// 生成对话摘要
    fn summarize(&self, messages: &[Message]) -> String {
        let mut topics = Vec::new();
        let mut user_count = 0;
        let mut tool_call_count = 0;

        for msg in messages {
            match msg.role {
                Role::User => {
                    user_count += 1;
                    let preview: String = msg.content.chars().take(30).collect();
                    if !preview.is_empty() {
                        topics.push(preview);
                    }
                }
                Role::Assistant => {
                    tool_call_count += msg.tool_calls.as_ref().map_or(0, |tc| tc.len());
                }
                _ => {}
            }
        }

        // 去重
        let mut seen = std::collections::HashSet::new();
        let unique: Vec<String> = topics
            .into_iter()
            .filter(|t| seen.insert(t.clone()))
            .collect();

        let mut result = format!("共{}轮对话", user_count);
        if tool_call_count > 0 {
            result.push_str(&format!(", {}次工具调用", tool_call_count));
        }
        if !unique.is_empty() {
            result.push_str(&format!(", 涉及: {}", unique.join("; ")));
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raven_types::{Config, Message, ToolCall, ToolCallFunction};

    fn mgr_with_keep(keep_rounds: usize) -> ContextManager {
        let mut cfg = Config::default();
        cfg.context.keep_rounds = keep_rounds;
        // 关闭持久化，避免测试写入 ~/.raven
        ContextManager::with_persistence(
            &cfg,
            PersistenceOptions {
                enabled: false,
                session_id: None,
                model: "test".to_string(),
            },
        )
    }

    fn assistant_with_tool(call_id: &str) -> Message {
        let mut m = Message::assistant("");
        m.tool_calls = Some(vec![ToolCall {
            index: 0,
            id: call_id.to_string(),
            call_type: "function".to_string(),
            function: ToolCallFunction {
                name: "shell".to_string(),
                arguments: "{}".to_string(),
            },
        }]);
        m
    }

    /// B7：压缩切点落在「assistant(tool_calls) + tool 结果」之间时，
    /// 保留段不应以孤立 Tool 消息开头。
    #[tokio::test]
    async fn test_compact_does_not_orphan_tool_results() {
        let mgr = mgr_with_keep(1); // keep_count = 2
                                    // 构造：user, assistant(tool), tool, assistant(最终回答)
        mgr.add_user_message("问题").await;
        {
            let mut msgs = mgr.messages.write().await;
            msgs.push(assistant_with_tool("c1"));
            msgs.push(Message::tool_result("c1", "shell", "输出"));
            msgs.push(Message::assistant("最终回答"));
        }
        // 原始 split_at = 4 - 2 = 2，恰好落在 tool 结果之前。
        mgr.compact().await.unwrap();

        let msgs = mgr.messages.read().await;
        // 第一条是摘要 system，其后不得是孤立 Tool 消息
        assert_eq!(msgs[0].role, Role::System);
        assert!(
            msgs.get(1).map(|m| m.role) != Some(Role::Tool),
            "保留段不应以孤立 Tool 消息开头"
        );
        // 不存在「Tool 紧跟在非 assistant(tool_calls) 之后」的破损配对
        for w in msgs.windows(2) {
            if w[1].role == Role::Tool {
                assert!(
                    w[0].role == Role::Assistant && w[0].tool_calls.is_some()
                        || w[0].role == Role::Tool,
                    "Tool 消息前必须是带 tool_calls 的 assistant 或另一条 Tool"
                );
            }
        }
    }

    /// B20：恢复会话时应保留「[历史对话摘要]」系统消息。
    #[tokio::test]
    async fn test_restore_keeps_compaction_summary() {
        let mgr = mgr_with_keep(2);
        let restored = vec![
            Message::system("你是助手"),                  // 普通系统提示 → 应剥离
            Message::system("[历史对话摘要] 之前聊了 X"), // 压缩摘要 → 应保留
            Message::user("继续"),
        ];
        mgr.restore_messages(restored).await;
        let msgs = mgr.messages.read().await;
        assert_eq!(msgs.len(), 2, "普通系统提示被剥离，摘要与用户消息保留");
        assert!(msgs[0].content.starts_with("[历史对话摘要]"));
        assert_eq!(msgs[1].role, Role::User);
    }
}
