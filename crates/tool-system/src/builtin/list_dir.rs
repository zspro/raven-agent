//! 目录列表工具：ListDirTool

use crate::Tool;
use async_trait::async_trait;
use raven_types::{FunctionSchema, ToolSchema};
use serde_json::json;

pub struct ListDirTool;

#[async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }
    fn description(&self) -> &str {
        "列出目录内容"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "list_dir".to_string(),
                description: "列出某个目录下的直接子目录和文件（含文件大小）。跨平台，优先于 shell 的 ls/dir 使用。\n\n何时使用：快速了解某一层目录里有什么。要递归查看整棵树或同时看文件内容时，改用 view。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "目录路径（可选，默认当前目录）。" }
                    }
                }),
            },
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path = args["path"].as_str().unwrap_or(".");

        let mut entries = tokio::fs::read_dir(path)
            .await
            .map_err(|e| format!("读取目录失败: {}", e))?;

        let mut dirs = Vec::new();
        let mut files = Vec::new();

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| format!("读取条目失败: {}", e))?
        {
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

        let dirs_str = if dirs.is_empty() {
            "(无)".to_string()
        } else {
            dirs.join("\n  ")
        };
        let files_str = if files.is_empty() {
            "(无)".to_string()
        } else {
            files.join("\n  ")
        };
        Ok(format!(
            "目录: {}\n\n子目录 ({}):\n  {}\n\n文件 ({}):\n  {}",
            path,
            dirs.len(),
            dirs_str,
            files.len(),
            files_str,
        ))
    }
}

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
