//! MCP (Model Context Protocol) 客户端
//!
//! MCP 是 Anthropic 推出的开放协议，允许 AI 助手连接外部工具和数据源。
//! 通过这个模块，Raven 可以调用任何 MCP Server 提供的工具。
//!
//! 使用方式:
//! 1. 启动 MCP Server (如 npx @anthropics/mcp-server-filesystem)
//! 2. 在配置中添加 [[mcp_servers]]
//! 3. Agent 自动发现并调用 MCP 工具

use raven_types::{FunctionSchema, McpServerConfig, ToolSchema};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tracing::{debug, info, warn};

// =============================================================================
// MCP 协议类型
// =============================================================================

/// MCP 工具定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// MCP 请求
#[derive(Debug, Serialize)]
struct McpRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

/// MCP 响应
#[derive(Debug, Deserialize)]
struct McpResponse {
    #[allow(dead_code)]
    id: u64,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<McpError>,
}

#[derive(Debug, Deserialize)]
struct McpError {
    code: i32,
    message: String,
}

// =============================================================================
// MCP 客户端
// =============================================================================

/// MCP Server 连接
pub struct McpConnection {
    name: String,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    request_id: u64,
    tools: Vec<McpTool>,
    #[allow(dead_code)]
    child: Child,
}

impl McpConnection {
    /// 连接到 MCP Server
    pub async fn connect(config: &McpServerConfig) -> Result<Self, String> {
        info!("连接 MCP Server: {}", config.name);

        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        if let Some(env_vars) = &config.env {
            cmd.envs(env_vars);
        }

        let mut child = cmd.spawn().map_err(|e| {
            format!(
                "启动 MCP Server '{}' 失败: {} (命令: {} {:?})",
                config.name, e, config.command, config.args
            )
        })?;

        let stdin = child.stdin.take().ok_or("无法获取 stdin")?;
        let stdout = child.stdout.take().ok_or("无法获取 stdout")?;
        let stdout_reader = BufReader::new(stdout);

        let mut conn = Self {
            name: config.name.clone(),
            stdin,
            stdout: stdout_reader,
            request_id: 0,
            tools: Vec::new(),
            child,
        };

        // 初始化连接
        conn.initialize().await?;

        // 发现工具
        conn.discover_tools().await?;

        info!(
            "MCP Server '{}' 已连接，发现 {} 个工具",
            config.name,
            conn.tools.len()
        );

        Ok(conn)
    }

    /// 获取发现的工具列表
    pub fn tools(&self) -> &[McpTool] {
        &self.tools
    }

    /// 将 MCP 工具转换为内部 ToolSchema
    pub fn to_tool_schemas(&self) -> Vec<ToolSchema> {
        self.tools
            .iter()
            .map(|t| ToolSchema {
                schema_type: "function".to_string(),
                function: FunctionSchema {
                    name: format!("{}__{}", self.name, t.name),
                    description: format!("[MCP:{}] {}", self.name, t.description),
                    parameters: t.input_schema.clone(),
                },
            })
            .collect()
    }

    /// 执行 MCP 工具调用
    pub async fn execute(
        &mut self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<String, String> {
        // 提取原始工具名（去掉 server 前缀）
        let original_name = tool_name.split("__").nth(1).unwrap_or(tool_name);

        let params = json!({
            "name": original_name,
            "arguments": arguments,
        });

        let result = self.request("tools/call", Some(params)).await?;

        // 解析工具调用结果
        if let Some(content) = result.get("content") {
            if let Some(items) = content.as_array() {
                let texts: Vec<String> = items
                    .iter()
                    .filter_map(|item| {
                        if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                            item.get("text")
                                .and_then(|t| t.as_str())
                                .map(|s| s.to_string())
                        } else {
                            None
                        }
                    })
                    .collect();
                Ok(texts.join("\n"))
            } else {
                Ok(content.to_string())
            }
        } else {
            Ok(result.to_string())
        }
    }

    // ===================================================================
    // 内部方法
    // ===================================================================

    /// 发送 JSON-RPC 请求并等待响应
    async fn request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        self.request_id += 1;
        let id = self.request_id;

