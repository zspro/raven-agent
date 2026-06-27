//! FetchUrlTool - 获取网页内容

use super::html::extract_text_from_html;
use crate::Tool;
use async_trait::async_trait;
use raven_types::{FunctionSchema, ToolSchema};
use serde_json::json;

pub struct FetchUrlTool;

#[async_trait]
impl Tool for FetchUrlTool {
    fn name(&self) -> &str {
        "fetch_url"
    }
    fn description(&self) -> &str {
        "获取网页的文本内容"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "fetch_url".to_string(),
                description: "抓取指定 URL 的网页并提取正文文本，自动去除 HTML 标签，支持 HTML 与 Markdown 页面。\n\n何时使用：已有具体 URL（用户给出的，或 web_search 返回的链接），需要阅读其完整内容时。URL 必须带协议（https://）。超长页面用 max_length 截断。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "要抓取的完整 URL，必须包含协议（如 https://example.com）。"
                        },
                        "max_length": {
                            "type": "integer",
                            "description": "提取正文的最大字符数（可选，默认 5000）。"
                        }
                    },
                    "required": ["url"]
                }),
            },
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let url = args["url"].as_str().ok_or("缺少 url 参数")?;
        let max_len = args["max_length"].as_u64().unwrap_or(5000) as usize;

        // URL 安全检查
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("URL 必须以 http:// 或 https:// 开头".to_string());
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .user_agent("Mozilla/5.0 (compatible; AgentFramework/0.1)")
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败: {}", e))?;

        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("请求失败: {}", e))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(format!("HTTP 错误: {}", status));
        }

        let body = resp
            .text()
            .await
            .map_err(|e| format!("读取响应失败: {}", e))?;

        // 提取文本内容
        let text = extract_text_from_html(&body);
        // 按 Unicode 字符（而非字节）截断，避免在多字节字符（如中文）中间
        // 切断导致 panic。max_len 语义为「最多保留的字符数」。
        let char_count = text.chars().count();
        let truncated = if char_count > max_len {
            let head: String = text.chars().take(max_len).collect();
            format!("{}...\n\n[已截断，共 {} 字符]", head, char_count)
        } else {
            text
        };

        Ok(format!("URL: {}\n\n{}", url, truncated))
    }
}
