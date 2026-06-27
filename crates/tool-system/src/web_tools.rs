//! Web 工具 - web_search + fetch_url
//!
//! 让 Agent 可以搜索网页和获取页面内容，获取实时信息。

use super::Tool;
use async_trait::async_trait;
use raven_types::{FunctionSchema, ToolSchema};
use serde_json::json;
use std::sync::LazyLock;

/// 全局 HTTP 客户端（连接池复用，避免每次搜索重建 + TCP 握手开销）
static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .build()
        .expect("构建 HTTP 客户端失败")
});

// =============================================================================
// WebSearchTool - 网页搜索
// =============================================================================

pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }
    fn description(&self) -> &str {
        "搜索网页内容"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "web_search".to_string(),
                description: "用搜索引擎检索关键词，返回若干条结果的标题、链接和摘要。\n\n何时使用：需要最新信息、超出已有知识、或要核实事实时。摘要往往很短，确定某条结果有价值后，用 fetch_url 拉取该链接的正文细读。\n注意：依赖网络连接，搜索服务可能限流。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "搜索关键词，尽量简洁（几个词即可）。"
                        },
                        "num_results": {
                            "type": "integer",
                            "description": "返回结果数量（可选，默认 5，最大 10）。"
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
        let q = urlencoding::encode(query);

        // Bing 搜索
        let bing_url = format!("https://www.bing.com/search?q={}&setlang=zh-CN", q);
        let bing_resp = CLIENT
            .get(&bing_url)
            .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
            .send()
            .await;

        // DDG 并行竞速（与 Bing 返回值同时处理）
        let ddg_lite_url = format!("https://lite.duckduckgo.com/lite/?q={}", q);
        let ddg_html_url = format!("https://html.duckduckgo.com/html/?q={}", q);
        let (ddg_lite, ddg_html) = tokio::join!(
            try_ddg_async(&ddg_lite_url, "lite", num),
            try_ddg_async(&ddg_html_url, "html", num),
        );

        // 优先 Bing
        if let Ok(resp) = bing_resp {
            if let Ok(body) = resp.text().await {
                let results = parse_bing_results(&body, num);
                if !results.is_empty() {
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
                    return Ok(output);
                }
            }
        }

        // 兜底：DDG（Lite / HTML 结果已在并行请求中取得）
        if let Some(ddg) = ddg_lite {
            return Ok(ddg);
        }
        if let Some(ddg) = ddg_html {
            return Ok(ddg);
        }

        // 全部失败
        Ok(format!(
            "搜索 '{}' 未找到结果。\n\n可能原因:\n1. 网络问题或搜索引擎不可达\n2. HTML 结构变更，解析器需更新\n3. 代理/防火墙拦截",
            query
        ))
    }
}

// =============================================================================
// FetchUrlTool - 获取网页内容
// =============================================================================

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

// =============================================================================
// HTML 解析（简化版）
// =============================================================================

