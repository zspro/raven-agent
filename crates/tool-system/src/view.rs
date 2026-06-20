//! ViewTool - Claude Code 风格的代码查看工具
//!
//! 核心设计：统一的查看原语，可以查看文件内容和目录结构。
//! Claude Code 用 ViewTool 替代了传统的 file_read + list_dir，
//! 提供更丰富的查看体验（行号、滚动、目录树）。

use super::Tool;
use raven_types::{FunctionSchema, ToolSchema};
use async_trait::async_trait;
use serde_json::json;
use std::path::Path;

pub struct ViewTool;

#[async_trait]
impl Tool for ViewTool {
    fn name(&self) -> &str { "view" }
    fn description(&self) -> &str { "查看文件内容或目录结构（带行号）" }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "view".to_string(),
                description: "查看文件或目录。\n\n- 文件：返回带行号的内容，支持行范围（offset/limit）\n- 目录：返回树形结构，标记文件/目录/大小\n\n这是查看代码的主要工具，比 file_read 提供更多信息。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "文件或目录路径"
                        },
                        "offset": {
                            "type": "integer",
                            "description": "起始行号（从1开始，仅文件，可选）"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "最大行数（默认50，仅文件，可选）"
                        }
                    },
                    "required": ["path"]
                }),
            },
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path_str = args["path"].as_str().ok_or("缺少 path 参数")?;
        let path = Path::new(path_str);

        // 检查是文件还是目录
        let metadata = tokio::fs::metadata(path).await
            .map_err(|e| format!("无法访问 '{}': {}", path_str, e))?;

        if metadata.is_dir() {
            view_directory(path).await
        } else {
            let offset = args["offset"].as_u64().map(|n| n as usize);
            let limit = args["limit"].as_u64().map(|n| n as usize);
            view_file(path, offset, limit).await
        }
    }
}

/// 查看文件内容（带行号）
async fn view_file(path: &Path, offset: Option<usize>, limit: Option<usize>) -> Result<String, String> {
    let content = tokio::fs::read_to_string(path).await
        .map_err(|e| format!("读取文件失败: {}", e))?;

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    // 默认参数
    let offset = offset.unwrap_or(1); // 默认从第1行开始
    let limit = limit.unwrap_or(50);  // 默认显示50行

    // 转换为 0-indexed
    let start = offset.saturating_sub(1).min(total_lines);
    let end = (start + limit).min(total_lines);

    // 构建输出行
    let mut output_lines = Vec::new();

    // 文件头
    let size = format_size(std::fs::metadata(path).map(|m| m.len()).unwrap_or(0));
    output_lines.push(format!("📄 {} ({} 行, {})", path.display(), total_lines, size));
    output_lines.push("─".repeat(60));

    // 如果跳过了开头，显示省略号
    if start > 0 {
        output_lines.push(format!("    ... (前 {} 行省略，使用 offset={} 查看)", start, start + 1));
    }

    // 内容行
    for (i, line) in lines[start..end].iter().enumerate() {
        let line_no = start + i + 1;
        output_lines.push(format!("{:4} │ {}", line_no, line));
    }

    // 如果还有更多行，显示省略号
    if end < total_lines {
        output_lines.push(format!(
            "    ... (后 {} 行省略，使用 offset={} 查看)",
            total_lines - end,
            end + 1
        ));
    }

    output_lines.push("─".repeat(60));
    output_lines.push(format!(
        "显示 {}-{} / {} 行 | 使用 offset={} 查看下一页",
        start + 1, end, total_lines, end + 1
    ));

    Ok(output_lines.join("\n"))
}

/// 查看目录（树形结构）
async fn view_directory(path: &Path) -> Result<String, String> {
    let mut entries = tokio::fs::read_dir(path).await
        .map_err(|e| format!("读取目录失败: {}", e))?;

    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut total_size: u64 = 0;

    while let Some(entry) = entries.next_entry().await.map_err(|e| format!("读取条目失败: {}", e))? {
        let name = entry.file_name().to_string_lossy().to_string();
        let meta = entry.metadata().await.ok();

        if let Some(m) = meta {
            if m.is_dir() {
                // 递归统计子目录中的文件数
                let count = count_files(&entry.path()).await.unwrap_or(0);
                dirs.push((name, count));
            } else {
                let size = m.len();
                total_size += size;
                files.push((name, size));
            }
        }
    }

    // 排序
    dirs.sort_by(|a, b| a.0.cmp(&b.0));
    files.sort_by(|a, b| a.0.cmp(&b.0));

    // 构建输出
    let mut lines = Vec::new();

    lines.push(format!("📁 {} ({} 子目录, {} 文件, 共 {})",
        path.display(), dirs.len(), files.len(), format_size(total_size)));
    lines.push("─".repeat(50));

    // 子目录
    if !dirs.is_empty() {
        lines.push(format!("  📂 子目录 ({}个):", dirs.len()));
        for (name, count) in &dirs {
            let indent = "    ";
            lines.push(format!("{}├── {}/ ({} 文件)", indent, name, count));
        }
        lines.push(String::new());
    }

    // 文件
    if !files.is_empty() {
        lines.push(format!("  📄 文件 ({}个):", files.len()));
        for (name, size) in &files {
            let indent = "    ";
            // 根据扩展名添加图标
            let icon = get_file_icon(&name);
            lines.push(format!("{}├── {} {} ({})", indent, icon, name, format_size(*size)));
        }
    }

    if dirs.is_empty() && files.is_empty() {
        lines.push("  (空目录)".to_string());
    }

    lines.push("─".repeat(50));
    lines.push("💡 提示: 使用 view(path=\"文件名\") 查看文件内容".to_string());

    Ok(lines.join("\n"))
}

/// 递归统计目录中的文件数
async fn count_files(path: &Path) -> Result<usize, std::io::Error> {
    let mut count = 0;
    let mut entries = tokio::fs::read_dir(path).await?;
    while let Some(entry) = entries.next_entry().await? {
        let meta = entry.metadata().await?;
        if meta.is_file() {
            count += 1;
        }
    }
    Ok(count)
}

/// 根据扩展名返回文件图标
fn get_file_icon(name: &str) -> &'static str {
    match name.rsplit('.').next() {
        Some("rs") => "🦀",
        Some("go") => "🔵",
        Some("js" | "ts" | "jsx" | "tsx") => "🟨",
        Some("py") => "🐍",
        Some("java") => "☕",
        Some("c" | "cpp" | "h" | "hpp") => "🔷",
        Some("md" | "txt" | "rst") => "📝",
        Some("json" | "yaml" | "yml" | "toml") => "⚙️",
        Some("sh" | "bash" | "zsh") => "🔧",
        Some("html" | "css") => "🌐",
        Some("dockerfile") => "🐳",
        _ => "📄",
    }
}

/// 格式化文件大小
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
