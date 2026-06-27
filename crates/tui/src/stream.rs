//! 流式 Markdown 渲染（延迟提交，参考 claw-code）：累积增量文本，只在遇到"安全边界"
//! （空行分隔的段落、闭合的代码围栏）时提交渲染，已提交内容不再重绘。

use crate::render::Renderer;
use crate::theme::FenceMarker;

/// 累积流式文本，只在遇到"安全边界"（空行分隔的段落、闭合的代码围栏）时
/// 提交渲染，已提交内容不再重绘。流结束时 flush 剩余部分。
pub(crate) struct MarkdownStreamState {
    pending: String,
}

impl MarkdownStreamState {
    pub(crate) fn new() -> Self {
        Self {
            pending: String::new(),
        }
    }

    /// 追加增量，返回本次可安全输出的已渲染 ANSI（可能为空）
    pub(crate) fn push(&mut self, delta: &str, renderer: &Renderer) -> String {
        self.pending.push_str(delta);
        let boundary = find_stream_safe_boundary(&self.pending);
        if boundary == 0 {
            return String::new();
        }
        let committed: String = self.pending.drain(..boundary).collect();
        // 规范化嵌套围栏：防止 LLM 输出的 markdown 代码示例破坏外层代码块
        let safe = normalize_nested_fences(&committed);
        renderer.render(&safe)
    }

    /// 渲染并清空剩余缓冲
    pub(crate) fn flush(&mut self, renderer: &Renderer) -> String {
        if self.pending.trim().is_empty() {
            self.pending.clear();
            return String::new();
        }
        let out = renderer.render(&self.pending);
        self.pending.clear();
        out
    }
}

/// 解析行首的围栏开启标记，返回 `FenceMarker`，如果不是围栏行则返回 `None`。
fn parse_fence_opener(line: &str) -> Option<FenceMarker> {
    let trimmed = line.trim_start();
    let ch = trimmed.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let count = trimmed.chars().take_while(|&c| c == ch).count();
    if count < 3 {
        return None;
    }
    // 围栏后面只能是空白或语言标识符（不能跟同字符）
    let after: String = trimmed[count..]
        .chars()
        .take_while(|&c| c != '\n' && c != '\r')
        .collect();
    if after.chars().any(|c| c == ch) {
        return None; // 混杂字符，不是纯围栏
    }
    Some(FenceMarker {
        character: ch,
        length: count,
    })
}

/// 检测嵌套代码围栏并自动扩展外层围栏。
///
/// LLM 可能在代码块内输出另一个 markdown 代码块（如示例文档），
/// 导致流式渲染器提前关闭外层代码块。此函数检测这种情况并加长外层围栏。
///
/// 注意：此函数假设传入文本是已提交的"安全"块（不在未闭合围栏内）。
fn normalize_nested_fences(text: &str) -> String {
    // 收集所有围栏标记
    let lines: Vec<&str> = text.lines().collect();
    let mut fences: Vec<(usize, FenceMarker, bool)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some(fm) = parse_fence_opener(line) {
            // 判断是开还是关：寻找栈顶匹配的
            let is_close = fences.iter().rev().any(|(_, f, is_open)| {
                *is_open && f.character == fm.character && f.length == fm.length
            });
            fences.push((i, fm, !is_close));
        }
    }

    if fences.len() < 2 {
        return text.to_string();
    }

    // 找出最大嵌套深度和所需最大围栏长度
    let mut max_depth = 0usize;
    let mut depth = 0usize;
    let mut max_len = 3usize;
    for (_, fm, is_open) in &fences {
        if *is_open {
            depth += 1;
            max_depth = max_depth.max(depth);
            max_len = max_len.max(fm.length);
        } else {
            depth = depth.saturating_sub(1);
        }
    }

    // 如果最大深度 > 1（有嵌套），需要加长外层围栏
    if max_depth <= 1 {
        return text.to_string();
    }

    // 按嵌套深度分配围栏长度：外层比内层长，保证内层关闭围栏不会
    // 误关外层。depth=1（最外层）最长，越往里越短，最里层保持原始长度
    // （至少 3）。把「开/关」配对：用栈记录每个开围栏所在的深度，关围栏
    // 沿用同一深度，从而成对加长。
    let target_len = |depth: usize| -> usize {
        // 最外层 depth=1 → max_len + max_depth - 1；最内层 depth=max_depth → max_len
        max_len + (max_depth - depth)
    };

    let mut result = String::with_capacity(text.len() + fences.len() * 3);
    let mut prev_line = 0usize;
    let mut depth_stack: Vec<usize> = Vec::new();
    let mut cur_depth = 0usize;

    for (li, fm, is_open) in &fences {
        let this_depth = if *is_open {
            cur_depth += 1;
            depth_stack.push(cur_depth);
            cur_depth
        } else {
            let d = depth_stack.pop().unwrap_or(cur_depth);
            cur_depth = cur_depth.saturating_sub(1);
            d
        };
        let want = target_len(this_depth);

        // 推入此围栏行之前的普通内容
        for l in &lines[prev_line..*li] {
            result.push_str(l);
            result.push('\n');
        }

        if fm.length != want {
            let old_fence: String = std::iter::repeat_n(fm.character, fm.length).collect();
            let new_fence: String = std::iter::repeat_n(fm.character, want).collect();
            let line = lines[*li].replacen(&old_fence, &new_fence, 1);
            result.push_str(&line);
        } else {
            result.push_str(lines[*li]);
        }
        result.push('\n');
        prev_line = *li + 1;
    }
    // 推入剩余的行
    for l in &lines[prev_line..] {
        result.push_str(l);
        result.push('\n');
    }

    result
}

/// 返回可安全提交的字节位置：最后一个"不在未闭合代码围栏内"的空行结束处。
///
/// 升级版：使用 `FenceMarker` 区分 backtick 和 tilde 围栏，
/// 避免 ``` 错误关闭 ~~~ 围栏（或反之）。
fn find_stream_safe_boundary(s: &str) -> usize {
    let mut fence_stack: Vec<FenceMarker> = Vec::new();
    let mut safe_idx = 0usize;
    let mut idx = 0usize;
    for line in s.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        idx += line.len();

        if let Some(fm) = parse_fence_opener(trimmed.trim_start()) {
            // 检查是否匹配栈顶
            if let Some(top) = fence_stack.last() {
                if top.character == fm.character && top.length == fm.length {
                    fence_stack.pop(); // 关闭围栏
                } else {
                    fence_stack.push(fm); // 不同围栏，嵌套
                }
            } else {
                fence_stack.push(fm); // 开启围栏
            }
            continue;
        }

        if trimmed.trim().is_empty() && fence_stack.is_empty() {
            safe_idx = idx;
        }
    }
    safe_idx
}
