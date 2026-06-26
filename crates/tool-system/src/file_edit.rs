//! FileEditTool - Claude Code 风格的 diff 编辑
//!
//! 核心设计：通过 old_string / new_string 的 diff 格式精确编辑文件。
//! 比全量覆写更安全，因为必须匹配现有内容才能修改。
//!
//! Claude Code 的 FileEditTool 是其编码能力的核心原语。

use super::Tool;
use async_trait::async_trait;
use raven_types::{FunctionSchema, ToolSchema};
use serde_json::json;

pub struct FileEditTool;

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str {
        "file_edit"
    }
    fn description(&self) -> &str {
        "精确编辑文件内容（diff 模式）"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "file_edit".to_string(),
                description: "精确编辑文件。提供 old_string（文件中现有的内容）和 new_string（替换后的内容）。\n\n规则:\n1. old_string 必须在文件中精确匹配（包括缩进）\n2. 如果 old_string 为空，new_string 将追加到文件末尾\n3. 修改后会返回修改位置的上下文（修改前后3行）\n4. 如需多次修改同一文件，需多次调用".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "文件路径（相对或绝对）"
                        },
                        "old_string": {
                            "type": "string",
                            "description": "文件中要替换的现有内容。必须精确匹配（包括缩进和空格）。如果为空字符串则追加到文件末尾。"
                        },
                        "new_string": {
                            "type": "string",
                            "description": "替换后的新内容。"
                        }
                    },
                    "required": ["path", "old_string", "new_string"]
                }),
            },
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path = args["path"].as_str().ok_or("缺少 path 参数")?;
        let old_str = args["old_string"].as_str().unwrap_or("");
        let new_str = args["new_string"].as_str().unwrap_or("");

        // 读取文件内容
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| format!("读取文件失败: {} (文件可能不存在)", e))?;

        if old_str.is_empty() {
            // 追加模式
            let had_newline = content.ends_with('\n') || content.is_empty();
            let prefix = if had_newline { "" } else { "\n" };
            let updated = format!("{}{}{}", content, prefix, new_str);

            tokio::fs::write(path, &updated)
                .await
                .map_err(|e| format!("写入失败: {}", e))?;

            // 返回追加位置的上下文
            let lines: Vec<&str> = updated.lines().collect();
            let append_line = lines.len();
            let context = get_context(&lines, append_line.saturating_sub(1), 3);

            return Ok(format!(
                "已追加到 {} (第{}行)\n\n上下文:\n{}",
                path, append_line, context
            ));
        }

        // 查找 old_string 的位置
        let occurrences = content.match_indices(old_str).collect::<Vec<_>>();

        if occurrences.is_empty() {
            return Err(format!(
                "在文件中找不到匹配的内容。\n\n你要查找的:\n```\n{}\n```\n\n提示:\n1. 确保 old_string 与文件内容完全匹配（包括缩进）\n2. 使用 file_read 工具先查看文件内容\n3. 如果要添加全新内容，将 old_string 设为空字符串",
                old_str
            ));
        }

        if occurrences.len() > 1 {
            // 多次出现，需要更精确的匹配
            let positions: Vec<usize> = occurrences.iter().map(|(i, _)| *i).collect();
            return Err(format!(
                "找到 {} 处匹配，无法确定要替换哪一处。\n匹配位置（字符偏移）: {:?}\n\n请提供更精确的 old_string（包含更多上下文）来唯一确定要修改的位置。",
                occurrences.len(), positions
            ));
        }

        // 精确替换
        let (match_pos, _) = occurrences[0];
        let updated = format!(
            "{}{}{}",
            &content[..match_pos],
            new_str,
            &content[match_pos + old_str.len()..]
        );

        tokio::fs::write(path, &updated)
            .await
            .map_err(|e| format!("写入失败: {}", e))?;

        // 计算修改的行号
        let lines_before: Vec<&str> = content[..match_pos].lines().collect();
        let edit_line = lines_before.len() + 1; // 1-indexed

        let updated_lines: Vec<&str> = updated.lines().collect();
        let context = get_context(&updated_lines, edit_line.saturating_sub(1), 3);

        // 统计修改
        let old_lines = old_str.lines().count();
        let new_lines = new_str.lines().count();
        let diff = new_lines as isize - old_lines as isize;
        let diff_str = if diff > 0 {
            format!("(+{} 行)", diff)
        } else if diff < 0 {
            format!("({} 行)", diff)
        } else {
            "(行数不变)".to_string()
        };

        Ok(format!(
            "已修改 {} (第{}行) {}\n\n修改后上下文:\n{}",
            path, edit_line, diff_str, context
        ))
    }
}

/// 获取指定行号的上下文（前后 n 行）
fn get_context(lines: &[&str], line_idx: usize, context: usize) -> String {
    let start = line_idx.saturating_sub(context);
    let end = (line_idx + context + 1).min(lines.len());

    lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let line_no = start + i + 1;
            let marker = if line_no == line_idx + 1 {
                ">>> "
            } else {
                "    "
            };
            format!("{}{:4} | {}", marker, line_no, line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
