//! 响应缓存系统
//!
//! 缓存 LLM 响应以避免重复请求相同的问题，节省 Token。
//! 使用请求内容的哈希作为键，缓存文本响应（不缓存工具调用）。

use raven_types::{ChatResponse, TokenUsage};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;

/// 缓存条目
#[derive(Clone)]
struct CacheEntry {
    response: String,
    usage: TokenUsage,
    created_at: std::time::Instant,
    /// 最近一次命中时间，用于 LRU 淘汰（区别于 created_at 的 TTL 计时）。
    last_access: std::time::Instant,
}

/// 响应缓存
pub struct ResponseCache {
    entries: Arc<RwLock<HashMap<String, CacheEntry>>>,
    max_entries: usize,
    ttl_secs: u64,
}

impl Default for ResponseCache {
    /// 合理默认值：100 条、1 小时过期
    fn default() -> Self {
        Self::new(100, 3600)
    }
}

impl ResponseCache {
    /// 创建缓存
    pub fn new(max_entries: usize, ttl_secs: u64) -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            max_entries,
            ttl_secs,
        }
    }

    /// 获取缓存键
    /// 基于模型ID和消息列表生成
    pub fn make_key(model: &str, messages: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(model.as_bytes());
        hasher.update(messages.as_bytes());
        format!("{:x}", hasher.finalize())[..16].to_string()
    }

    /// 查询缓存。命中时刷新 LRU 最近访问时间；顺带清除已过期条目。
    pub async fn get(&self, key: &str) -> Option<ChatResponse> {
        let mut entries = self.entries.write().await;

        // 先清除所有过期条目（避免过期项长期占位、误导淘汰）。
        let ttl = std::time::Duration::from_secs(self.ttl_secs);
        entries.retain(|_, e| e.created_at.elapsed() <= ttl);

        if let Some(entry) = entries.get_mut(key) {
            entry.last_access = std::time::Instant::now();
            debug!("缓存命中: {}", key);
            let total = entry.usage.total;
            return Some(ChatResponse {
                content: entry.response.clone(),
                tool_calls: Vec::new(), // 不缓存带工具调用的响应
                model: "cached".to_string(),
                finish_reason: "stop".to_string(),
                usage: TokenUsage {
                    input: 0,
                    output: 0,
                    total: 0,
                    cached: Some(total),
                },
            });
        }

        None
    }

    /// 存入缓存
    pub async fn put(&self, key: String, response: &ChatResponse) {
        // 只缓存没有工具调用的简单文本响应
        if !response.tool_calls.is_empty() {
            return;
        }

        let mut entries = self.entries.write().await;

        // 先清掉过期条目，可能腾出空间。
        let ttl = std::time::Duration::from_secs(self.ttl_secs);
        entries.retain(|_, e| e.created_at.elapsed() <= ttl);

        // 仍超过容量时，按最近最少使用（last_access 最早）淘汰。
        if entries.len() >= self.max_entries {
            let lru = entries
                .iter()
                .min_by_key(|(_, v)| v.last_access)
                .map(|(k, _)| k.clone());
            if let Some(k) = lru {
                entries.remove(&k);
            }
        }

        let now = std::time::Instant::now();
        entries.insert(
            key,
            CacheEntry {
                response: response.content.clone(),
                usage: response.usage.clone(),
                created_at: now,
                last_access: now,
            },
        );

        debug!("缓存已存储: {} 条", entries.len());
    }

    /// 清空缓存
    pub async fn clear(&self) {
        let mut entries = self.entries.write().await;
        entries.clear();
        debug!("缓存已清空");
    }

    /// 获取统计
    pub async fn stats(&self) -> CacheStats {
        let entries = self.entries.read().await;
        CacheStats {
            entries: entries.len(),
            max_entries: self.max_entries,
            ttl_secs: self.ttl_secs,
        }
    }
}

/// 缓存统计
#[derive(Debug, Clone, serde::Serialize)]
pub struct CacheStats {
    pub entries: usize,
    pub max_entries: usize,
    pub ttl_secs: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(content: &str) -> ChatResponse {
        ChatResponse {
            content: content.to_string(),
            tool_calls: Vec::new(),
            usage: TokenUsage::default(),
            model: "test".to_string(),
            finish_reason: "stop".to_string(),
        }
    }

    #[tokio::test]
    async fn test_make_key_stable_and_distinct() {
        let a = ResponseCache::make_key("m", "hello");
        let b = ResponseCache::make_key("m", "hello");
        let c = ResponseCache::make_key("m", "world");
        assert_eq!(a, b, "相同输入应得相同键");
        assert_ne!(a, c, "不同输入应得不同键");
    }

    #[tokio::test]
    async fn test_put_get_roundtrip() {
        let cache = ResponseCache::new(10, 3600);
        cache.put("k1".to_string(), &resp("hi")).await;
        let got = cache.get("k1").await.expect("应命中");
        assert_eq!(got.content, "hi");
    }

    #[tokio::test]
    async fn test_tool_call_responses_not_cached() {
        let cache = ResponseCache::new(10, 3600);
        let mut r = resp("x");
        r.tool_calls = vec![raven_types::ToolCall {
            index: 0,
            id: "1".to_string(),
            call_type: "function".to_string(),
            function: raven_types::ToolCallFunction {
                name: "x".to_string(),
                arguments: "{}".to_string(),
            },
        }];
        cache.put("k".to_string(), &r).await;
        assert!(cache.get("k").await.is_none(), "带工具调用的响应不应缓存");
    }

    #[tokio::test]
    async fn test_lru_eviction_keeps_recently_used() {
        let cache = ResponseCache::new(2, 3600);
        cache.put("a".to_string(), &resp("A")).await;
        cache.put("b".to_string(), &resp("B")).await;
        // 访问 a，使其成为最近使用；插入 c 应淘汰最久未用的 b
        let _ = cache.get("a").await;
        cache.put("c".to_string(), &resp("C")).await;
        assert!(cache.get("a").await.is_some(), "最近访问的 a 应保留");
        assert!(cache.get("c").await.is_some(), "新插入的 c 应保留");
        assert!(cache.get("b").await.is_none(), "最久未用的 b 应被淘汰");
    }

    #[tokio::test]
    async fn test_expired_entry_purged() {
        let cache = ResponseCache::new(10, 0); // ttl=0：任何条目立即过期
        cache.put("k".to_string(), &resp("v")).await;
        // 等待一个时间刻度，确保 elapsed > 0
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        assert!(cache.get("k").await.is_none(), "过期条目不应命中");
    }
}
