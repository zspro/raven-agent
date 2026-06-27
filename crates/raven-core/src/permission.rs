//! 权限门控：三态权限检查器与工具执行前的交互式确认。

use crate::{confirm, Agent, ConfirmRequest, Confirmer, Decision};
use raven_types::ToolCall;
use std::collections::HashSet;
use std::sync::{Arc, RwLock as StdRwLock};
use tokio::sync::RwLock;

/// 权限门控决定
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Gate {
    /// 直接放行
    Allow,
    /// 直接拒绝，附原因
    Deny(String),
    /// 需要向用户实时确认
    NeedConfirm,
}

/// 权限检查器
#[derive(Clone)]
pub(crate) struct PermissionChecker {
    /// 用共享锁持有，使配置热重载能在运行时切换权限模式，
    /// 且变更对已 clone 到流式任务中的副本同样可见。
    pub(crate) mode: Arc<StdRwLock<String>>,
    pub(crate) allowed: Arc<StdRwLock<Vec<String>>>,
    pub(crate) denied: Arc<StdRwLock<Vec<String>>>,
    /// 本会话内"始终允许"的工具（用户选择 AllowAlways 后写入），避免反复打扰。
    pub(crate) session_allow: Arc<RwLock<HashSet<String>>>,
}

impl PermissionChecker {
    /// 当前是否只读模式。
    pub(crate) fn is_readonly(&self) -> bool {
        self.mode.read().unwrap().as_str() == "readonly"
    }

    /// 三态权限门控。
    ///
    /// - `readonly`：写工具一律拒绝（schema 已不下发，这里再兜底）。
    /// - `yes`：除显式 denied 外全部放行。
    /// - `auto`：除 denied 外全部放行（与 yes 类似，但保留语义区分）。
    /// - `ask`：denied → 拒绝；白名单或本会话已"始终允许" → 放行；其余 → 需确认。
    async fn gate(&self, tool_name: &str) -> Gate {
        let name = tool_name.to_string();

        if self.denied.read().unwrap().contains(&name) {
            return Gate::Deny(format!(
                "工具 '{tool_name}' 在 'permission.denied_tools' 黑名单中，已拒绝。"
            ));
        }

        // 先取出模式字符串再释放锁，避免在 await 点持有 std 锁
        let mode = self.mode.read().unwrap().clone();
        match mode.as_str() {
            "readonly" => Gate::Deny(format!(
                "只读模式（readonly）下不允许执行工具 '{tool_name}'。"
            )),
            "yes" | "auto" => Gate::Allow,
            // ask 模式（默认）
            _ => {
                if self.allowed.read().unwrap().contains(&name) {
                    return Gate::Allow;
                }
                if self.session_allow.read().await.contains(&name) {
                    return Gate::Allow;
                }
                Gate::NeedConfirm
            }
        }
    }

    /// 记录"本会话始终允许"该工具（用户选择 AllowAlways 后）。
    async fn remember_allow(&self, tool_name: &str) {
        self.session_allow
            .write()
            .await
            .insert(tool_name.to_string());
    }
}

