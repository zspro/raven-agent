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
    /// 放行本次，并放行**本轮**剩余所有待确认工具（一次性批准多个操作）
    AllowRound,
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

/// 一次「向用户提问」请求（`ask_user` 工具触发）。
#[derive(Debug, Clone)]
pub struct AskRequest {
    /// 要问用户的问题
    pub question: String,
    /// 候选选项（人类可读文本）
    pub options: Vec<String>,
    /// 是否允许多选（true=空格勾选多个，回车提交；false=单选回车即定）
    pub multi_select: bool,
    /// 是否额外提供「其他（手动输入）」让用户自由输入
    pub allow_custom: bool,
}

/// 确认回调：由 UI 层实现。
///
/// 实现需在用户拒绝、无法询问（如非交互终端）等情况下返回 `Decision::Deny`，
/// 以「默认拒绝」保证安全底线。
#[async_trait]
pub trait Confirmer: Send + Sync {
    async fn confirm(&self, req: &ConfirmRequest) -> Decision;

    /// 向用户提问并返回其选择（`ask_user` 工具用）。
    ///
    /// 返回选中的选项文本列表（多选可多个，自定义输入为单条文本）。
    /// 默认实现返回 `None`，表示当前环境无法交互提问——非交互兜底。
    async fn ask(&self, _req: &AskRequest) -> Option<Vec<String>> {
        None
    }
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

    async fn ask(&self, req: &AskRequest) -> Option<Vec<String>> {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return None;
        }
        let req = req.clone();
        tokio::task::spawn_blocking(move || prompt_question(&req))
            .await
            .ok()
            .flatten()
    }
}

/// 在终端用方向键菜单读取用户决定（阻塞）。
///
/// 仿 Claude Code：↑/↓ 移动高亮，回车选定，y/a/n 快捷键直选，Esc/Ctrl+C 拒绝。
/// 菜单做成**完全瞬态**：开头 `\x1b7` 保存光标（位于 spinner 的「○ 工具」行末），
/// 选完后 `\x1b8` 恢复 + `\x1b[0J` 清除下方整块——屏幕回到只剩「○」行，
/// 再由 TUI 的 finish 把「○」原地重绘成「●」。raw mode 失败时回退到普通 read_line。
fn prompt_decision(tool: &str, detail: &str) -> Decision {
    use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
    use std::io::Write;

    const Y: &str = "\x1b[1;33m";
    const DIM: &str = "\x1b[2m";
    const ACCENT: &str = "\x1b[36m";
    const RST: &str = "\x1b[0m";

    let options = [
        ("允许本次", Decision::Allow),
        ("允许本轮全部", Decision::AllowRound),
        ("始终允许该工具", Decision::AllowAlways),
        ("拒绝", Decision::Deny),
    ];

    let mut out = std::io::stdout();
    // 保存光标（spinner 的「○ 工具」行末），随后在其下方打印标题与摘要
    let _ = write!(out, "\x1b7\n{Y}⚠ 需要确认{RST} [{tool}]\n  {detail}\n");
    let _ = out.flush();

    // raw mode 失败 → 回退到普通输入（保留文本提示，不做瞬态清屏）
    if enable_raw_mode().is_err() {
        return prompt_decision_fallback(&options);
    }

    let mut selected = 0usize;
    let mut first = true;
    let decision = loop {
        // 重绘菜单：非首次需先把光标移回菜单顶部
        if !first {
            let _ = write!(out, "\x1b[{}A", options.len());
        }
        first = false;
        for (i, (label, _)) in options.iter().enumerate() {
            if i == selected {
                let _ = write!(out, "\r\x1b[2K{ACCENT}❯ {label}{RST}\r\n");
            } else {
                let _ = write!(out, "\r\x1b[2K{DIM}  {label}{RST}\r\n");
            }
        }
        let _ = out.flush();

        // 阻塞读键
        match event::read() {
            Ok(Event::Key(KeyEvent {
                code,
                modifiers,
                kind,
                ..
            })) => {
                // Windows 上每次物理按键会同时上报 Press 与 Release 两个事件，
                // 只处理 Press（含长按 Repeat），否则一次按键被处理两遍：
                // 高亮移动两格、菜单重绘两遍。
                if kind == KeyEventKind::Release {
                    continue;
                }
                match code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        selected = (selected + options.len() - 1) % options.len();
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        selected = (selected + 1) % options.len();
                    }
                    KeyCode::Enter => break options[selected].1,
                    KeyCode::Char('y') | KeyCode::Char('Y') => break Decision::Allow,
                    KeyCode::Char('r') | KeyCode::Char('R') => break Decision::AllowRound,
                    KeyCode::Char('a') | KeyCode::Char('A') => break Decision::AllowAlways,
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => break Decision::Deny,
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                        break Decision::Deny
                    }
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(_) => break Decision::Deny,
        }
    };

    let _ = disable_raw_mode();
    // 恢复到保存的光标（○ 行末）并清除其下方整个确认块——菜单完全退场
    let _ = write!(out, "\x1b8\x1b[0J");
    let _ = out.flush();

    decision
}

