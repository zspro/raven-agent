//! 工具事件解析与美化：把流式 `tool_call` / `tool_result` 事件渲染成实色圆点标题、
//! 紧凑参数和缩进预览的终端输出。
//!
//! 渲染模型（重要）：模型一轮可发**多个并行 tool_call**，core 按调用顺序执行并按
//! 同序发回 tool_result。因此 TUI 不在 call 时画占位行（会产生孤儿 `○` 且 `\r`
//! 原地重绘无法对应到正确的调用），而是把 (name,args) 入队，等对应 result 到达时
//! 再把「● 工具 参数」标题与「└ 结果」预览**成组一次性打印**。

use crate::theme::ColorTheme;

pub(crate) fn parse_tool_call(content: Option<&str>) -> (String, String) {
    match content.and_then(|s| serde_json::from_str::<raven_types::ToolCall>(s).ok()) {
        Some(tc) => (tc.function.name, tc.function.arguments),
        None => ("tool".into(), String::new()),
    }
}

pub(crate) fn parse_tool_result(content: Option<&str>) -> (String, String, bool) {
    match content.and_then(|s| serde_json::from_str::<raven_types::ToolResult>(s).ok()) {
        Some(tr) => (tr.name, tr.content, tr.is_error),
        None => (String::new(), content.unwrap_or_default().into(), false),
    }
}

/// 仅渲染工具调用的标题行「● 工具 参数」，青色圆点表示「执行中」。
///
/// 在 `tool_call` 事件到达时立即打印，避免并行 task 等耗时工具执行期间屏幕长时间空白。
/// 结果体由稍后到达的 `tool_result` 通过 `render_result_only` 补上。
pub(crate) fn render_tool_header(name: &str, args: &str) -> String {
    let brief = brief_for(name, args);
    let dot = ColorTheme::ACCENT;
    let rst = ColorTheme::RESET;
    let bold = ColorTheme::BOLD;
    let dim = ColorTheme::DIM;
    if brief.is_empty() {
        format!("\n{dot}●{rst} {bold}{name}{rst} {dim}(no args){rst}\n")
    } else {
        format!("\n{dot}●{rst} {bold}{name}{rst} {dim}{brief}{rst}\n")
    }
}

/// 仅渲染结果体（缩进预览），配合 `render_tool_header` 使用。
///
/// `preview_lines` 控制最多显示几行；超出时折叠并提示「Ctrl+O 展开」。
/// `None` 表示完整展开（不折叠、不提示），用于 Ctrl+O 重打。
/// 标题行的青色圆点已打印无法回改，失败时用红色 `✗` 结果体明确标示错误。
pub(crate) fn render_result_only(
    output: &str,
    is_error: bool,
    preview_lines: Option<usize>,
) -> String {
    render_result_body(output, is_error, preview_lines)
}

/// 按工具名选择参数摘要策略（task 只显 description，ask_user 显 question，其余压平参数）。
fn brief_for(name: &str, args: &str) -> String {
    if name == "task" {
        task_brief(args)
    } else if name == "ask_user" {
        ask_user_brief(args)
    } else {
        brief_args(args)
    }
}

/// 结果预览：成功灰色 `└` 引导 / 失败红色 `✗` 高亮。
///
/// `preview_lines = Some(n)`：最多 n 行，超出折叠并在末尾提示展开；
/// `None`：完整输出，不折叠。
fn render_result_body(output: &str, is_error: bool, preview_lines: Option<usize>) -> String {
    let trimmed = output.trim_end();
    let total = trimmed.lines().count();
    let (body, hidden) = match preview_lines {
        Some(n) if total > n => {
            let kept: String = trimmed.lines().take(n).collect::<Vec<_>>().join("\n");
            (kept, total - n)
        }
        _ => (trimmed.to_string(), 0),
    };
    let dim = ColorTheme::DIM;
    let rst = ColorTheme::RESET;
    if is_error {
        let mut out = format!(
            "  {err}✗{rst} {dim}{body}{rst}\n",
            err = ColorTheme::ERROR,
            rst = rst,
            dim = dim
        );
        if hidden > 0 {
            out.push_str(&fold_hint(total));
        }
        out
    } else {
        let mut out = String::new();
        let mut lines = body.lines();
        if let Some(first) = lines.next() {
            out.push_str(&format!("  {dim}└{rst} {dim}{first}{rst}\n"));
        } else {
            out.push_str(&format!("  {dim}└{rst}\n"));
        }
        for line in lines {
            out.push_str(&format!("    {dim}{line}{rst}\n"));
        }
        if hidden > 0 {
            out.push_str(&fold_hint(total));
        }
        out
    }
}

