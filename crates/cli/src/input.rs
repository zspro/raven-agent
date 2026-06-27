//! stdin 读取与 prompt 合并

use std::io::{IsTerminal, Read};

/// 读取管道传入的 stdin 内容。
///
/// 当 stdin 是终端（交互式）、读取失败、或内容 trim 后为空时返回 `None`；
/// 否则返回 `Some(原始内容)`。
pub(crate) fn read_piped_stdin() -> Option<String> {
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut buffer = String::new();
    if std::io::stdin().read_to_string(&mut buffer).is_err() {
        return None;
    }
    if buffer.trim().is_empty() {
        return None;
    }
    Some(buffer)
}

/// 合并命令行 prompt 与管道 stdin。
///
/// - stdin 为空 → 原样返回 prompt
/// - prompt 为空 → 返回 stdin（trim 后）
/// - 两者都有 → `prompt\n\nstdin`（prompt 在前，管道上下文在后）
pub(crate) fn merge_prompt_with_stdin(prompt: &str, stdin_content: Option<&str>) -> String {
    let Some(raw) = stdin_content else {
        return prompt.to_string();
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return prompt.to_string();
    }
    if prompt.trim().is_empty() {
        return trimmed.to_string();
    }
    format!("{prompt}\n\n{trimmed}")
}
