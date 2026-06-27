//! 文件读写工具：FileReadTool / FileWriteTool

use crate::Tool;
use async_trait::async_trait;
use raven_types::{FunctionSchema, ToolSchema};
use serde_json::json;
use std::path::Path;

pub struct FileReadTool;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }
    fn description(&self) -> &str {
        "读取文件内容，支持行号范围和行数限制"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "file_read".to_string(),
                description: "读取单个文本文件的内容，返回带行号的文本。\n\n通常优先使用 view（功能更全，文件和目录都能看）；仅在只想快速取某文件纯内容时用 file_read。大文件用 offset/limit 分段读取，避免一次拉取过多。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "文件路径（相对或绝对）。" },
                        "offset": { "type": "integer", "description": "起始行号（可选，从 0 开始）。文件较大、想从中间读起时提供。" },
                        "limit": { "type": "integer", "description": "最多读取的行数（可选，默认 100）。" }
                    },
                    "required": ["path"]
                }),
            },
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path = args["path"].as_str().ok_or("缺少 path 参数")?;
        let path = Path::new(path);

        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| format!("读取失败: {}", e))?;

        let offset = args["offset"].as_u64().unwrap_or(0) as usize;
        let limit = args["limit"].as_u64().unwrap_or(100) as usize;

        let lines: Vec<&str> = content.lines().collect();
        let start = offset.min(lines.len());
        let end = offset.saturating_add(limit).min(lines.len());

        let selected: Vec<String> = lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:4} | {}", start + i + 1, line))
            .collect();

        Ok(format!(
            "文件: {} (共{}行)\n{}",
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
    fn name(&self) -> &str {
        "file_write"
    }
    fn description(&self) -> &str {
        "写入或追加文件内容"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "file_write".to_string(),
                description: "把内容整体写入文件，用于新建文件或完整重写。\n\n何时使用：创建全新文件，或文件需要从头重写时。\n何时不要用：修改已有文件的局部内容时改用 file_edit——它通过精确匹配替换，更安全、不会误删其余内容。默认会覆盖同名文件的全部内容，写之前先确认这是你想要的。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "目标文件路径（相对或绝对）。" },
                        "content": { "type": "string", "description": "要写入的完整内容。" },
                        "append": { "type": "boolean", "description": "true 表示追加到文件末尾，false（默认）表示覆盖整个文件。" }
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
            use tokio::io::AsyncWriteExt;
            let mut f = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await
                .map_err(|e| format!("打开文件失败: {}", e))?;
            f.write_all(content.as_bytes())
                .await
                .map_err(|e| format!("追加失败: {}", e))?;
        } else {
            tokio::fs::write(path, content)
                .await
                .map_err(|e| format!("写入失败: {}", e))?;
        }

        let mode = if append { "追加" } else { "覆盖" };
        Ok(format!(
            "已{}写入 {} ({} 字符)",
            mode,
            path,
            content.chars().count()
        ))
    }
}