/// raw mode 不可用时的兜底：普通 read_line 读取 y/a/n。
fn prompt_decision_fallback(options: &[(&str, Decision)]) -> Decision {
    use std::io::Write;
    const DIM: &str = "\x1b[2m";
    const RST: &str = "\x1b[0m";

    let mut out = std::io::stdout();
    for (i, (label, _)) in options.iter().enumerate() {
        let _ = writeln!(out, "  {DIM}{}. {label}{RST}", i + 1);
    }
    let _ = write!(
        out,
        "  {DIM}允许(y/回车) · 允许本轮(r) · 始终允许(a) · 拒绝(n){RST}\n> "
    );
    let _ = out.flush();

    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return Decision::Deny;
    }
    match line.trim().to_lowercase().as_str() {
        "" | "y" | "yes" | "1" => Decision::Allow,
        "r" | "2" => Decision::AllowRound,
        "a" | "always" | "3" => Decision::AllowAlways,
        _ => Decision::Deny,
    }
}

/// 在终端用方向键菜单向用户提问（阻塞）。`ask_user` 工具的交互实现。
///
/// - 单选：↑/↓ 移动高亮，回车选定。
/// - 多选（`multi_select`）：空格勾选/取消，回车提交所有勾选项。
/// - `allow_custom`：菜单末尾追加「✎ 其他（手动输入）」，选中后退出 raw mode 读一行自由文本。
/// - Esc / Ctrl+C：放弃（返回 None），由调用方告知模型用户未作答。
///
/// 与确认菜单一样做成瞬态：`\x1b7` 存光标，选完 `\x1b8\x1b[0J` 清除整块。
/// raw mode 不可用时回退到编号输入。
fn prompt_question(req: &AskRequest) -> Option<Vec<String>> {
    use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
    use std::io::Write;

    const Y: &str = "\x1b[1;33m";
    const DIM: &str = "\x1b[2m";
    const ACCENT: &str = "\x1b[36m";
    const RST: &str = "\x1b[0m";

    // 行项：真实选项 + 可选的「其他」。用索引统一处理。
    let custom_idx = req.options.len(); // allow_custom 时这一项是自定义输入
    let total = req.options.len() + if req.allow_custom { 1 } else { 0 };
    if total == 0 {
        return None;
    }

    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b7\n{Y}❓ {}{RST}\n", req.question);
    if req.multi_select {
        let _ = writeln!(out, "  {DIM}空格勾选 · 回车提交 · Esc 取消{RST}");
    } else {
        let _ = writeln!(out, "  {DIM}↑/↓ 选择 · 回车确定 · Esc 取消{RST}");
    }
    let _ = out.flush();

    if enable_raw_mode().is_err() {
        return prompt_question_fallback(req);
    }

    let mut selected = 0usize;
    let mut checked = vec![false; total];
    let mut first = true;

    let result: Option<Vec<String>> = loop {
        if !first {
            let _ = write!(out, "\x1b[{total}A");
        }
        first = false;
        #[allow(clippy::needless_range_loop)]
        for i in 0..total {
            let label: &str = if i == custom_idx && req.allow_custom {
                "✎ 其他（手动输入）"
            } else {
                &req.options[i]
            };
            // 多选前缀 [x]/[ ]，单选不显示勾选框
            let mark = if req.multi_select {
                if checked[i] {
                    "[x] "
                } else {
                    "[ ] "
                }
            } else {
                ""
            };
            if i == selected {
                let _ = write!(out, "\r\x1b[2K{ACCENT}❯ {mark}{label}{RST}\r\n");
            } else {
                let _ = write!(out, "\r\x1b[2K{DIM}  {mark}{label}{RST}\r\n");
            }
        }
        let _ = out.flush();

        match event::read() {
            Ok(Event::Key(KeyEvent {
                code,
                modifiers,
                kind,
                ..
            })) => {
                if kind == KeyEventKind::Release {
                    continue;
                }
                match code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        selected = (selected + total - 1) % total;
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        selected = (selected + 1) % total;
                    }
                    KeyCode::Char(' ') if req.multi_select => {
                        checked[selected] = !checked[selected];
                    }
                    KeyCode::Enter => {
                        // 选中「其他」→ 退出 raw mode 读自由文本
                        if req.allow_custom && selected == custom_idx && !req.multi_select {
                            break read_custom_line(&mut out);
                        }
                        if req.multi_select {
                            // 自定义项被勾选时也读一行
                            let mut picked: Vec<String> = Vec::new();
                            for (i, &on) in checked.iter().enumerate() {
                                if !on {
                                    continue;
                                }
                                if req.allow_custom && i == custom_idx {
                                    if let Some(mut c) = read_custom_line(&mut out) {
                                        picked.append(&mut c);
                                    }
                                } else {
                                    picked.push(req.options[i].clone());
                                }
                            }
                            break if picked.is_empty() {
                                None
                            } else {
                                Some(picked)
                            };
                        }
                        break Some(vec![req.options[selected].clone()]);
                    }
                    KeyCode::Esc => break None,
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => break None,
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(_) => break None,
        }
    };

    let _ = disable_raw_mode();
    let _ = write!(out, "\x1b8\x1b[0J");
    let _ = out.flush();

    result
}

