//! 内置工具实现

use super::Tool;
use raven_types::{FunctionSchema, ToolSchema};
use async_trait::async_trait;
use serde_json::json;
use std::path::Path;

// =============================================================================
// FileReadTool - 文件读取
// =============================================================================

pub struct FileReadTool;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str { "file_read" }
    fn description(&self) -> &str { "读取文件内容，支持行号范围和行数限制" }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "file_read".to_string(),
                description: "读取指定文件的内容。支持文本文件。如果文件太大，会自动截断。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "文件路径（相对或绝对）" },
                        "offset": { "type": "integer", "description": "起始行号（可选，从0开始）" },
                        "limit": { "type": "integer", "description": "最大读取行数（可选，默认100）" }
                    },
                    "required": ["path"]
                }),
            },
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path = args["path"].as_str().ok_or("缺少 path 参数")?;
        let path = Path::new(path);

        let content = tokio::fs::read_to_string(path).await
            .map_err(|e| format!("读取失败: {}", e))?;

        let offset = args["offset"].as_u64().unwrap_or(0) as usize;
        let limit = args["limit"].as_u64().unwrap_or(100) as usize;

        let lines: Vec<&str> = content.lines().collect();
        let start = offset.min(lines.len());
        let end = (offset + limit).min(lines.len());

        let selected: Vec<String> = lines[start..end].iter()
            .enumerate()
            .map(|(i, line)| format!("{:4} | {}", start + i + 1, line))
            .collect();

        Ok(format!("文件: {} (共{}行)\n{}",
            path.display(),
            lines.len(),
            selected.join("\n")
        ))
    }
}

// =============================================================================
// FileWriteTool - 文件写入
// =============================================================================

pub struct FileWriteTool;

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str { "file_write" }
    fn description(&self) -> &str { "写入或追加文件内容" }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "file_write".to_string(),
                description: "写入内容到文件。如果文件存在会覆盖，除非 append=true。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "文件路径" },
                        "content": { "type": "string", "description": "要写入的内容" },
                        "append": { "type": "boolean", "description": "是否追加模式（默认false）" }
                    },
                    "required": ["path", "content"]
                }),
            },
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path = args["path"].as_str().ok_or("缺少 path 参数")?;
        let content = args["content"].as_str().ok_or("缺少 content 参数")?;
        let append = args["append"].as_bool().unwrap_or(false);

        if append {
            tokio::fs::write(path, content).await.map_err(|e| format!("写入失败: {}", e))?;
        } else {
            tokio::fs::write(path, content).await.map_err(|e| format!("写入失败: {}", e))?;
        }

        let mode = if append { "追加" } else { "覆盖" };
        Ok(format!("已{}写入 {} ({} 字符)", mode, path, content.len()))
    }
}

// =============================================================================
// ShellTool - Shell 执行
// =============================================================================

pub struct ShellTool {
    /// 允许执行的命令白名单。为空时回退到内置安全默认集。
    pub allowed: Vec<String>,
    /// 命令超时（秒）
    pub timeout: u64,
}

impl Default for ShellTool {
    fn default() -> Self {
        Self {
            allowed: default_shell_allowed(),
            timeout: 30,
        }
    }
}

impl ShellTool {
    /// 用配置构造（白名单为空则使用内置默认集）
    pub fn with_config(allowed: Vec<String>, timeout: u64) -> Self {
        Self {
            allowed: if allowed.is_empty() {
                default_shell_allowed()
            } else {
                allowed
            },
            timeout: if timeout == 0 { 30 } else { timeout },
        }
    }
}

/// 内置安全默认命令集（按平台区分）
fn default_shell_allowed() -> Vec<String> {
    #[cfg(windows)]
    let cmds = [
        "dir", "type", "findstr", "where", "git", "go", "npm", "node",
        "echo", "cd", "more", "tree", "curl", "python", "cargo",
    ];
    #[cfg(not(windows))]
    let cmds = [
        "ls", "cat", "grep", "find", "git", "go", "npm", "node", "echo",
        "pwd", "head", "tail", "wc", "mkdir", "touch", "cp", "mv", "curl",
    ];
    cmds.iter().map(|s| s.to_string()).collect()
}

