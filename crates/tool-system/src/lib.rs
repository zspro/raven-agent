//! # tool-system
//! 工具注册和执行系统

use raven_types::{ToolCall, ToolResult, ToolSchema};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

pub mod builtin;
pub mod file_edit;
pub mod view;
pub mod mcp;
pub mod diff_display;
pub mod web_tools;
pub mod git_first;
pub mod repo_map;

use builtin::*;
use file_edit::*;
use view::*;
use web_tools::*;

/// 工具 trait
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> ToolSchema;
    async fn execute(&self, args: serde_json::Value) -> Result<String, String>;
}

/// 工具注册表
pub struct Registry {
    tools: Arc<RwLock<HashMap<String, Box<dyn Tool>>>>,
}

impl Registry {
    /// 创建注册表并注册所有内置工具（shell 使用安全默认白名单）
    pub fn new() -> Self {
        Self::with_shell_config(Vec::new(), 30)
    }

    /// 用 shell 配置创建注册表（白名单为空则回退到内置安全默认集）
    pub fn with_shell_config(shell_allowed: Vec<String>, shell_timeout: u64) -> Self {
        let mut map: HashMap<String, Box<dyn Tool>> = HashMap::new();

        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(FileReadTool),
            Box::new(FileWriteTool),
            Box::new(ShellTool::with_config(shell_allowed, shell_timeout)),
            Box::new(SearchTool),
            Box::new(ListDirTool),
            Box::new(GitTool),
            // Phase 5: Claude Code 风格的高级工具
            Box::new(FileEditTool),
            Box::new(ViewTool),
            // Phase 6: Web 工具
            Box::new(WebSearchTool),
            Box::new(FetchUrlTool),
        ];

        for t in tools {
            let name = t.name().to_string();
            map.insert(name, t);
        }

        info!("已注册 {} 个内置工具", map.len());

        Self {
            tools: Arc::new(RwLock::new(map)),
        }
    }

    /// 获取工具（返回新实例）
    pub fn get_tool(&self, name: &str) -> Option<Box<dyn Tool>> {
        Some(builtin_tool_by_name(name))
    }

    /// 列出所有工具的 schema
    pub async fn list_schemas(&self) -> Vec<ToolSchema> {
        let tools = self.tools.read().await;
        tools.values().map(|t| t.schema()).collect()
    }

    /// 执行工具调用
    pub async fn execute(&self, call: &ToolCall) -> ToolResult {
        let raw_args = &call.function.arguments;
        debug!(
            "执行工具: {} | args_raw_len={} | args_preview={:.200}",
            call.function.name,
            raw_args.len(),
            raw_args,
        );

        let tools = self.tools.read().await;

        if let Some(tool) = tools.get(&call.function.name) {
            let args = serde_json::from_str::<serde_json::Value>(raw_args)
                .unwrap_or_else(|e| {
                    debug!("工具参数解析失败，使用空对象: {e} | raw={:.100}", raw_args);
                    serde_json::Value::Object(serde_json::Map::new())
                });

            let start = std::time::Instant::now();

            match tool.execute(args).await {
                Ok(content) => {
                    let elapsed = start.elapsed().as_millis();
                    let truncated = raven_types::truncate(&content, 8000);

                    ToolResult {
                        tool_call_id: call.id.clone(),
                        name: call.function.name.clone(),
                        content: format!("[{}ms] {}", elapsed, truncated),
                        is_error: false,
                    }
                }
                Err(e) => {
                    ToolResult {
                        tool_call_id: call.id.clone(),
                        name: call.function.name.clone(),
                        content: format!("错误: {}", e),
                        is_error: true,
                    }
                }
            }
        } else {
            ToolResult {
                tool_call_id: call.id.clone(),
                name: call.function.name.clone(),
                content: format!("未知工具: {}", call.function.name),
                is_error: true,
            }
        }
    }
}

/// 通过名称创建内置工具实例
fn builtin_tool_by_name(name: &str) -> Box<dyn Tool> {
    match name {
        "file_read" => Box::new(FileReadTool),
        "file_write" => Box::new(FileWriteTool),
        "shell" => Box::new(ShellTool::default()),
        "search" => Box::new(SearchTool),
        "list_dir" => Box::new(ListDirTool),
        "git" => Box::new(GitTool),
        "file_edit" => Box::new(FileEditTool),
        "view" => Box::new(ViewTool),
        "web_search" => Box::new(WebSearchTool),
        "fetch_url" => Box::new(FetchUrlTool),
        _ => panic!("未知工具: {}", name),
    }
}
