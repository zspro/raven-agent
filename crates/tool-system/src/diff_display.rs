//! Diff 显示 - Aider 风格的文件修改可视化
//!
//! 借鉴 Aider 的 diff 显示，带颜色标记的行级差异。
//! 用于 file_edit 工具执行后的结果展示。

use std::collections::VecDeque;

/// 行差异类型
#[derive(Debug, Clone, PartialEq)]
pub enum DiffLine {
    Context(String), // 上下文行（未变更）
    Old(String),     // 删除的行（红色）
    New(String),     // 新增的行（绿色）
    Hunk(String),    // hunk 头信息
}

/// 计算两组文本行的差异（Myers diff 简化版）
pub fn compute_diff(old_text: &str, new_text: &str) -> Vec<DiffLine> {
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();

    // 使用 LCS（最长公共子序列）计算差异
    let lcs = longest_common_subsequence(&old_lines, &new_lines);

    let mut result = Vec::new();
    let mut old_idx = 0;
    let mut new_idx = 0;
    let mut lcs_idx = 0;

    // 上下文窗口大小
    let context_size = 3;

    while old_idx < old_lines.len() || new_idx < new_lines.len() {
        let in_lcs = lcs_idx < lcs.len();
        let lcs_line = if in_lcs { Some(lcs[lcs_idx]) } else { None };

        let old_match =
            old_idx < old_lines.len() && in_lcs && old_lines[old_idx] == lcs_line.unwrap();
        let new_match =
            new_idx < new_lines.len() && in_lcs && new_lines[new_idx] == lcs_line.unwrap();

        if old_match && new_match {
            // 匹配行（上下文）
            result.push(DiffLine::Context(old_lines[old_idx].to_string()));
            old_idx += 1;
            new_idx += 1;
            lcs_idx += 1;
        } else if old_idx < old_lines.len() && (!new_match || new_idx >= new_lines.len()) {
            // 删除的行
            if result.is_empty() || !matches!(result.last(), Some(DiffLine::Old(_))) {
                // 添加 hunk 头
                let old_start = old_idx + 1;
                let old_count = count_consecutive_not_in_lcs(&old_lines, old_idx, &lcs, lcs_idx);
                let new_start = new_idx + 1;
                result.push(DiffLine::Hunk(format!(
                    "@@ -{},{} +{},{} @@",
                    old_start, old_count, new_start, 0
                )));
            }
            result.push(DiffLine::Old(old_lines[old_idx].to_string()));
            old_idx += 1;
        } else {
            // 新增的行
            if result.is_empty() || !matches!(result.last(), Some(DiffLine::New(_))) {
                let old_start = old_idx + 1;
                let new_start = new_idx + 1;
                let new_count = count_consecutive_not_in_lcs(&new_lines, new_idx, &lcs, lcs_idx);
                result.push(DiffLine::Hunk(format!(
                    "@@ -{},{} +{},{} @@",
                    old_start, 0, new_start, new_count
                )));
            }
            result.push(DiffLine::New(new_lines[new_idx].to_string()));
            new_idx += 1;
        }
    }

    // 添加上下文窗口：只保留变更附近的上下文
    filter_context(&result, context_size)
}

/// 格式化 diff 为终端可显示的字符串（纯符号，无颜色）
pub fn format_diff_terminal(diff: &[DiffLine]) -> String {
    let mut lines = Vec::new();

    for d in diff {
        match d {
            DiffLine::Context(text) => {
                lines.push(format!("     {}", text));
            }
            DiffLine::Old(text) => {
                lines.push(format!("  ─  {}", text));
            }
            DiffLine::New(text) => {
                lines.push(format!("  +  {}", text));
            }
            DiffLine::Hunk(header) => {
                lines.push(format!("  {}", header));
            }
        }
    }

    lines.join("\n")
}

/// 格式化 diff 为带 ANSI 颜色的终端字符串：删除行红色、新增行绿色、
/// hunk 头青色、上下文暗色。供 file_edit 等编辑工具展示改动用。
pub fn format_diff_colored(diff: &[DiffLine]) -> String {
    const RED: &str = "\x1b[31m";
    const GREEN: &str = "\x1b[32m";
    const CYAN: &str = "\x1b[36m";
    const DIM: &str = "\x1b[2m";
    const RST: &str = "\x1b[0m";

    let mut lines = Vec::new();
    for d in diff {
        match d {
            DiffLine::Context(text) => lines.push(format!("{DIM}    {text}{RST}")),
            DiffLine::Old(text) => lines.push(format!("{RED}  - {text}{RST}")),
            DiffLine::New(text) => lines.push(format!("{GREEN}  + {text}{RST}")),
            DiffLine::Hunk(header) => lines.push(format!("{CYAN}  {header}{RST}")),
        }
    }
    lines.join("\n")
}