/// 危险命令黑名单：即使在白名单内也一律拒绝执行。
/// 这是不可绕过的安全底线，防止破坏性操作。
fn is_dangerous_command(command: &str) -> Option<&'static str> {
    let lower = command.to_lowercase();
    // 破坏性删除 / 磁盘操作 / 关机等
    const PATTERNS: &[(&str, &str)] = &[
        ("rm -rf", "递归强制删除"),
        ("rm -fr", "递归强制删除"),
        ("rmdir /s", "递归删除目录"),
        ("del /f", "强制删除"),
        ("del /s", "递归删除"),
        ("format ", "格式化磁盘"),
        ("mkfs", "格式化文件系统"),
        ("dd ", "磁盘块写入"),
        (":(){", "fork 炸弹"),
        ("shutdown", "关机/重启"),
        ("reboot", "重启"),
        ("> /dev/sda", "覆写磁盘设备"),
        ("chmod -r 777", "递归放开权限"),
        ("mkfs.", "格式化文件系统"),
    ];
    for (pat, reason) in PATTERNS {
        if lower.contains(pat) {
            return Some(reason);
        }
    }
    None
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str { "shell" }
    fn description(&self) -> &str { "执行 Shell 命令（有安全限制）" }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "shell".to_string(),
                description: "执行 Shell 命令。支持常用命令如 ls、cat、grep、find、git 等。超时 30 秒。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "要执行的命令" },
                        "timeout": { "type": "integer", "description": "超时秒数（默认30）" }
                    },
                    "required": ["command"]
                }),
            },
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let command = args["command"].as_str().ok_or("缺少 command 参数")?;
        let timeout = args["timeout"].as_u64().unwrap_or(self.timeout);

        // 危险命令黑名单：不可绕过的安全底线
        if let Some(reason) = is_dangerous_command(command) {
            return Err(format!(
                "已拒绝危险命令（{}）: '{}'\n此类破坏性操作被安全策略禁止，如确需执行请在终端手动运行。",
                reason, command
            ));
        }

        // 白名单检查：取首个 token 作为命令名
        let cmd = command.split_whitespace().next().unwrap_or("");
        if !self.allowed.iter().any(|c| c == cmd) {
            return Err(format!(
                "命令 '{}' 不在允许列表中。\n可在配置 'tools.shell.allowed' 中添加，当前允许: {}",
                cmd,
                self.allowed.join(", ")
            ));
        }

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(timeout),
            {
                // 跨平台 shell：Windows 用 cmd /C，其余用 sh -c
                #[cfg(windows)]
                let mut c = {
                    let mut c = tokio::process::Command::new("cmd");
                    c.arg("/C");
                    c
                };
                #[cfg(not(windows))]
                let mut c = {
                    let mut c = tokio::process::Command::new("sh");
                    c.arg("-c");
                    c
                };
                c.arg(command).output()
            },
        ).await
            .map_err(|_| "命令超时")?
            .map_err(|e| format!("执行失败: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            return Err(format!("exit code: {}\n{}", output.status, stderr));
        }

        if !stderr.is_empty() {
            Ok(format!("{}\n[stderr]: {}", stdout, stderr))
        } else {
            Ok(stdout.to_string())
        }
    }
}

// =============================================================================
// SearchTool - 文件搜索
// =============================================================================

