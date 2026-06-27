//! WebSearchTool - 网页搜索

use super::html::{parse_bing_results, try_ddg_async, CLIENT};
use crate::Tool;
use async_trait::async_trait;
use raven_types::{FunctionSchema, ToolSchema};
use serde_json::json;

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