        let req = McpRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        };

        let req_json = serde_json::to_string(&req).map_err(|e| format!("序列化请求失败: {}", e))?;

        debug!("MCP -> {}: {}", self.name, req_json);

        // 发送（带长度前缀，LSP 风格）
        let msg = format!("Content-Length: {}\r\n\r\n{}", req_json.len(), req_json);
        self.stdin
            .write_all(msg.as_bytes())
            .await
            .map_err(|e| format!("发送请求失败: {}", e))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("刷新失败: {}", e))?;

        // 读取响应头
        let mut header = String::new();
        loop {
            header.clear();
            match self.stdout.read_line(&mut header).await {
                Ok(0) => return Err("MCP Server 断开连接".to_string()),
                Ok(_) => {
                    if header.trim().is_empty() {
                        break;
                    }
                }
                Err(e) => return Err(format!("读取响应头失败: {}", e)),
            }
        }

        // 读取响应体
        let mut body = String::new();
        match self.stdout.read_line(&mut body).await {
            Ok(0) => return Err("MCP Server 断开连接".to_string()),
            Ok(_) => {}
            Err(e) => return Err(format!("读取响应体失败: {}", e)),
        }

        debug!("MCP <- {}: {}", self.name, body.trim());

        let resp: McpResponse =
            serde_json::from_str(&body).map_err(|e| format!("解析响应失败: {}", e))?;

        if let Some(err) = resp.error {
            return Err(format!("MCP 错误 [{}]: {}", err.code, err.message));
        }

        resp.result.ok_or("空响应".to_string())
    }

    /// 初始化连接
    async fn initialize(&mut self) -> Result<(), String> {
        let params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "raven", "version": "0.1.0" },
        });

        self.request("initialize", Some(params)).await?;

        // 发送 initialized 通知
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        });
        let notif_json = serde_json::to_string(&notif).unwrap();
        let msg = format!("Content-Length: {}\r\n\r\n{}", notif_json.len(), notif_json);
        let _ = self.stdin.write_all(msg.as_bytes()).await;
        let _ = self.stdin.flush().await;

        Ok(())
    }

    /// 发现工具列表
    async fn discover_tools(&mut self) -> Result<(), String> {
        let result = self.request("tools/list", None).await?;

        if let Some(tools_arr) = result.get("tools").and_then(|t| t.as_array()) {
            self.tools = tools_arr
                .iter()
                .filter_map(|t| serde_json::from_value::<McpTool>(t.clone()).ok())
                .collect();
        }

        Ok(())
    }
}

/// MCP 工具管理器（管理多个 MCP Server 连接）
pub struct McpManager {
    connections: Vec<McpConnection>,
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            connections: Vec::new(),
        }
    }

    /// 添加 MCP Server 连接
    pub async fn add_server(&mut self, config: &McpServerConfig) -> Result<(), String> {
        match McpConnection::connect(config).await {
            Ok(conn) => {
                self.connections.push(conn);
                Ok(())
            }
            Err(e) => {
                warn!("MCP Server '{}' 连接失败: {}", config.name, e);
                Err(e)
            }
        }
    }

    /// 获取所有 MCP 工具的 ToolSchema
    pub fn all_tool_schemas(&self) -> Vec<ToolSchema> {
        self.connections
            .iter()
            .flat_map(|c| c.to_tool_schemas())
            .collect()
    }

    /// 执行 MCP 工具调用
    pub async fn execute(
        &mut self,
        prefixed_name: &str,
        arguments: serde_json::Value,
    ) -> Result<String, String> {
        // 解析 server__tool 格式的名称
        let parts: Vec<&str> = prefixed_name.split("__").collect();
        if parts.len() != 2 {
            return Err(format!(
                "无效的 MCP 工具名称: {} (应为 server__tool 格式)",
                prefixed_name
            ));
        }

        let server_name = parts[0];

        for conn in &mut self.connections {
            if conn.name == server_name {
                return conn.execute(prefixed_name, arguments).await;
            }
        }

        Err(format!("未找到 MCP Server: {}", server_name))
    }

    /// 检查工具是否是 MCP 工具
    pub fn is_mcp_tool(&self, name: &str) -> bool {
        name.contains("__")
    }

    /// 获取连接数量
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// 获取所有 MCP 工具名
    pub fn tool_names(&self) -> Vec<String> {
        self.connections
            .iter()
            .flat_map(|c| c.tools().iter().map(|t| format!("{}__{}", c.name, t.name)))
            .collect()
    }
}
