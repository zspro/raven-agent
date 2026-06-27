//! 多 agent 并行：`task` 工具派生无状态子 agent。
//!
//! 设计（Claude Code 风格）：主 agent 通过 `task` 工具派生子 agent，子 agent 用**独立上下文**
//! 加全部内置工具（**禁递归**，看不到 task 工具）独立跑完整工具循环，返回单个最终文本；
//! 主 agent 一条消息可发多个 task，由 `run_parallel_tasks` 用 Semaphore 限流并发执行。
//!
//! 借鉴 agent_society 的"结构化任务委托"（每个 task 带 description + prompt），
//! 但刻意不做消息总线、树形递归、长生命周期等重型设施——少即是多。
//!
//! 关键约束：
//! - 子 agent 复用主 agent 的 `router` / `tools`（Arc 共享），但 context 每次新建且关闭持久化，
//!   不污染主会话、不写 checkpoint、不进响应缓存。
//! - 子 agent **不触发交互式确认**：并行子 agent 在并发任务里跑，无法串行读 stdin 确认，
//!   故对内置工具一律放行（继承"主 agent 已授权使用工具"的前提）。

use crate::Agent;
use context_engine::{ContextManager, PersistenceOptions};
use futures::future::join_all;
use model_router::Router;
use raven_types::*;
use std::sync::{Arc, RwLock as StdRwLock};
use tokio::sync::Semaphore;
use tool_system::Registry;
use tracing::debug;

impl Agent {
    /// `task` 工具的 schema。仅在非 readonly 时追加到主 agent 的工具清单。
    pub(crate) fn task_tool_schema() -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "task".to_string(),
                description:
                    "派生一个独立的子 agent 来完成一个自包含的子任务。子 agent 拥有完整工具集\
                    （读写文件、shell、搜索等），用独立上下文跑完一整轮后只返回最终结果文本。\
                    适合可并行、互不依赖的子任务（如同时调查多个文件/模块）。\
                    一条消息里可同时调用多个 task 实现并行。子 agent 不能再派生 task（禁递归）。"
                        .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "description": {
                            "type": "string",
                            "description": "子任务的简短描述（3-6 字），用于界面展示"
                        },
                        "prompt": {
                            "type": "string",
                            "description": "给子 agent 的完整指令，应自包含、明确说明要做什么和要返回什么"
                        }
                    },
                    "required": ["description", "prompt"]
                }),
            },
        }
    }

    /// 并发执行一批 task 子任务，返回 `(description, result_text, is_error)` 列表（保持输入顺序）。
    /// 用 Semaphore 限制同时运行的子 agent 数（config.tools.max_parallel_agents）。
    ///
    /// 设计成只依赖 Arc 字段（不借 `&self`），因为流式路径在 `tokio::spawn` 里只持有 Arc 克隆，
    /// 拿不到 `&self`；同步路径与流式路径都调用本函数，避免逻辑分叉。
    pub(crate) async fn run_parallel_tasks(
        config: Arc<StdRwLock<Config>>,
        router: Arc<Router>,
        tools: Arc<Registry>,
        tasks: Vec<(String, String)>,
    ) -> Vec<(String, String, bool)> {
        let max = config.read().unwrap().tools.max_parallel_agents.max(1);
        let sem = Arc::new(Semaphore::new(max));
        debug!("并发执行 {} 个子任务，上限 {}", tasks.len(), max);

        let futs = tasks.into_iter().map(|(desc, prompt)| {
            let sem = sem.clone();
            let config = config.clone();
            let router = router.clone();
            let tools = tools.clone();
            async move {
                let _permit = sem.acquire().await.expect("semaphore closed");
                match Self::run_subagent(&config, &router, &tools, &prompt).await {
                    Ok(text) => (desc, text, false),
                    Err(e) => (desc, format!("子 agent 执行失败: {e}"), true),
                }
            }
        });

        join_all(futs).await
    }

    /// 单个子 agent：独立上下文 + 全内置工具（无 task、无 MCP）跑完整工具循环，返回最终文本。
    pub(crate) async fn run_subagent(
        config: &Arc<StdRwLock<Config>>,
        router: &Arc<Router>,
        tools: &Arc<Registry>,
        prompt: &str,
    ) -> Result<String, AgentError> {
        // 独立上下文，关闭持久化，不污染主会话
        let cfg = config.read().unwrap().clone();
        let ctx = ContextManager::with_persistence(
            &cfg,
            PersistenceOptions {
                enabled: false,
                session_id: None,
                model: cfg.model.clone(),
            },
        );
        ctx.set_system_prompt(SUBAGENT_SYSTEM_PROMPT).await;
        ctx.add_user_message(prompt).await;

        // 子 agent 工具 = 主 agent 内置工具（list_schemas 不含 task、不含 MCP），天然禁递归
        let schemas = tools.list_schemas().await;
        let model = cfg.model.clone();

        loop {
            ctx.check_budget()?;
            if ctx.should_compact().await {
                ctx.compact().await.map_err(AgentError::Internal)?;
            }

            let messages = ctx.messages().await;
            let resp = router.chat(&model, &messages, Some(&schemas)).await?;
            ctx.record_usage_async(&resp.usage).await;

            if resp.tool_calls.is_empty() {
                ctx.add_assistant_message(&resp.content, Vec::new()).await;
                return Ok(resp.content);
            }

            ctx.add_assistant_message(&resp.content, resp.tool_calls.clone())
                .await;

            // 子 agent 工具直接放行执行（不交互确认；并行场景无法串行读 stdin）。
            // 子 agent 工具集不含 MCP，直接走内置注册表即可。
            let mut results = Vec::with_capacity(resp.tool_calls.len());
            for call in &resp.tool_calls {
                results.push(tools.execute(call).await);
            }
            ctx.add_tool_results(results).await;
        }
    }
}

/// 子 agent 的系统提示：专注单一子任务、用工具、给简洁结论。
const SUBAGENT_SYSTEM_PROMPT: &str = "你是一个子 agent，被主 agent 派来完成一个自包含的子任务。\
你拥有完整的工具集（读写文件、shell、搜索、查看等），请独立调用工具完成任务。\
完成后，用简洁、结构化的文本返回最终结果——主 agent 只会看到你的最终回复，看不到你的中间过程，\
所以请把关键发现、结论、产出都写在最终回复里。不要寒暄，直接给结果。";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_schema_has_required_fields() {
        let s = Agent::task_tool_schema();
        assert_eq!(s.function.name, "task");
        let props = s.function.parameters.get("properties").unwrap();
        assert!(props.get("description").is_some());
        assert!(props.get("prompt").is_some());
        let req = s.function.parameters.get("required").unwrap();
        assert_eq!(req.as_array().unwrap().len(), 2);
    }
}