/// 解析 Bing 搜索结果 HTML（多层策略，适应 HTML 结构变更）。
///
/// 策略 1: `<li class="b_algo">`（经典结构）
/// 策略 2: `<ol id="b_results">` 内 `<li>` 含 `<h2><a href>`（变体）
/// 策略 3: 整页搜 `<cite>` + 邻接 `<a href>`（Bing 长久特征）
fn parse_bing_results(html: &str, max: usize) -> Vec<(String, String, String)> {
    // 策略 1 + 2: 先找所有可能的结果条目
    let mut results = Vec::new();

    // 两种 item 模式
    let patterns: &[&str] = &[
        r#"(?s)<li[^>]*class="[^"]*\bb_algo\b[^"]*"[^>]*>(.*?)</li>"#,
        r#"(?s)<li[^>]*class="[^"]*\bb_ans\b[^"]*"[^>]*>(.*?)</li>"#,
    ];

    let re_link = regex::Regex::new(r#"<a[^>]*href="(https?://[^"]+)"[^>]*>(.*?)</a>"#).ok();
    let re_snippet = regex::Regex::new(r#"(?s)<p[^>]*>(.*?)</p>"#).ok();

    for pat in patterns {
        if results.len() >= max {
            break;
        }
        let re_item = match regex::Regex::new(pat) {
            Ok(r) => r,
            Err(_) => continue,
        };

        for cap in re_item.captures_iter(html) {
            if results.len() >= max {
                break;
            }
            let item = match cap.get(1) {
                Some(m) => m.as_str(),
                None => continue,
            };

            let (link, title) = match re_link.as_ref().and_then(|re| re.captures(item)) {
                Some(c) => {
                    let link = c
                        .get(1)
                        .map(|m| decode_html(m.as_str()))
                        .unwrap_or_default();
                    let title = c.get(2).map(|m| strip_html(m.as_str())).unwrap_or_default();
                    (link, title)
                }
                None => continue,
            };
            if title.is_empty() || link.is_empty() {
                continue;
            }

            let snippet = re_snippet
                .as_ref()
                .and_then(|re| re.captures(item))
                .and_then(|c| c.get(1))
                .map(|m| strip_html(m.as_str()))
                .unwrap_or_default();

            results.push((title, link, snippet));
        }
    }

    // 策略 3: <cite> 标签（Bing 永久特征，含真实 URL）
    if results.is_empty() {
        let re_cite_block = regex::Regex::new(
            r#"(?s)<cite[^>]*>(.*?)</cite>\s*</div>\s*<a[^>]*href="(https?://[^"]+)"[^>]*>(.*?)</a>"#
        ).ok();
        if let Some(re) = re_cite_block {
            for cap in re.captures_iter(html) {
                if results.len() >= max {
                    break;
                }
                let url = cap
                    .get(2)
                    .map(|m| decode_html(m.as_str()))
                    .unwrap_or_default();
                let title = cap
                    .get(3)
                    .map(|m| strip_html(m.as_str()))
                    .unwrap_or_default();
                if !title.is_empty() && !url.is_empty() {
                    let snippet = cap
                        .get(1)
                        .map(|m| strip_html(m.as_str()))
                        .unwrap_or_default();
                    results.push((title, url, snippet));
                }
            }
        }
    }

    results
}

/// 解析 DuckDuckGo Lite 结果页（Bing 兜底方案）。
fn parse_ddg_lite_results(html: &str, max: usize) -> Vec<(String, String, String)> {
    let mut results = Vec::new();

    let re_row = match regex::Regex::new(r#"(?s)<tr[^>]*class="result-snippet"[^>]*>(.*?)</tr>"#) {
        Ok(re) => re,
        Err(_) => return results,
    };
    let re_link =
        regex::Regex::new(r#"<a[^>]*class="result-link"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#).ok();
    let re_snip = regex::Regex::new(r#"(?s)<td[^>]*class="result-snippet"[^>]*>(.*?)</td>"#).ok();

    for row_cap in re_row.captures_iter(html) {
        if results.len() >= max {
            break;
        }
        let row = match row_cap.get(1) {
            Some(m) => m.as_str(),
            None => continue,
        };
        let (link, title) = match re_link.as_ref().and_then(|re| re.captures(row)) {
            Some(c) => {
                let link = c
                    .get(1)
                    .map(|m| decode_html(m.as_str()))
                    .unwrap_or_default();
                let title = c.get(2).map(|m| strip_html(m.as_str())).unwrap_or_default();
                (link, title)
            }
            None => continue,
        };
        if title.is_empty() || link.is_empty() {
            continue;
        }
        let snippet = re_snip
            .as_ref()
            .and_then(|re| re.captures(row))
            .and_then(|c| c.get(1))
            .map(|m| strip_html(m.as_str()))
            .unwrap_or_default();
        results.push((title, link, snippet));
    }
    results
}

/// 尝试 DDG 搜索（Lite 或 HTML 版），成功返回格式化文本。
/// 使用全局 CLIENT 避免重复构建 HTTP 连接池。
async fn try_ddg(client: &reqwest::Client, url: &str, variant: &str, max: usize) -> Option<String> {
    let resp = client.get(url).send().await.ok()?;
    let body = resp.text().await.ok()?;
    let results = match variant {
        "lite" => parse_ddg_lite_results(&body, max),
        "html" => parse_ddg_html_results(&body, max),
        _ => return None,
    };
    if results.is_empty() {
        return None;
    }
    let src = if variant == "lite" {
        "DuckDuckGo Lite"
    } else {
        "DuckDuckGo HTML"
    };
    let mut output = format!("搜索（{} 兜底）:\n\n", src);
    for (i, (title, link, snippet)) in results.iter().enumerate() {
        output.push_str(&format!(
            "{}. {}\n   {}\n   {}\n\n",
            i + 1,
            title,
            link,
            snippet
        ));
    }
    Some(output)
}

/// 异步版 DDG 搜索，使用全局 CLIENT（配合 tokio::join! 并行竞速）。
async fn try_ddg_async(url: &str, variant: &str, max: usize) -> Option<String> {
    try_ddg(&CLIENT, url, variant, max).await
}

/// 解析 DDG HTML 版结果（html.duckduckgo.com/html/）。
/// 结构: `<div class="result">` 内含 `<a class="result__a" href="URL">标题</a>` + `<a class="result__url">URL</a>` + `<div class="result__snippet">摘要</div>`
fn parse_ddg_html_results(html: &str, max: usize) -> Vec<(String, String, String)> {
    let mut results = Vec::new();
    let re_result =
        match regex::Regex::new(r#"(?s)<div[^>]*class="[^"]*\bresult\b[^"]*"[^>]*>(.*?)</div>"#) {
            Ok(re) => re,
            Err(_) => return results,
        };
    let re_link = regex::Regex::new(
        r#"<a[^>]*class="[^"]*\bresult__a\b[^"]*"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#,
    )
    .ok();
    let re_snippet =
        regex::Regex::new(r#"(?s)<[^>]*class="[^"]*\bresult__snippet\b[^"]*"[^>]*>(.*?)</[^>]+>"#)
            .ok();

    for cap in re_result.captures_iter(html) {
        if results.len() >= max {
            break;
        }
        let block = match cap.get(1) {
            Some(m) => m.as_str(),
            None => continue,
        };
        let (link, title) = match re_link.as_ref().and_then(|re| re.captures(block)) {
            Some(c) => {
                let link = c
                    .get(1)
                    .map(|m| decode_html(m.as_str()))
                    .unwrap_or_default();
                let title = c.get(2).map(|m| strip_html(m.as_str())).unwrap_or_default();
                (link, title)
            }
            None => continue,
        };
        if title.is_empty() || link.is_empty() {
            continue;
        }
        let snippet = re_snippet
            .as_ref()
            .and_then(|re| re.captures(block))
            .and_then(|c| c.get(1))
            .map(|m| strip_html(m.as_str()))
            .unwrap_or_default();
        results.push((title, link, snippet));
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

#[cfg(test)]
mod tests {
    use super::*;

    // 模拟 Bing 搜索结果页的核心结构
    const SAMPLE: &str = r#"
<ol id="b_results">
<li class="b_algo"><h2><a href="https://www.rust-lang.org/" h="ID=SERP">Rust <strong>编程</strong>语言</a></h2><div class="b_caption"><p>A language empowering everyone to build reliable software.</p></div></li>
<li class="b_algo"><h2><a href="https://doc.rust-lang.org/book/">The Rust Book</a></h2><div class="b_caption"><p class="b_lineclamp2">官方入门书籍。</p></div></li>
</ol>
"#;

    #[test]
    fn test_parse_bing_results_basic() {
        let results = parse_bing_results(SAMPLE, 5);
        assert_eq!(results.len(), 2, "应解析出 2 条结果");
        assert_eq!(results[0].0, "Rust 编程语言");
        assert_eq!(results[0].1, "https://www.rust-lang.org/");
        assert!(results[0].2.contains("reliable software"));
        assert_eq!(results[1].1, "https://doc.rust-lang.org/book/");
    }

    #[test]
    fn test_parse_bing_results_respects_max() {
        let results = parse_bing_results(SAMPLE, 1);
        assert_eq!(results.len(), 1, "应受 max 限制");
    }

    #[test]
    fn test_parse_bing_results_empty() {
        let results = parse_bing_results("<html><body>no results</body></html>", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_strip_html_removes_tags() {
        assert_eq!(strip_html("<strong>Rust</strong> lang"), "Rust lang");
    }

    #[test]
    fn test_decode_html_entities() {
        assert_eq!(decode_html("a&amp;b &lt;c&gt;"), "a&b <c>");
    }

    // DuckDuckGo Lite 模拟 HTML
    const DDG_SAMPLE: &str = r#"
<table class="table">
<tr class="result-snippet"><td><a rel="nofollow" class="result-link" href="https://www.rust-lang.org/">Rust Programming</a><br /><span class="link-text">https://www.rust-lang.org/</span></td></tr>
<tr class="result-snippet"><td><a rel="nofollow" class="result-link" href="https://doc.rust-lang.org/">Documentation</a><br />official docs</td></tr>
</table>
"#;

    #[test]
    fn test_parse_ddg_lite_basic() {
        let results = parse_ddg_lite_results(DDG_SAMPLE, 5);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "Rust Programming");
        assert_eq!(results[0].1, "https://www.rust-lang.org/");
        assert_eq!(results[1].0, "Documentation");
    }

    #[test]
    fn test_parse_ddg_lite_empty() {
        let results = parse_ddg_lite_results("<html>no</html>", 5);
        assert!(results.is_empty());
    }
}
