//! Token 预算与使用统计

use tracing::warn;

/// Token 预算
pub(crate) struct TokenBudget {
    limit: usize,
    used: std::sync::atomic::AtomicUsize,
}

/// 使用统计
#[derive(Default)]
pub(crate) struct UsageStats {
    pub total_input: usize,
    pub total_output: usize,
}

impl TokenBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            used: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub(crate) fn add(&self, tokens: usize) {
        if self.limit == 0 {
            return;
        }
        self.used
            .fetch_add(tokens, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn check(&self) -> Result<(), raven_types::AgentError> {
        if self.limit == 0 {
            return Ok(());
        }

        let used = self.used.load(std::sync::atomic::Ordering::Relaxed);
        let ratio = used as f64 / self.limit as f64;

        if ratio >= 1.0 {
            return Err(raven_types::AgentError::budget(used, self.limit));
        }

        if ratio >= 0.8 {
            warn!(
                "Token 预算即将用完: {}/{} ({:.0}%)",
                used,
                self.limit,
                ratio * 100.0
            );
        }

        Ok(())
    }

    pub(crate) fn reset(&self) {
        self.used.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn status(&self) -> String {
        if self.limit == 0 {
            return "无限制".to_string();
        }

        let used = self.used.load(std::sync::atomic::Ordering::Relaxed);
        let ratio = used as f64 / self.limit as f64;
        format!("{:.1}% ({}/{})", ratio * 100.0, used, self.limit)
    }
}

/// 上下文统计
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextStats {
    pub current_context_tokens: usize,
    pub total_input_tokens: usize,
    pub total_output_tokens: usize,
    pub total_tokens: usize,
    pub message_count: usize,
    pub budget_status: String,
}
