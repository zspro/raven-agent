//! 文件内容搜索工具：SearchTool

use crate::Tool;
use async_trait::async_trait;
use raven_types::{FunctionSchema, ToolSchema};
use serde_json::json;

pub struct SearchTool;

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &str {
        "search"
    }
    fn description(&self) -> &str {
        "在文件中搜索内容"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "search".to_string(),
                description: "在目录下递归搜索文件内容中匹配正则的行，返回 文件:行号:内容。跨平台纯 Rust 实现，大小写不敏感，自动跳过 .git/target/node_modules 等噪音目录。\n\n何时使用：在代码库中按关键字/正则定位定义、引用或字符串。\n何时不要用：已知确切路径、只想看某个文件时用 view。结果较多时用 ext 限定文件类型、用 max_results 收口。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "搜索用的正则表达式。" },
                        "path": { "type": "string", "description": "搜索起始目录（可选，默认当前目录）。" },
                        "ext": { "type": "string", "description": "按扩展名过滤（可选，如 .rs、.go；带不带点都可）。" },
                        "max_results": { "type": "integer", "description": "最多返回的匹配数（可选，默认 20）。" }
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
        let ext = args["ext"]
            .as_str()
            .map(|e| e.trim_start_matches('.').to_string());

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
                        if name.starts_with('.') || name == "target" || name == "node_modules" {
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
            Ok(format!(
                "找到 {} 个匹配:\n{}",
                matches.len(),
                matches.join("\n")
            ))
        }
    }
}