/// 生成编辑改动的彩色 diff 展示。对过大的文本（行数超过 `max_lines`）返回
/// None，让调用方回退到轻量显示，避免 LCS 的 O(m*n) 内存开销在大文件上爆炸。
pub fn render_edit_diff(old_text: &str, new_text: &str, max_lines: usize) -> Option<String> {
    let old_n = old_text.lines().count();
    let new_n = new_text.lines().count();
    if old_n.max(new_n) > max_lines {
        return None;
    }
    let diff = compute_diff(old_text, new_text);
    if diff.is_empty() {
        return None;
    }
    Some(format_diff_colored(&diff))
}

// =============================================================================
// 内部工具函数
// =============================================================================

/// 最长公共子序列（LCS）
fn longest_common_subsequence<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<&'a str> {
    let m = a.len();
    let n = b.len();

    // 动态规划表
    let mut dp = vec![vec![0; n + 1]; m + 1];

    for i in 1..=m {
        for j in 1..=n {
            if a[i - 1] == b[j - 1] {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    // 回溯
    let mut result = Vec::new();
    let mut i = m;
    let mut j = n;

    while i > 0 && j > 0 {
        if a[i - 1] == b[j - 1] {
            result.push(a[i - 1]);
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] > dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }

    result.reverse();
    result
}

/// 计算从 start 开始连续不在 LCS 中的行数
fn count_consecutive_not_in_lcs(
    lines: &[&str],
    start: usize,
    lcs: &[&str],
    lcs_start: usize,
) -> usize {
    let mut count = 0;
    let lcs_idx = lcs_start;

    for line in lines.iter().skip(start) {
        if lcs_idx < lcs.len() && *line == lcs[lcs_idx] {
            break;
        }
        count += 1;
    }

    count.max(1)
}

/// 过滤上下文：只保留变更附近的行
fn filter_context(diff: &[DiffLine], context_size: usize) -> Vec<DiffLine> {
    let mut result = Vec::new();
    let mut context_buffer: VecDeque<DiffLine> = VecDeque::new();
    let mut in_change = false;
    let mut pending_context: Vec<DiffLine> = Vec::new();

    for d in diff {
        match d {
            DiffLine::Old(_) | DiffLine::New(_) | DiffLine::Hunk(_) => {
                // 变更行：输出缓冲区中的上下文，然后输出变更
                if !in_change {
                    // 首次遇到变更，输出前导上下文
                    let start = context_buffer.len().saturating_sub(context_size);
                    for item in context_buffer.iter().skip(start) {
                        result.push(item.clone());
                    }
                    context_buffer.clear();
                    in_change = true;
                }

                // 输出待处理的尾部上下文
                for ctx in &pending_context {
                    result.push(ctx.clone());
                }
                pending_context.clear();

                result.push(d.clone());
            }
            DiffLine::Context(text) => {
                if in_change {
                    // 变更后的上下文：先缓存
                    pending_context.push(DiffLine::Context(text.clone()));
                    if pending_context.len() > context_size {
                        pending_context.remove(0);
                    }
                } else {
                    // 变更前的上下文：加入缓冲区
                    context_buffer.push_back(DiffLine::Context(text.clone()));
                    if context_buffer.len() > context_size {
                        context_buffer.pop_front();
                    }
                }
            }
        }
    }

    // 输出最后的上下文
    for ctx in pending_context {
        result.push(ctx);
    }

    result
}

// =============================================================================
// Git 风格 diff（Aider 借鉴）
// =============================================================================

/// 生成 Git 风格的统一 diff（Aider 默认格式）
pub fn git_style_diff(old_path: &str, new_path: &str, old_text: &str, new_text: &str) -> String {
    let mut result = Vec::new();

    result.push(format!("diff --git a/{} b/{}", old_path, new_path));
    result.push(format!("--- a/{}", old_path));
    result.push(format!("+++ b/{}", new_path));

    let diff_lines = compute_diff(old_text, new_text);
    for d in &diff_lines {
        match d {
            DiffLine::Hunk(h) => result.push(h.clone()),
            DiffLine::Context(t) => result.push(format!(" {}", t)),
            DiffLine::Old(t) => result.push(format!("-{}", t)),
            DiffLine::New(t) => result.push(format!("+{}", t)),
        }
    }

    result.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_edit_diff_shows_old_and_new() {
        let out = render_edit_diff("foo\nbar\n", "foo\nbaz\n", 400).unwrap();
        // bar 被删、baz 新增；ANSI 颜色码存在
        assert!(out.contains("bar"));
        assert!(out.contains("baz"));
        assert!(out.contains("\x1b["));
    }

    #[test]
    fn render_edit_diff_returns_none_for_oversized() {
        let big = "x\n".repeat(500);
        assert!(render_edit_diff(&big, &big, 400).is_none());
    }

    #[test]
    fn render_edit_diff_none_when_identical() {
        // 完全相同 → compute_diff 只产生上下文，filter 后为空 → None
        assert!(render_edit_diff("same\n", "same\n", 400).is_none());
    }

    #[test]
    fn compute_diff_detects_change() {
        let diff = compute_diff("a\nb\nc\n", "a\nB\nc\n");
        assert!(diff.iter().any(|d| matches!(d, DiffLine::Old(t) if t == "b")));
        assert!(diff.iter().any(|d| matches!(d, DiffLine::New(t) if t == "B")));
    }
}