pub struct SearchTool;

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &str { "search" }
    fn description(&self) -> &str { "在文件中搜索内容" }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "search".to_string(),
                description: "在指定目录的文件中搜索匹配的内容。支持正则表达式。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "搜索模式" },
                        "path": { "type": "string", "description": "搜索目录（默认当前目录）" },
                        "ext": { "type": "string", "description": "文件扩展名过滤（如.go,.js，可选）" },
                        "max_results": { "type": "integer", "description": "最大结果数（默认20）" }
                    },
                    "required": ["pattern"]
                }),
            },
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let pattern = args["pattern"].as_str().ok_or("缺少 pattern 参数")?;
        let path = args["path"].as_str().unwrap_or(".").to_string();
        let max_results = args["max_results"].as_u64().unwrap_or(20) as usize;
        let ext = args["ext"].as_str().map(|e| e.trim_start_matches('.').to_string());

        // 纯 Rust 实现，跨平台、不依赖外部 grep。大小写不敏感。
        let re = regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .map_err(|e| format!("无效的正则: {}", e))?;

        let matches = tokio::task::spawn_blocking(move || {
            let mut out: Vec<String> = Vec::new();
            let mut stack = vec![std::path::PathBuf::from(&path)];
            while let Some(dir) = stack.pop() {
                if out.len() >= max_results {
                    break;
                }
                let rd = match std::fs::read_dir(&dir) {
                    Ok(rd) => rd,
                    Err(_) => continue,
                };
                for entry in rd.flatten() {
                    let p = entry.path();
                    let ft = match entry.file_type() {
                        Ok(ft) => ft,
                        Err(_) => continue,
                    };
                    // 跳过隐藏目录与常见噪音目录
                    let name = entry.file_name().to_string_lossy().to_string();
                    if ft.is_dir() {
                        if name.starts_with('.')
                            || name == "target"
                            || name == "node_modules"
                        {
                            continue;
                        }
                        stack.push(p);
                        continue;
                    }
                    // 扩展名过滤
                    if let Some(want) = &ext {
                        let matches_ext = p
                            .extension()
                            .map(|e| e.to_string_lossy() == want.as_str())
                            .unwrap_or(false);
                        if !matches_ext {
                            continue;
                        }
                    }
                    // 读取文本文件（二进制读取失败会得到非 UTF-8，lossy 处理后仍可匹配）
                    let content = match std::fs::read(&p) {
                        Ok(bytes) => {
                            // 简单二进制判定：包含 NUL 字节则跳过
                            if bytes.contains(&0) {
                                continue;
                            }
                            String::from_utf8_lossy(&bytes).into_owned()
                        }
                        Err(_) => continue,
                    };
                    for (lineno, line) in content.lines().enumerate() {
                        if re.is_match(line) {
                            out.push(format!("{}:{}:{}", p.display(), lineno + 1, line.trim()));
                            if out.len() >= max_results {
                                break;
                            }
                        }
                    }
                }
            }
            out
        })
        .await
        .map_err(|e| format!("搜索任务失败: {}", e))?;

        if matches.is_empty() {
            Ok("未找到匹配".to_string())
        } else {
            Ok(format!("找到 {} 个匹配:\n{}", matches.len(), matches.join("\n")))
        }
    }
}

// =============================================================================
// ListDirTool - 目录列表
// =============================================================================

pub struct ListDirTool;

#[async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &str { "list_dir" }
    fn description(&self) -> &str { "列出目录内容" }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "list_dir".to_string(),
                description: "列出指定目录中的文件和子目录。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "目录路径（默认当前目录）" }
                    }
                }),
            },
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path = args["path"].as_str().unwrap_or(".");

        let mut entries = tokio::fs::read_dir(path).await
            .map_err(|e| format!("读取目录失败: {}", e))?;

        let mut dirs = Vec::new();
        let mut files = Vec::new();

        while let Some(entry) = entries.next_entry().await.map_err(|e| format!("读取条目失败: {}", e))? {
            let name = entry.file_name().to_string_lossy().to_string();
            let metadata = entry.metadata().await.ok();

            if let Some(meta) = metadata {
                if meta.is_dir() {
                    dirs.push(format!("{}/", name));
                } else {
                    let size = format_size(meta.len());
                    files.push(format!("{} ({})", name, size));
                }
            }
        }

        let dirs_str = if dirs.is_empty() { "(无)".to_string() } else { dirs.join("\n  ") };
        let files_str = if files.is_empty() { "(无)".to_string() } else { files.join("\n  ") };
        Ok(format!("目录: {}\n\n子目录 ({}):\n  {}\n\n文件 ({}):\n  {}",
            path, dirs.len(), dirs_str, files.len(), files_str,
        ))
    }
}

