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

    /// 查询缓存
    pub async fn get(&self, key: &str) -> Option<ChatResponse> {
        let entries = self.entries.read().await;

        if let Some(entry) = entries.get(key) {
            // 检查是否过期
            if entry.created_at.elapsed().as_secs() > self.ttl_secs {
                debug!("缓存条目已过期: {}", key);
                return None;
            }

            debug!("缓存命中: {}", key);
            return Some(ChatResponse {
                content: entry.response.clone(),
                tool_calls: Vec::new(), // 不缓存带工具调用的响应
                model: "cached".to_string(),
                finish_reason: "stop".to_string(),
                usage: TokenUsage {
                    input: 0,
                    output: 0,
                    total: 0,
                    cached: Some(entry.usage.total),
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

        // 如果超过容量，移除最旧的条目
        if entries.len() >= self.max_entries {
            let oldest = entries
                .iter()
                .min_by_key(|(_, v)| v.created_at)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                entries.remove(&k);
            }
        }

        entries.insert(
            key,
            CacheEntry {
                response: response.content.clone(),
                usage: response.usage.clone(),
                created_at: std::time::Instant::now(),
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