/// 折叠提示行：「… 共 N 行 · Ctrl+O 展开」。
fn fold_hint(total: usize) -> String {
    format!(
        "    {dim}… 共 {total} 行 · Ctrl+O 展开{rst}\n",
        dim = ColorTheme::DIM,
        rst = ColorTheme::RESET,
    )
}

/// 把 JSON 参数压成一行简短摘要（最多 ~60 字符）
fn brief_args(args: &str) -> String {
    let trimmed = args.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return String::new();
    }
    let compact = match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(serde_json::Value::Object(map)) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    let vs = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    format!("{k}={vs}")
                })
                .collect();
            parts.join(" ")
        }
        _ => trimmed.to_string(),
    };
    let oneline: String = compact.split_whitespace().collect::<Vec<_>>().join(" ");
    if oneline.chars().count() > 60 {
        let s: String = oneline.chars().take(57).collect();
        format!("{s}...")
    } else {
        oneline
    }
}

/// task 工具的简短摘要：只取 description（prompt 太长不展示）。
fn task_brief(args: &str) -> String {
    serde_json::from_str::<serde_json::Value>(args.trim())
        .ok()
        .and_then(|v| {
            v.get("description")
                .and_then(|d| d.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default()
}

/// ask_user 工具的简短摘要：展示 question（选项菜单由 confirmer 在提问时已画过）。
fn ask_user_brief(args: &str) -> String {
    serde_json::from_str::<serde_json::Value>(args.trim())
        .ok()
        .and_then(|v| {
            v.get("question")
                .and_then(|q| q.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_brief_extracts_description_only() {
        let args = r#"{"description":"调查配置","prompt":"读 config.rs 并总结所有字段"}"#;
        assert_eq!(task_brief(args), "调查配置");
    }

    #[test]
    fn task_brief_empty_on_bad_json() {
        assert_eq!(task_brief("not json"), "");
    }

    #[test]
    fn render_tool_task_shows_description() {
        let args = r#"{"description":"调查配置","prompt":"很长的指令..."}"#;
        let header = render_tool_header("task", args);
        assert!(header.contains("task"));
        assert!(header.contains("调查配置"));
        assert!(!header.contains("很长的指令"));
        let body = render_result_only("结论", false, Some(5));
        assert!(body.contains("结论"));
    }

    #[test]
    fn render_result_folds_when_over_preview() {
        let output = (1..=10)
            .map(|i| format!("行{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let folded = render_result_only(&output, false, Some(5));
        assert!(folded.contains("行5"));
        assert!(!folded.contains("行6"));
        assert!(folded.contains("共 10 行"));
        assert!(folded.contains("Ctrl+O 展开"));
    }

    #[test]
    fn render_result_full_when_none() {
        let output = (1..=10)
            .map(|i| format!("行{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let full = render_result_only(&output, false, None);
        assert!(full.contains("行10"));
        assert!(!full.contains("Ctrl+O 展开"));
    }

    #[test]
    fn render_result_no_fold_when_within_preview() {
        let full = render_result_only("一行而已", false, Some(5));
        assert!(!full.contains("Ctrl+O 展开"));
    }
}