impl Agent {
    /// 工具执行前的权限门控 + 交互式确认（CLI 同步路径与流式路径共用）。
    ///
    /// 返回 `Ok(())` 表示放行，`Err(reason)` 表示拒绝（reason 作为工具错误结果回传给模型）。
    pub(crate) async fn gate_and_confirm(
        permission: &PermissionChecker,
        confirmer: Option<&Arc<dyn Confirmer>>,
        call: &ToolCall,
        round_allow: &std::sync::atomic::AtomicBool,
    ) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        let tool = &call.function.name;
        // 本轮已选「允许本轮全部」→ 直接放行（仍受 denied/readonly 兜底）
        match permission.gate(tool).await {
            Gate::Allow => Ok(()),
            Gate::Deny(reason) => Err(reason),
            Gate::NeedConfirm => {
                if round_allow.load(Ordering::Relaxed) {
                    return Ok(());
                }
                let Some(confirmer) = confirmer else {
                    // 无确认回调（如非交互场景）：默认拒绝，保证安全底线。
                    return Err(format!(
                        "需要确认但当前环境无法交互，已拒绝工具 '{tool}'\n\
                         修复: 在交互终端中运行，或在配置 'permission.allowed_tools' 添加 '{tool}'，或切到 'yes' 模式。"
                    ));
                };
                let args: serde_json::Value =
                    serde_json::from_str(&call.function.arguments).unwrap_or_default();
                let detail = confirm::describe_tool(tool, &args);
                let req = ConfirmRequest {
                    tool: tool.clone(),
                    detail,
                };
                match confirmer.confirm(&req).await {
                    Decision::Allow => Ok(()),
                    Decision::AllowAlways => {
                        permission.remember_allow(tool).await;
                        Ok(())
                    }
                    Decision::AllowRound => {
                        round_allow.store(true, Ordering::Relaxed);
                        Ok(())
                    }
                    Decision::Deny => Err(format!("用户拒绝执行工具 '{tool}'")),
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AllowAllConfirmer, DenyAllConfirmer};
    use raven_types::ToolCallFunction;

    fn checker(mode: &str) -> PermissionChecker {
        PermissionChecker {
            mode: Arc::new(StdRwLock::new(mode.to_string())),
            allowed: Arc::new(StdRwLock::new(vec![
                "file_read".to_string(),
                "search".to_string(),
            ])),
            denied: Arc::new(StdRwLock::new(vec!["dangerous".to_string()])),
            session_allow: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    #[tokio::test]
    async fn readonly_denies_everything() {
        let c = checker("readonly");
        assert!(matches!(c.gate("file_read").await, Gate::Deny(_)));
        assert!(matches!(c.gate("shell").await, Gate::Deny(_)));
    }

    #[tokio::test]
    async fn yes_mode_allows_except_denied() {
        let c = checker("yes");
        assert_eq!(c.gate("shell").await, Gate::Allow);
        assert!(matches!(c.gate("dangerous").await, Gate::Deny(_)));
    }

    #[tokio::test]
    async fn ask_allows_whitelist_confirms_others() {
        let c = checker("ask");
        // 白名单内静默放行
        assert_eq!(c.gate("file_read").await, Gate::Allow);
        // 黑名单直接拒绝
        assert!(matches!(c.gate("dangerous").await, Gate::Deny(_)));
        // 其余需确认
        assert_eq!(c.gate("shell").await, Gate::NeedConfirm);
    }

    #[tokio::test]
    async fn ask_remembers_allow_always() {
        let c = checker("ask");
        assert_eq!(c.gate("shell").await, Gate::NeedConfirm);
        c.remember_allow("shell").await;
        // AllowAlways 后本会话不再询问
        assert_eq!(c.gate("shell").await, Gate::Allow);
    }

    #[tokio::test]
    async fn gate_and_confirm_allow_all() {
        let perm = checker("ask");
        let confirmer: Arc<dyn Confirmer> = Arc::new(AllowAllConfirmer);
        let call = ToolCall {
            index: 0,
            id: "1".to_string(),
            call_type: "function".to_string(),
            function: ToolCallFunction {
                name: "shell".to_string(),
                arguments: r#"{"command":"ls"}"#.to_string(),
            },
        };
        assert!(Agent::gate_and_confirm(
            &perm,
            Some(&confirmer),
            &call,
            &std::sync::atomic::AtomicBool::new(false)
        )
        .await
        .is_ok());
    }

    #[tokio::test]
    async fn gate_and_confirm_deny_when_user_rejects() {
        let perm = checker("ask");
        let confirmer: Arc<dyn Confirmer> = Arc::new(DenyAllConfirmer);
        let call = ToolCall {
            index: 0,
            id: "1".to_string(),
            call_type: "function".to_string(),
            function: ToolCallFunction {
                name: "shell".to_string(),
                arguments: r#"{"command":"ls"}"#.to_string(),
            },
        };
        assert!(Agent::gate_and_confirm(
            &perm,
            Some(&confirmer),
            &call,
            &std::sync::atomic::AtomicBool::new(false)
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn gate_and_confirm_no_confirmer_denies() {
        let perm = checker("ask");
        let call = ToolCall {
            index: 0,
            id: "1".to_string(),
            call_type: "function".to_string(),
            function: ToolCallFunction {
                name: "shell".to_string(),
                arguments: r#"{"command":"ls"}"#.to_string(),
            },
        };
        // 无确认回调（非交互场景）→ 默认拒绝
        assert!(Agent::gate_and_confirm(
            &perm,
            None,
            &call,
            &std::sync::atomic::AtomicBool::new(false)
        )
        .await
        .is_err());
    }

    #[test]
    fn describe_tool_renders_summary() {
        let args = serde_json::json!({"command": "rm file.txt"});
        assert!(confirm::describe_tool("shell", &args).contains("rm file.txt"));
        let args = serde_json::json!({"path": "a.txt", "append": false});
        assert!(confirm::describe_tool("file_write", &args).contains("a.txt"));
    }
}
