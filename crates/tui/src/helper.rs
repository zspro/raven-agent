//! rustyline Helper：括号匹配高亮与校验（已移除命令补全与历史提示）。
//! 另含 Ctrl+O 展开折叠的事件处理器 `ExpandHandler`。

use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::{CmdKind, Highlighter, MatchingBracketHighlighter};
use rustyline::hint::Hinter;
use rustyline::validate::{MatchingBracketValidator, ValidationResult, Validator};
use rustyline::{Cmd, ConditionalEventHandler, Context, Event, EventContext, Helper, RepeatCount};
use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Ctrl+O 事件处理器：仅在输入行为空时触发——设置共享标志并提交空行，
/// 让主循环 readline 返回后识别标志、重打上一轮工具的完整输出。
///
/// 行内有文本时返回 `None`（交还默认行为，避免吞掉用户正在输入的内容）。
pub(crate) struct ExpandHandler {
    flag: Arc<AtomicBool>,
}

impl ExpandHandler {
    pub(crate) fn new(flag: Arc<AtomicBool>) -> Self {
        Self { flag }
    }
}

impl ConditionalEventHandler for ExpandHandler {
    fn handle(
        &self,
        _evt: &Event,
        _n: RepeatCount,
        _positive: bool,
        ctx: &EventContext,
    ) -> Option<Cmd> {
        if ctx.line().is_empty() {
            self.flag.store(true, Ordering::Relaxed);
            Some(Cmd::AcceptLine)
        } else {
            None
        }
    }
}

pub(crate) struct ReplHelper {
    bracket: MatchingBracketHighlighter,
    validator: MatchingBracketValidator,
}

impl ReplHelper {
    pub(crate) fn new() -> Self {
        Self {
            bracket: MatchingBracketHighlighter::new(),
            validator: MatchingBracketValidator::new(),
        }
    }
}

impl Helper for ReplHelper {}

impl Completer for ReplHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        _: &str,
        _: usize,
        _: &Context<'_>,
    ) -> Result<(usize, Vec<Pair>), ReadlineError> {
        Ok((0, vec![]))
    }
}

impl Hinter for ReplHelper {
    type Hint = String;
    fn hint(&self, _: &str, _: usize, _: &Context<'_>) -> Option<String> {
        None
    }
}

impl Highlighter for ReplHelper {
    fn highlight<'l>(&self, line: &'l str, _: usize) -> Cow<'l, str> {
        Cow::Borrowed(line)
    }
    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(&'s self, p: &'p str, _: bool) -> Cow<'b, str> {
        Cow::Borrowed(p)
    }
    fn highlight_char(&self, line: &str, pos: usize, kind: CmdKind) -> bool {
        self.bracket.highlight_char(line, pos, kind)
    }
}

impl Validator for ReplHelper {
    fn validate(
        &self,
        ctx: &mut rustyline::validate::ValidationContext,
    ) -> Result<ValidationResult, ReadlineError> {
        self.validator.validate(ctx)
    }
    fn validate_while_typing(&self) -> bool {
        self.validator.validate_while_typing()
    }
}