// =============================================================================
// GitTool - Git 操作
// =============================================================================

pub struct GitTool;

#[async_trait]
impl Tool for GitTool {
    fn name(&self) -> &str { "git" }
    fn description(&self) -> &str { "执行 Git 命令" }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "git".to_string(),
                description: "执行 Git 命令。支持 status、log、diff、branch 等只读操作，以及 commit。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Git 子命令（如 status, log, diff, branch, commit）" },
                        "args": { "type": "string", "description": "额外参数" }
                    },
                    "required": ["command"]
                }),
            },
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let command = args["command"].as_str().ok_or("缺少 command 参数")?;
        let extra = args["args"].as_str().unwrap_or("");

        // 安全检查
        let allowed = ["status", "log", "diff", "branch", "show", "blame", "remote", "config", "commit", "add", "init"];
        if !allowed.contains(&command) {
            return Err(format!("不支持的 git 命令: {}", command));
        }

        let mut cmd = tokio::process::Command::new("git");
        cmd.arg(command);
        if !extra.is_empty() {
            cmd.args(extra.split_whitespace());
        }

        let output = cmd.output().await
            .map_err(|e| format!("git 执行失败: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            return Err(format!("{}", stderr));
        }

        Ok(if stderr.is_empty() { stdout.to_string() } else { format!("{}\n[stderr]: {}", stdout, stderr) })
    }
}

// =============================================================================
// 工具函数
// =============================================================================

fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;

    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }

    format!("{:.1} {}", size, UNITS[unit_idx])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dangerous_command_detected() {
        assert!(is_dangerous_command("rm -rf /").is_some());
        assert!(is_dangerous_command("RM -RF ~/data").is_some()); // 大小写不敏感
        assert!(is_dangerous_command("sudo shutdown now").is_some());
        assert!(is_dangerous_command("mkfs.ext4 /dev/sdb").is_some());
        assert!(is_dangerous_command("dd if=/dev/zero of=/dev/sda").is_some());
    }

    #[test]
    fn test_safe_command_not_flagged() {
        assert!(is_dangerous_command("ls -la").is_none());
        assert!(is_dangerous_command("git status").is_none());
        assert!(is_dangerous_command("cat file.txt").is_none());
    }

    #[test]
    fn test_shell_with_config_empty_falls_back() {
        let tool = ShellTool::with_config(Vec::new(), 0);
        assert!(!tool.allowed.is_empty(), "空白名单应回退到默认集");
        assert_eq!(tool.timeout, 30, "0 超时应回退到 30");
    }

    #[test]
    fn test_shell_with_config_custom() {
        let tool = ShellTool::with_config(vec!["ls".to_string(), "echo".to_string()], 60);
        assert_eq!(tool.allowed.len(), 2);
        assert_eq!(tool.timeout, 60);
    }

    #[tokio::test]
    async fn test_shell_rejects_dangerous() {
        let tool = ShellTool::default();
        let res = tool
            .execute(serde_json::json!({"command": "rm -rf /tmp/x"}))
            .await;
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("危险命令"));
    }

    #[tokio::test]
    async fn test_shell_rejects_not_in_whitelist() {
        let tool = ShellTool::with_config(vec!["ls".to_string()], 30);
        let res = tool
            .execute(serde_json::json!({"command": "wget http://x"}))
            .await;
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("不在允许列表"));
    }
}
