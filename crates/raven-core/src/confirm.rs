//! 工具执行前的交互式确认
//!
//! `Agent::execute_tools` 是无交互的纯异步执行，自身无法向用户提问。
//! 这里定义一个 `Confirmer` 回调 trait，由 UI 层（CLI/TUI）实现并注入到 core，
//! 在执行敏感工具（尤其是 shell / file_write 等写操作）前实时征求用户同意。
//!
//! 设计参考 claw-code / Claude Code 的权限确认 UX：允许一次 / 始终允许 / 拒绝。

use async_trait::async_trait;

/// 用户对一次工具执行请求的决定
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// 放行本次执行
    Allow,
    /// 放行本次，并在本会话内对该工具不再询问
    AllowAlways,
    /// 拒绝本次执行
    Deny,
}

/// 一次确认请求的上下文
#[derive(Debug, Clone)]
pub struct ConfirmRequest {
    /// 工具名（如 "shell" / "file_write"）
    pub tool: String,
    /// 人类可读的操作摘要（如具体命令、目标文件路径）
    pub detail: String,
}

/// 确认回调：由 UI 层实现。
///
/// 实现需在用户拒绝、无法询问（如非交互终端）等情况下返回 `Decision::Deny`，
/// 以「默认拒绝」保证安全底线。
#[async_trait]
pub trait Confirmer: Send + Sync {
    async fn confirm(&self, req: &ConfirmRequest) -> Decision;
}

/// 永远拒绝（非交互场景的安全兜底）
pub struct DenyAllConfirmer;

#[async_trait]
impl Confirmer for DenyAllConfirmer {
    async fn confirm(&self, _req: &ConfirmRequest) -> Decision {
        Decision::Deny
    }
}

/// 永远放行（仅用于测试 / 明确信任场景）
pub struct AllowAllConfirmer;

#[async_trait]
impl Confirmer for AllowAllConfirmer {
    async fn confirm(&self, _req: &ConfirmRequest) -> Decision {
        Decision::Allow
    }
}

/// 终端 stdin 确认器：在终端打印操作摘要，读取一行 y / n / a。
///
/// - `y` / 回车 → 允许本次（Allow）
/// - `a`        → 始终允许该工具（AllowAlways）
/// - 其他       → 拒绝（Deny）
///
/// 通过 `spawn_blocking` 在阻塞线程读取 stdin，避免阻塞 async 运行时。
/// 若 stdin 非交互（管道/CI）或读取失败，返回 Deny 作为安全兜底。
pub struct StdinConfirmer;

#[async_trait]
impl Confirmer for StdinConfirmer {
    async fn confirm(&self, req: &ConfirmRequest) -> Decision {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return Decision::Deny;
        }
        let tool = req.tool.clone();
        let detail = req.detail.clone();
        tokio::task::spawn_blocking(move || prompt_decision(&tool, &detail))
            .await
            .unwrap_or(Decision::Deny)
    }
}

/// 在终端打印彩色确认提示并读取用户决定（阻塞）。
fn prompt_decision(tool: &str, detail: &str) -> Decision {
    use std::io::Write;

    // ANSI: 黄色加粗标题 + 暗色提示
    const Y: &str = "\x1b[1;33m";
    const DIM: &str = "\x1b[2m";
    const RST: &str = "\x1b[0m";

    let mut out = std::io::stdout();
    let _ = write!(
        out,
        "\n{Y}⚠ 需要确认{RST} [{tool}]\n  {detail}\n  {DIM}允许(y/回车) · 始终允许(a) · 拒绝(n){RST}\n> "
    );
    let _ = out.flush();

    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return Decision::Deny;
    }
    match line.trim().to_lowercase().as_str() {
        "" | "y" | "yes" => Decision::Allow,
        "a" | "always" => Decision::AllowAlways,
        _ => Decision::Deny,
    }
}

/// 为一次工具调用生成人类可读的操作摘要，用于确认提示。
pub fn describe_tool(tool: &str, args: &serde_json::Value) -> String {
    match tool {
        "shell" => args
            .get("command")
            .and_then(|v| v.as_str())
            .map(|c| format!("执行命令: {c}"))
            .unwrap_or_else(|| "执行 shell 命令".to_string()),
        "file_write" => args
            .get("path")
            .and_then(|v| v.as_str())
            .map(|p| {
                let append = args
                    .get("append")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let verb = if append {
                    "追加写入"
                } else {
                    "覆盖写入"
                };
                format!("{verb}文件: {p}")
            })
            .unwrap_or_else(|| "写入文件".to_string()),
        "file_edit" => args
            .get("path")
            .and_then(|v| v.as_str())
            .map(|p| format!("编辑文件: {p}"))
            .unwrap_or_else(|| "编辑文件".to_string()),
        "git" => {
            let sub = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let extra = args.get("args").and_then(|v| v.as_str()).unwrap_or("");
            format!("git {sub} {extra}").trim().to_string()
        }
        other => format!("调用工具 {other}"),
    }
}
