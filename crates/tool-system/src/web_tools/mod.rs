//! Web 工具 - web_search + fetch_url
//!
//! 让 Agent 可以搜索网页和获取页面内容，获取实时信息。

mod fetch_url;
mod html;
mod web_search;

pub use fetch_url::FetchUrlTool;
pub use web_search::WebSearchTool;
