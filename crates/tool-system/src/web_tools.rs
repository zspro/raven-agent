//! Web 工具 - web_search + fetch_url
//!
//! 让 Agent 可以搜索网页和获取页面内容，获取实时信息。

use super::Tool;
use raven_types::{FunctionSchema, ToolSchema};
use async_trait::async_trait;
use serde_json::json;

// =============================================================================
// WebSearchTool - 网页搜索
// =============================================================================

pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str { "web_search" }
    fn description(&self) -> &str { "搜索网页内容" }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "web_search".to_string(),
                description: "在搜索引擎中搜索指定关键词。返回搜索结果的标题、链接和摘要。\n\n注意：需要有网络连接，且搜索服务可能有限制。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "搜索关键词"
                        },
                        "num_results": {
                            "type": "integer",
                            "description": "返回结果数量（默认5，最大10）"
                        }
                    },
                    "required": ["query"]
                }),
            },
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let query = args["query"].as_str().ok_or("缺少 query 参数")?;
        let num = args["num_results"].as_u64().unwrap_or(5).min(10) as usize;

        // 使用 DuckDuckGo HTML 搜索（无需 API Key）
        let url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            urlencoding::encode(query)
        );

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent("Mozilla/5.0 (compatible; AgentFramework/0.1)")
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败: {}", e))?;

        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("搜索请求失败: {}", e))?;

        let body = resp
            .text()
            .await
            .map_err(|e| format!("读取响应失败: {}", e))?;

        // 解析搜索结果
        let results = parse_duckduckgo_results(&body, num);

        if results.is_empty() {
            Ok(format!("搜索 '{}' 未找到结果。\n\n可能原因:\n1. 网络连接问题\n2. 搜索服务暂时不可用\n3. 关键词过于具体", query))
        } else {
            let mut output = format!("搜索 '{}':\n\n", query);
            for (i, (title, link, snippet)) in results.iter().enumerate() {
                output.push_str(&format!(
                    "{}. {}\n   {}\n   {}\n\n",
                    i + 1,
                    title,
                    link,
                    snippet
                ));
            }
            Ok(output)
        }
    }
}

// =============================================================================
// FetchUrlTool - 获取网页内容
// =============================================================================

pub struct FetchUrlTool;

#[async_trait]
impl Tool for FetchUrlTool {
    fn name(&self) -> &str { "fetch_url" }
    fn description(&self) -> &str { "获取网页的文本内容" }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "fetch_url".to_string(),
                description: "获取指定 URL 的网页内容。自动提取正文文本，去除 HTML 标签。支持 HTML 和 Markdown 页面。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "要获取的 URL"
                        },
                        "max_length": {
                            "type": "integer",
                            "description": "最大字符数（默认5000）"
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
        let truncated = if text.len() > max_len {
            format!("{}...\n\n[已截断，共 {} 字符]", &text[..max_len], text.len())
        } else {
            text
        };

        Ok(format!("URL: {}\n\n{}", url, truncated))
    }
}

// =============================================================================
// HTML 解析（简化版）
// =============================================================================

/// 解析 DuckDuckGo HTML 结果
fn parse_duckduckgo_results(html: &str, max: usize) -> Vec<(String, String, String)> {
    let mut results = Vec::new();

    // 简单的正则风格解析
    let re_web_result = regex::Regex::new(
        r#"<a[^>]*class=""result__a""[^>]*href=""([^""]*)""[^>]*>(.*?)</a>"#
    );
    let re_snippet = regex::Regex::new(
        r#"<a[^>]*class=""result__snippet""[^>]*>(.*?)</a>"#
    );

    if let (Ok(title_re), Ok(snippet_re)) = (re_web_result, re_snippet) {
        let titles: Vec<(String, String)> = title_re
            .captures_iter(html)
            .map(|c| {
                let link = c.get(1).map(|m| decode_html(m.as_str())).unwrap_or_default();
                let title = c.get(2).map(|m| strip_html(m.as_str())).unwrap_or_default();
                (title, link)
            })
            .collect();

        let snippets: Vec<String> = snippet_re
            .captures_iter(html)
            .map(|c| {
                c.get(1)
                    .map(|m| strip_html(m.as_str()))
                    .unwrap_or_default()
            })
            .collect();

        for (i, (title, link)) in titles.iter().enumerate().take(max) {
            let snippet = snippets.get(i).cloned().unwrap_or_default();
            results.push((title.clone(), link.clone(), snippet));
        }
    }

    results
}

/// 从 HTML 提取文本
fn extract_text_from_html(html: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    let mut in_script_or_style = false;
    let mut tag_buffer = String::new();
    let mut prev_char = ' ';

    for ch in html.chars() {
        if ch == '<' {
            in_tag = true;
            tag_buffer.clear();
        } else if ch == '>' {
            in_tag = false;
            // 检测 <script> 或 <style> 标签（不含斜杠是开始标签）
            let tag_lower = tag_buffer.trim().to_lowercase();
            if tag_lower.starts_with("script") || tag_lower.starts_with("style") {
                in_script_or_style = true;
            } else if tag_lower.starts_with("/script") || tag_lower.starts_with("/style") {
                in_script_or_style = false;
            }
            tag_buffer.clear();
        } else if in_tag {
            tag_buffer.push(ch);
        } else if !in_script_or_style {
            if ch.is_whitespace() {
                if !prev_char.is_whitespace() {
                    text.push(' ');
                }
            } else {
                text.push(ch);
            }
        }
        prev_char = ch;
    }

    text.trim().to_string()
}

/// 去除 HTML 标签
fn strip_html(html: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;

    for ch in html.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            text.push(ch);
        }
    }

    decode_html(&text)
}

/// 解码 HTML 实体（简化版）
fn decode_html(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}