/// 退出 raw mode、读一行自定义文本，再恢复 raw mode（供 prompt_question 内复用）。
fn read_custom_line(out: &mut std::io::Stdout) -> Option<Vec<String>> {
    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
    use std::io::Write;
    const DIM: &str = "\x1b[2m";
    const RST: &str = "\x1b[0m";

    let _ = disable_raw_mode();
    let _ = write!(out, "\r\n  {DIM}请输入:{RST} ");
    let _ = out.flush();
    let mut line = String::new();
    let got = std::io::stdin().read_line(&mut line).is_ok();
    let _ = enable_raw_mode();
    if !got {
        return None;
    }
    let t = line.trim();
    if t.is_empty() {
        None
    } else {
        Some(vec![t.to_string()])
    }
}

/// raw mode 不可用时的提问兜底：编号选择（单选取一个编号，多选用逗号分隔）。
fn prompt_question_fallback(req: &AskRequest) -> Option<Vec<String>> {
    use std::io::Write;
    const DIM: &str = "\x1b[2m";
    const RST: &str = "\x1b[0m";

    let mut out = std::io::stdout();
    for (i, opt) in req.options.iter().enumerate() {
        let _ = writeln!(out, "  {DIM}{}. {opt}{RST}", i + 1);
    }
    if req.allow_custom {
        let _ = writeln!(
            out,
            "  {DIM}{}. 其他（手动输入）{RST}",
            req.options.len() + 1
        );
    }
    let hint = if req.multi_select {
        "输入编号（多选用逗号分隔）"
    } else {
        "输入编号"
    };
    let _ = write!(out, "  {DIM}{hint}:{RST} ");
    let _ = out.flush();

    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return None;
    }
    let custom_no = req.options.len() + 1;
    let mut picked = Vec::new();
    for tok in line.trim().split(',') {
        let tok = tok.trim();
        if let Ok(n) = tok.parse::<usize>() {
            if n >= 1 && n <= req.options.len() {
                picked.push(req.options[n - 1].clone());
            } else if req.allow_custom && n == custom_no {
                let _ = write!(out, "  {DIM}请输入:{RST} ");
                let _ = out.flush();
                let mut c = String::new();
                if std::io::stdin().read_line(&mut c).is_ok() && !c.trim().is_empty() {
                    picked.push(c.trim().to_string());
                }
            }
        }
        if !req.multi_select && !picked.is_empty() {
            break;
        }
    }
    if picked.is_empty() {
        None
    } else {
        Some(picked)
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
