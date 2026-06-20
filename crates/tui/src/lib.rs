//! # TUI - Rustyline REPL + Markdown 渲染
//!
//! 架构：
//! - 入口：独立 OS 线程上创建 tokio runtime（避免嵌套 runtime panic）
//! - 输入：rustyline（自动处理中文、光标、粘贴、历史、Ctrl+C）
//! - 输出：pulldown-cmark + syntect 渲染 Markdown（表格、代码高亮、粗体、引用）
//! - 流式：延迟提交（参考 claw-code）——文本边到边渲染已"安全"的块（空行分隔的
//!   段落、闭合的代码围栏），未闭合部分留在缓冲区，流结束时 flush。已提交内容不重绘。
//! - 中断：生成期间 Ctrl+C 只中断当前轮（drop 流），不退出 REPL。
//! - 信息栏：每轮结束打印模型 / token / 权限模式。
//! - 循环：同步 REPL（rustyline.readline）+ 本地 tokio runtime 做 async
//!
//! 启动: raven tui

use raven_core::Agent;
use pulldown_cmark::{Options, Parser, Tag, TagEnd, Event};
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::{Highlighter, MatchingBracketHighlighter, CmdKind};
use rustyline::history::DefaultHistory;
use rustyline::hint::{Hinter, HistoryHinter};
use rustyline::validate::{MatchingBracketValidator, ValidationResult, Validator};
use rustyline::{CompletionType, Config, Context, EditMode, Editor, Helper};
use std::borrow::Cow;
use std::io::Write;
use std::sync::Arc;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

// =============================================================================
// 颜色主题 — 集中管理所有 ANSI 颜色，一处修改全局生效
// =============================================================================

struct ColorTheme;

impl ColorTheme {
    const DIM: &'static str = "\x1b[90m";
    const RESET: &'static str = "\x1b[0m";
    const BOLD: &'static str = "\x1b[1m";
    const ITALIC: &'static str = "\x1b[3m";
    const UNDERLINE: &'static str = "\x1b[4m";
    const ACCENT: &'static str = "\x1b[36m";      // 青色：工具调用、引用、标题
    const ERROR: &'static str = "\x1b[31m";       // 红色：错误
    const SUCCESS: &'static str = "\x1b[32m";     // 绿色：成功
    const CODE_INLINE: &'static str = "\x1b[33m"; // 黄色：行内代码
    const BULLET: &'static str = "\x1b[33m";      // 黄色：列表符号
    const HEADING_H1: &'static str = "\x1b[1;36m";
    const HEADING_H2: &'static str = "\x1b[1;34m";
    const HEADING_H3: &'static str = "\x1b[1;35m";
    const QUOTE: &'static str = "\x1b[36m";
    const STRIKETHROUGH: &'static str = "\x1b[9m";
    const CODE_BG: &'static str = "\x1b[48;5;236m"; // 代码块深色背景
    const LINK: &'static str = "\x1b[34m";         // 蓝色：链接
}

// =============================================================================
// Spinner — braille 旋转动画帧
// =============================================================================

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// 根据索引返回 spinner 帧字符
fn spinner_frame(idx: u32) -> &'static str {
    SPINNER_FRAMES[(idx as usize) % SPINNER_FRAMES.len()]
}

// =============================================================================
// 围栏标记 — 用于流式渲染中追踪未闭合代码围栏
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FenceMarker {
    /// 围栏字符：'`' (backtick) 或 '~' (tilde)
    character: char,
    /// 围栏长度（最少 3）
    length: usize,
}

// =============================================================================
// 入口
// =============================================================================

pub fn run(agent: Arc<Agent>, opening: String) -> anyhow::Result<()> {
    // 确保 Windows 旧版控制台也能渲染 ANSI 颜色（非 Windows 为空操作）。
    config_system::platform::enable_ansi_support();
    // 必须在独立 OS 线程上运行 REPL。#[tokio::main] 已创建 runtime，
    // 无论用 Handle::block_on 还是 spawn_blocking 都会冲突。
    // 唯一安全方案：新线程 → 新 runtime → block_on。
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new()?;
        let mut repl = Repl::new(agent, rt);
        repl.run(opening)
    });

    handle.join().map_err(|e| anyhow::anyhow!("TUI 线程 panic: {e:?}"))?
}

// =============================================================================
// Repl — 核心结构，持有所有状态
// =============================================================================

struct Repl {
    agent: Arc<Agent>,
    rt: tokio::runtime::Runtime,
    editor: Editor<ReplHelper, DefaultHistory>,
    renderer: Renderer,
}

impl Repl {
    fn new(agent: Arc<Agent>, rt: tokio::runtime::Runtime) -> Self {
        let config = Config::builder()
            .completion_type(CompletionType::List)
            .edit_mode(EditMode::Emacs)
            .build();

        let mut editor = Editor::with_config(config).expect("初始化 rustyline 失败");
        editor.set_helper(Some(ReplHelper::new()));

        // 加载历史
        let history_path = Self::history_path();
        let _ = editor.load_history(&history_path);

        Self {
            agent,
            rt,
            editor,
            renderer: Renderer::new(),
        }
    }

    fn run(&mut self, opening: String) -> anyhow::Result<()> {
        self.print_welcome();

        // 开场白：直接提交，不进 readline
        let opening = opening.trim().to_string();
        if !opening.is_empty() {
            self.execute_turn(&opening);
        }

        // REPL 主循环
        loop {
            let line = match self.editor.readline(">>> ") {
                Ok(l) => l,
                Err(ReadlineError::Interrupted) => {
                    println!("(Ctrl+C)\n");
                    break;
                }
                Err(ReadlineError::Eof) => {
                    println!("(Ctrl+D)\n");
                    break;
                }
                Err(e) => {
                    eprintln!("错误: {e}");
                    break;
                }
            };

            let input = line.trim().to_string();
            if input.is_empty() {
                continue;
            }
            let _ = self.editor.add_history_entry(&input);

            // 斜杠命令
            if input.starts_with('/') {
                if self.handle_command(&input) {
                    break; // /quit
                }
                continue;
            }

            self.execute_turn(&input);
        }

        let _ = self.editor.save_history(&Self::history_path());
        Ok(())
    }

    /// 执行一轮对话：流式渲染（延迟提交）+ 工具美化 + Ctrl+C 中断
    fn execute_turn(&mut self, input: &str) {
        let rx = match self.rt.block_on(self.agent.run_stream(input)) {
            Ok(rx) => rx,
            Err(e) => {
                println!("{err}错误: {e}{rst}\n", err = ColorTheme::ERROR, rst = ColorTheme::RESET);
                return;
            }
        };

        let mut rx = rx;
        let mut stream = MarkdownStreamState::new();
        let mut tool_count = 0u32;
        let mut had_text = false;
        let mut interrupted = false;

        loop {
            // 在生成期间，Ctrl+C 只中断当前轮（不退出 REPL）
            let next = self.rt.block_on(async {
                tokio::select! {
                    ev = rx.recv() => Ok(ev),
                    _ = tokio::signal::ctrl_c() => Err(()),
                }
            });

            let event = match next {
                Ok(Some(ev)) => ev,
                Ok(None) => break,
                Err(()) => {
                    interrupted = true;
                    break;
                }
            };

            match event.event_type.as_str() {
                "text" => {
                    if let Some(ref t) = event.content {
                        had_text = true;
                        // 边到边渲染已"安全"的部分
                        let committed = stream.push(t, &self.renderer);
                        if !committed.is_empty() {
                            print!("{committed}");
                            let _ = std::io::stdout().flush();
                        }
                    }
                }
                "tool_call" => {
                    // 工具调用前先 flush 已缓冲的文本
                    let rest = stream.flush(&self.renderer);
                    if !rest.is_empty() {
                        print!("{rest}");
                    }
                    let (name, args) = parse_tool_call(event.content.as_deref());
                    print!("{}", render_tool_call(&name, &args, tool_count));
                    let _ = std::io::stdout().flush();
                    tool_count += 1;
                }
                "tool_result" => {
                    let (name, output, is_error) = parse_tool_result(event.content.as_deref());
                    print!("{}", render_tool_result(&name, &output, is_error));
                    let _ = std::io::stdout().flush();
                }
                "error" => {
                    let rest = stream.flush(&self.renderer);
                    if !rest.is_empty() {
                        print!("{rest}");
                    }
                    let msg = event.content.unwrap_or_default();
                    println!("{err}错误: {msg}{rst}\n", err = ColorTheme::ERROR, rst = ColorTheme::RESET);
                    return;
                }
                "done" => {}
                _ => {}
            }
        }

        // flush 缓冲区剩余内容
        let rest = stream.flush(&self.renderer);
        if !rest.is_empty() {
            print!("{rest}");
        }

        if interrupted {
            // 主动 drop 接收端，让上游 spawn 任务自然结束
            drop(rx);
            println!("\n{dim}  (已中断){rst}", dim = ColorTheme::DIM, rst = ColorTheme::RESET);
        } else if !had_text && tool_count == 0 {
            println!("{dim}  (空回复){rst}", dim = ColorTheme::DIM, rst = ColorTheme::RESET);
        } else {
            println!();
        }

        self.print_info_bar();
    }

    /// 会话信息栏：模型 · 权限模式 · token 用量
    fn print_info_bar(&self) {
        let stats = self.rt.block_on(self.agent.stats());
        let total = stats.total_input_tokens + stats.total_output_tokens;
        println!(
            "{dim}  🐦‍⬛ {model} · {mode} · ctx {ctx} · {inp}↑/{out}↓ ({total} tokens){rst}\n",
            dim = ColorTheme::DIM,
            rst = ColorTheme::RESET,
            model = self.agent.config().model,
            mode = self.agent.permission_mode(),
            ctx = stats.current_context_tokens,
            inp = stats.total_input_tokens,
            out = stats.total_output_tokens,
        );
    }

    /// 处理斜杠命令，返回 true 表示应退出
    fn handle_command(&mut self, cmd: &str) -> bool {
        let dim = ColorTheme::DIM;
        let rst = ColorTheme::RESET;
        let ok = ColorTheme::SUCCESS;
        let err = ColorTheme::ERROR;
        match cmd {
            "/quit" | "/exit" | "/q" => true,
            "/clear" => {
                self.rt.block_on(self.agent.clear());
                println!("{ok}✓ 会话已清空{rst}\n");
                false
            }
            "/compact" => {
                match self.rt.block_on(self.agent.compact()) {
                    Ok(_) => {
                        let stats = self.rt.block_on(self.agent.stats());
                        println!("{ok}✓ 已压缩（{} in / {} out）{rst}\n",
                            stats.total_input_tokens, stats.total_output_tokens);
                    }
                    Err(e) => println!("{err}✗ 压缩失败: {e}{rst}\n"),
                }
                false
            }
            "/cost" => {
                let stats = self.rt.block_on(self.agent.stats());
                println!("{dim}  in: {} | out: {} | total: {}{rst}\n",
                    stats.total_input_tokens,
                    stats.total_output_tokens,
                    stats.total_input_tokens + stats.total_output_tokens);
                false
            }
            "/help" => {
                println!("{dim}/quit 退出 · /clear 清空 · /compact 压缩 · /cost 统计 · 生成中 Ctrl+C 中断本轮{rst}\n");
                false
            }
            _ => {
                println!("{err}未知命令: {cmd}{rst}\n");
                false
            }
        }
    }

    fn history_path() -> std::path::PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".raven")
            .join("tui_history")
    }

    fn print_welcome(&self) {
        let cfg = self.agent.config();
        let bold = ColorTheme::BOLD;
        let accent = ColorTheme::ACCENT;
        let dim = ColorTheme::DIM;
        let rst = ColorTheme::RESET;

        // 乌鸦 ASCII 标志 —— Think like a raven. Code like the wind.
        let art = [
            "      ___",
            "     (o,o)    ",
            "    {  \"  }   ",
            "  ---\"-\"---   ",
        ];
        let tag = [
            "",
            "Raven 🐦‍⬛",
            "Think like a raven.",
            "Code like the wind.",
        ];
        println!();
        for (a, t) in art.iter().zip(tag.iter()) {
            if t.is_empty() {
                println!("  {accent}{a}{rst}");
            } else if t.starts_with("Raven") {
                println!("  {accent}{a}{rst}  {bold}{accent}{t}{rst}");
            } else {
                println!("  {accent}{a}{rst}  {dim}{t}{rst}");
            }
        }
        println!();
        println!(
            "  {dim}模型{rst} {model}   {dim}模式{rst} {mode}",
            model = cfg.model,
            mode = self.agent.permission_mode(),
        );
        println!("  {dim}/help 查看命令 · Ctrl+C 中断生成 · Ctrl+D 退出{rst}\n");
    }
}

// =============================================================================
// Markdown → ANSI 渲染
// =============================================================================

/// 预处理：HTML `<sub>`/`<sup>` + LaTeX `$...$`/`$$...$$` 数学 → Unicode 上下标
///
/// 化学方程式示例：
///   H<sub>2</sub>O            → H₂O
///   $2H_2 + O_2 \rightarrow 2H_2O$ → 2H₂ + O₂ → 2H₂O
fn preprocess_chemistry(text: &str) -> String {
    // 第一步：HTML 标签
    let text = preprocess_html_sub_sup(text);
    // 第二步：LaTeX 数学（$...$ 内部）
    let text = preprocess_latex_math(&text);
    // 第三步：独立 \command（$...$ 外面的 \boxed{...}, \quad, \; 等）
    preprocess_standalone_commands(&text)
}

/// 处理 $...$ 外部的独立 LaTeX 命令（\boxed, \quad, \; 等）
fn preprocess_standalone_commands(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            let next = chars[i + 1];
            // 空格类: \  \quad \qquad \; \, \: \!
            if next == ' ' || matches!(next, ';' | ',' | ':' | '!') {
                out.push(' ');
                i += 2;
                continue;
            }
            if next.is_alphabetic() {
                let cmd_start = i + 1;
                let mut j = cmd_start;
                while j < chars.len() && chars[j].is_alphabetic() { j += 1; }
                let cmd: String = chars[cmd_start..j].iter().collect();

                match cmd.as_str() {
                    // 空格类
                    "quad" | "qquad" => {
                        out.push(' ');
                        i = j;
                        continue;
                    }
                    // 无操作（忽略）
                    "displaystyle" | "textstyle" | "scriptstyle" => {
                        i = j;
                        continue;
                    }
                    // \boxed{...} → 提取内容
                    "boxed" => {
                        if j < chars.len() && chars[j] == '{' {
                            let mut depth = 1u32;
                            let mut k = j + 1;
                            while k < chars.len() && depth > 0 {
                                if chars[k] == '{' { depth += 1; }
                                if chars[k] == '}' { depth -= 1; }
                                if depth > 0 { k += 1; }
                            }
                            let inner: String = chars[j + 1..k].iter().collect();
                            // 递归处理内部内容
                            let processed = preprocess_standalone_commands(&inner);
                            out.push_str(&processed);
                            i = k + 1; // skip }
                            continue;
                        }
                        i = j;
                        continue;
                    }
                    // 文本模式: \text{...}, \mathrm{...} → 提取内容
                    "text" | "mathrm" | "mathbf" | "mathcal" | "mathit" | "mathsf" | "mathtt" => {
                        if j < chars.len() && chars[j] == '{' {
                            let mut depth = 1u32;
                            let mut k = j + 1;
                            while k < chars.len() && depth > 0 {
                                if chars[k] == '{' { depth += 1; }
                                if chars[k] == '}' { depth -= 1; }
                                if depth > 0 { k += 1; }
                            }
                            let inner: String = chars[j + 1..k].iter().collect();
                            let processed = preprocess_standalone_commands(&inner);
                            out.push_str(&processed);
                            i = k + 1;
                            continue;
                        }
                        i = j;
                        continue;
                    }
                    _ => {
                        // 未知命令，保留原样
                        out.push('\\');
                        out.push_str(&cmd);
                        i = j;
                        continue;
                    }
                }
            }
            // 非字母非空格的 \X → 原样保留
            out.push(chars[i]);
            i += 1;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn preprocess_html_sub_sup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 5 <= bytes.len() && &bytes[i..i+5] == b"<sub>" {
            i += 5;
            let start = i;
            while i + 6 <= bytes.len() && &bytes[i..i+6] != b"</sub>" {
                i += 1;
            }
            let content = std::str::from_utf8(&bytes[start..i]).unwrap_or("");
            if all_subscriptable(content) {
                out.push_str(&sub_to_unicode(content));
            } else {
                out.push_str(&format!("_({content})"));
            }
            if i + 6 <= bytes.len() { i += 6; }
        } else if i + 5 <= bytes.len() && &bytes[i..i+5] == b"<sup>" {
            i += 5;
            let start = i;
            while i + 6 <= bytes.len() && &bytes[i..i+6] != b"</sup>" {
                i += 1;
            }
            let content = std::str::from_utf8(&bytes[start..i]).unwrap_or("");
            if all_superscriptable(content) {
                out.push_str(&sup_to_unicode(content));
            } else {
                out.push_str(&format!("^({content})"));
            }
            if i + 6 <= bytes.len() { i += 6; }
        } else {
            let ch = text[i..].chars().next().unwrap_or('\0');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// 扫描 `$...$` / `$$...$$` / `\(...\)` / `\[...\]` 并转换 LaTeX 数学 → Unicode
fn preprocess_latex_math(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // 检测 \[ 块 (LaTeX display math)
        if i + 2 <= len && &bytes[i..i+2] == b"\\[" {
            let start = i + 2;
            if let Some(end) = text[start..].find("\\]") {
                let math = &text[start..start + end];
                out.push_str(&latex_to_unicode(math));
                i = start + end + 2;
                continue;
            }
        }
        // 检测 \( 行内 (LaTeX inline math)
        if i + 2 <= len && &bytes[i..i+2] == b"\\(" {
            let start = i + 2;
            if let Some(end) = text[start..].find("\\)") {
                let math = &text[start..start + end];
                out.push_str(&latex_to_unicode(math));
                i = start + end + 2;
                continue;
            }
        }
        // 检测 $$ 块
        if i + 2 <= len && &bytes[i..i+2] == b"$$" {
            // 跳过 `$$` 分隔符，找到匹配的 `$$`
            let start = i + 2;
            if let Some(end) = text[start..].find("$$") {
                let math = &text[start..start + end];
                out.push_str(&latex_to_unicode(math));
                i = start + end + 2;
                continue;
            }
        }
        // 检测 $ 行内（反斜杠保护 \$ 不算）
        if bytes[i] == b'$' && (i == 0 || bytes[i - 1] != b'\\') {
            // 跳过 $, 找到匹配的 $
            let start = i + 1;
            if let Some(end) = text[start..].find('$') {
                // 过滤货币: "$5" 或 "$10.50" — 数字紧跟 $ 不算数学
                let math = &text[start..start + end];
                if !math.is_empty() && !math.starts_with(|c: char| c.is_ascii_digit()) {
                    out.push_str(&latex_to_unicode(math));
                    i = start + end + 1;
                    continue;
                }
            }
        }
        let ch = text[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 精简版 LaTeX → Unicode（对齐 oh-my-pi 的 latex-to-unicode.ts）
fn latex_to_unicode(math: &str) -> String {
    let mut out = String::with_capacity(math.len());
    let chars: Vec<char> = math.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        match c {
            // 下标: _ 后跟单字符 或 _{...}（不可映射时 fallback _(...)）
            '_' => {
                i += 1;
                if i < chars.len() && chars[i] == '{' {
                    i += 1;
                    let content = extract_brace_group(&chars, &mut i);
                    let inner = latex_to_unicode(&content);
                    if all_subscriptable(&inner) {
                        out.push_str(&sub_to_unicode(&inner));
                    } else {
                        out.push_str(&format!("_({inner})"));
                    }
                    i += 1;
                } else if i < chars.len() {
                    let ch = chars[i];
                    let inner = latex_to_unicode(&ch.to_string());
                    if all_subscriptable(&inner) {
                        out.push_str(&sub_to_unicode(&inner));
                    } else {
                        out.push_str(&format!("_({inner})"));
                    }
                    i += 1;
                }
            }
            // 上标: ^ 后跟单字符 或 ^{...}（不可映射时 fallback ^(...)）
            '^' => {
                i += 1;
                if i < chars.len() && chars[i] == '{' {
                    i += 1;
                    let content = extract_brace_group(&chars, &mut i);
                    let inner = latex_to_unicode(&content);
                    if all_superscriptable(&inner) {
                        out.push_str(&sup_to_unicode(&inner));
                    } else {
                        out.push_str(&format!("^({inner})"));
                    }
                    i += 1;
                } else if i < chars.len() {
                    let ch = chars[i];
                    let inner = latex_to_unicode(&ch.to_string());
                    if all_superscriptable(&inner) {
                        out.push_str(&sup_to_unicode(&inner));
                    } else {
                        out.push_str(&format!("^({inner})"));
                    }
                    i += 1;
                }
            }
            // 反斜杠命令
            '\\' => {
                i += 1;
                let cmd_start = i;
                while i < chars.len() && chars[i].is_alphabetic() { i += 1; }
                let cmd: String = chars[cmd_start..i].iter().collect();

                // 空命令（\; \, \: \! \ 等）→ 空格
                if cmd.is_empty() {
                    out.push(' ');
                    // 跳过空格类字符: 空格本身 \; \, \: \!  → 都已消费
                    if i < chars.len() && matches!(chars[i], ';' | ',' | ':' | '!') {
                        i += 1;
                    }
                    continue;
                }

                match cmd.as_str() {
                    // 箭头
                    "rightarrow" | "to" | "longrightarrow" => out.push('\u{2192}'),
                    "leftarrow" | "longleftarrow" => out.push('\u{2190}'),
                    "leftrightarrow" | "longleftrightarrow" => out.push('\u{2194}'),
                    "Rightarrow" | "Longrightarrow" => out.push('\u{21D2}'),
                    "Leftarrow" | "Longleftarrow" => out.push('\u{21D0}'),
                    "uparrow" => out.push('\u{2191}'),
                    "downarrow" => out.push('\u{2193}'),
                    "rightleftharpoons" => out.push('\u{21CC}'),
                    // 带文字箭头: \xrightarrow{text} → →(text)
                    "xrightarrow" | "xleftarrow" | "xleftrightarrow" => {
                        let arrow = match cmd.as_str() {
                            "xrightarrow" => '\u{2192}',
                            "xleftarrow" => '\u{2190}',
                            _ => '\u{2194}',
                        };
                        if i < chars.len() && chars[i] == '{' {
                            i += 1;
                            let text = extract_brace_group(&chars, &mut i);
                            out.push_str(&format!("\u{002D}{text}\u{2192}"));
                            i += 1;
                        } else {
                            out.push(arrow);
                        }
                    }
                    // 希腊字母
                    "Delta" => out.push('\u{0394}'),
                    "Gamma" => out.push('\u{0393}'),
                    "alpha" => out.push('\u{03B1}'),
                    "beta" => out.push('\u{03B2}'),
                    "gamma" => out.push('\u{03B3}'),
                    "delta" => out.push('\u{03B4}'),
                    "epsilon" | "varepsilon" => out.push('\u{03B5}'),
                    "zeta" => out.push('\u{03B6}'),
                    "eta" => out.push('\u{03B7}'),
                    "theta" => out.push('\u{03B8}'),
                    "lambda" => out.push('\u{03BB}'),
                    "mu" => out.push('\u{03BC}'),
                    "nu" => out.push('\u{03BD}'),
                    "xi" => out.push('\u{03BE}'),
                    "pi" => out.push('\u{03C0}'),
                    "rho" => out.push('\u{03C1}'),
                    "sigma" => out.push('\u{03C3}'),
                    "tau" => out.push('\u{03C4}'),
                    "phi" | "varphi" => out.push('\u{03C6}'),
                    "omega" => out.push('\u{03C9}'),
                    // 运算符
                    "times" => out.push('\u{00D7}'),
                    "cdot" => out.push('\u{22C5}'),
                    "pm" => out.push('\u{00B1}'),
                    "mp" => out.push('\u{2213}'),
                    "div" => out.push('\u{00F7}'),
                    "infty" => out.push('\u{221E}'),
                    "approx" => out.push('\u{2248}'),
                    "equiv" => out.push('\u{2261}'),
                    "neq" | "ne" => out.push('\u{2260}'),
                    "leq" | "le" => out.push('\u{2264}'),
                    "geq" | "ge" => out.push('\u{2265}'),
                    "ll" => out.push('\u{226A}'),
                    "gg" => out.push('\u{226B}'),
                    "sim" => out.push('\u{223C}'),
                    "propto" => out.push('\u{221D}'),
                    "partial" => out.push('\u{2202}'),
                    "nabla" => out.push('\u{2207}'),
                    "sum" => out.push('\u{2211}'),
                    "prod" => out.push('\u{220F}'),
                    "int" => out.push('\u{222B}'),
                    "oint" => out.push('\u{222E}'),
                    "sqrt" => out.push('\u{221A}'),
                    "degree" | "circ" => out.push('\u{00B0}'),
                    // 文本模式: \mathrm, \text, \mathbf → 纯文本
                    "mathrm" | "text" | "mathbf" | "mathcal" | "mathit" | "mathsf" | "mathtt" => {
                        if i < chars.len() && chars[i] == '{' {
                            i += 1;
                            let content = extract_brace_group(&chars, &mut i);
                            out.push_str(&latex_to_unicode(&content));
                            i += 1;
                        }
                    }
                    // 分数: \frac{a}{b} → (a)/(b)  或  用 Unicode 分割线
                    "frac" => {
                        if i < chars.len() && chars[i] == '{' {
                            i += 1;
                            let num = extract_brace_group(&chars, &mut i);
                            i += 1; // skip }
                            if i < chars.len() && chars[i] == '{' {
                                i += 1;
                                let den = extract_brace_group(&chars, &mut i);
                                out.push_str(&format!("({})/({})",
                                    latex_to_unicode(&num), latex_to_unicode(&den)));
                                i += 1;
                            } else {
                                out.push_str(&latex_to_unicode(&num));
                            }
                        }
                    }
                    // 极限: \lim → lim
                    "lim" => out.push_str("lim"),
                    // 空格类
                    "quad" | "qquad" => out.push(' '),
                    // 无操作（忽略）
                    "displaystyle" | "textstyle" | "scriptstyle" => {},
                    // \boxed{...} → 提取内容（数学模式内）
                    "boxed" => {
                        if i < chars.len() && chars[i] == '{' {
                            i += 1;
                            let content = extract_brace_group(&chars, &mut i);
                            out.push_str(&latex_to_unicode(&content));
                            i += 1;
                        }
                    }
                    // 换行: \\ → 换行（只在数学模式内部，align 等）
                    // 未知命令 → 去掉反斜杠，保留命令名
                    _ => {
                        out.push_str(&cmd);
                    }
                }
            }
            // 花括号 — 数学模式中裸花括号跳过（已由分数/文本等处理）
            '{' | '}' => { i += 1; }
            // 其他字符
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }

    out
}

/// 从 chars[i..] 提取花括号组内容，i 前进到对应的 '}'
fn extract_brace_group(chars: &[char], i: &mut usize) -> String {
    let start = *i;
    let mut depth = 1usize;
    while *i < chars.len() && depth > 0 {
        if chars[*i] == '{' { depth += 1; }
        if chars[*i] == '}' { depth -= 1; }
        if depth > 0 { *i += 1; }
    }
    chars[start..*i].iter().collect()
}

fn sub_to_unicode(s: &str) -> String {
    s.chars().map(|c| match c {
        '0' => '₀','1' => '₁','2' => '₂','3' => '₃','4' => '₄',
        '5' => '₅','6' => '₆','7' => '₇','8' => '₈','9' => '₉',
        'a' => 'ₐ','e' => 'ₑ','h' => 'ₕ','i' => 'ᵢ','j' => 'ⱼ',
        'k' => 'ₖ','l' => 'ₗ','m' => 'ₘ','n' => 'ₙ','o' => 'ₒ',
        'p' => 'ₚ','r' => 'ᵣ','s' => 'ₛ','t' => 'ₜ','u' => 'ᵤ',
        'v' => 'ᵥ','x' => 'ₓ',
        '+' => '₊','-' => '₋','=' => '₌','(' => '₍',')' => '₎',
        _ => c,
    }).collect()
}

fn sup_to_unicode(s: &str) -> String {
    s.chars().map(|c| match c {
        '0' => '⁰','1' => '¹','2' => '²','3' => '³','4' => '⁴',
        '5' => '⁵','6' => '⁶','7' => '⁷','8' => '⁸','9' => '⁹',
        'a' => 'ᵃ','b' => 'ᵇ','c' => 'ᶜ','d' => 'ᵈ','e' => 'ᵉ',
        'f' => 'ᶠ','g' => 'ᵍ','h' => 'ʰ','i' => 'ⁱ','j' => 'ʲ',
        'k' => 'ᵏ','l' => 'ˡ','m' => 'ᵐ','n' => 'ⁿ','o' => 'ᵒ',
        'p' => 'ᵖ','r' => 'ʳ','s' => 'ˢ','t' => 'ᵗ','u' => 'ᵘ',
        'v' => 'ᵛ','w' => 'ʷ','x' => 'ˣ','y' => 'ʸ','z' => 'ᶻ',
        '+' => '⁺','-' => '⁻','=' => '⁼','(' => '⁽',')' => '⁾',
        _ => c,
    }).collect()
}

/// 字符串中所有字符是否均可转为下标/上标
fn all_subscriptable(s: &str) -> bool { s.chars().all(|c| sub_to_unicode_char(c) != c || c == ' ') }
fn all_superscriptable(s: &str) -> bool { s.chars().all(|c| sup_to_unicode_char(c) != c || c == ' ') }

fn sub_to_unicode_char(c: char) -> char {
    match c {
        '0' => '₀','1' => '₁','2' => '₂','3' => '₃','4' => '₄',
        '5' => '₅','6' => '₆','7' => '₇','8' => '₈','9' => '₉',
        'a' => 'ₐ','e' => 'ₑ','h' => 'ₕ','i' => 'ᵢ','j' => 'ⱼ',
        'k' => 'ₖ','l' => 'ₗ','m' => 'ₘ','n' => 'ₙ','o' => 'ₒ',
        'p' => 'ₚ','r' => 'ᵣ','s' => 'ₛ','t' => 'ₜ','u' => 'ᵤ',
        'v' => 'ᵥ','x' => 'ₓ',
        '+' => '₊','-' => '₋','=' => '₌','(' => '₍',')' => '₎',
        _ => c,
    }
}

fn sup_to_unicode_char(c: char) -> char {
    match c {
        '0' => '⁰','1' => '¹','2' => '²','3' => '³','4' => '⁴',
        '5' => '⁵','6' => '⁶','7' => '⁷','8' => '⁸','9' => '⁹',
        'a' => 'ᵃ','b' => 'ᵇ','c' => 'ᶜ','d' => 'ᵈ','e' => 'ᵉ',
        'f' => 'ᶠ','g' => 'ᵍ','h' => 'ʰ','i' => 'ⁱ','j' => 'ʲ',
        'k' => 'ᵏ','l' => 'ˡ','m' => 'ᵐ','n' => 'ⁿ','o' => 'ᵒ',
        'p' => 'ᵖ','r' => 'ʳ','s' => 'ˢ','t' => 'ᵗ','u' => 'ᵘ',
        'v' => 'ᵛ','w' => 'ʷ','x' => 'ˣ','y' => 'ʸ','z' => 'ᶻ',
        '+' => '⁺','-' => '⁻','=' => '⁼','(' => '⁽',')' => '⁾',
        _ => c,
    }
}

struct Renderer {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
}

impl Renderer {
    fn new() -> Self {
        Self {
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
        }
    }

    fn render(&self, md: &str) -> String {
        // 预处理：化学方程式 HTML/LaTeX → Unicode 上下标
        let md = preprocess_chemistry(md);

        let mut opts = Options::empty();
        opts.insert(Options::ENABLE_TABLES);
        opts.insert(Options::ENABLE_STRIKETHROUGH);
        opts.insert(Options::ENABLE_TASKLISTS);

        let parser = Parser::new_ext(&md, opts);
        let mut out = String::new();
        let mut in_code = false;
        let mut lang = String::new();
        let mut code = String::new();

        // 表格缓冲状态
        let mut in_table = false;
        let mut table_aligns: Vec<pulldown_cmark::Alignment> = Vec::new();
        let mut table_rows: Vec<Vec<String>> = Vec::new();
        let mut table_cur_row: Vec<String> = Vec::new();
        let mut table_cur_cell = String::new();

        // 列表状态：栈结构追踪嵌套列表 (kind, next_index)
        // kind = Some(n) 有序, None 无序
        let mut list_stack: Vec<(Option<u64>, u64)> = Vec::new();

        // 链接状态
        let mut link_url = String::new();

        // 行内样式栈：追踪活跃的样式代码，关闭时恢复外层样式
        let mut style_stack: Vec<&'static str> = Vec::new();

        for ev in parser {
            // ---- 表格缓冲模式 ----
            if in_table {
                match &ev {
                    Event::Start(Tag::TableHead) => continue,
                    Event::End(TagEnd::TableHead) => continue,
                    Event::Start(Tag::TableRow) => {
                        table_cur_row = Vec::new();
                        continue;
                    }
                    Event::End(TagEnd::TableRow) => {
                        if !table_cur_row.is_empty() {
                            table_rows.push(std::mem::take(&mut table_cur_row));
                        }
                        continue;
                    }
                    Event::Start(Tag::TableCell) => {
                        table_cur_cell = String::new();
                        continue;
                    }
                    Event::End(TagEnd::TableCell) => {
                        table_cur_row.push(std::mem::take(&mut table_cur_cell));
                        continue;
                    }
                    Event::End(TagEnd::Table) => {
                        out.push_str(&self.render_table(&table_aligns, &table_rows));
                        out.push('\n');
                        table_aligns.clear();
                        table_rows.clear();
                        in_table = false;
                        continue;
                    }
                    Event::Text(t) => {
                        table_cur_cell.push_str(t);
                        continue;
                    }
                    Event::Code(c) => {
                        table_cur_cell.push_str(&format!("{}`{c}`{}", ColorTheme::CODE_INLINE, ColorTheme::RESET));
                        continue;
                    }
                    Event::Start(Tag::Emphasis) => {
                        table_cur_cell.push_str(ColorTheme::ITALIC);
                        continue;
                    }
                    Event::End(TagEnd::Emphasis) => {
                        table_cur_cell.push_str(ColorTheme::RESET);
                        continue;
                    }
                    Event::Start(Tag::Strong) => {
                        table_cur_cell.push_str(ColorTheme::BOLD);
                        continue;
                    }
                    Event::End(TagEnd::Strong) => {
                        table_cur_cell.push_str(ColorTheme::RESET);
                        continue;
                    }
                    _ => continue,
                }
            }

            match ev {
                // ---- 代码块 ----
                Event::Start(Tag::CodeBlock(kind)) => {
                    in_code = true;
                    lang = match kind {
                        pulldown_cmark::CodeBlockKind::Fenced(l) => l.to_string(),
                        _ => String::new(),
                    };
                }
                Event::End(TagEnd::CodeBlock) => {
                    out.push_str(&self.code_block(&lang, &code));
                    code.clear();
                    in_code = false;
                }
                Event::Text(t) if in_code => code.push_str(&t),

                // ---- 表格入口 ----
                Event::Start(Tag::Table(aligns)) => {
                    in_table = true;
                    table_aligns = aligns;
                    table_rows = Vec::new();
                    table_cur_row = Vec::new();
                    table_cur_cell = String::new();
                }

                // ---- 普通文本 ----
                Event::Text(t) => out.push_str(&t),

                // ---- 标题 ----
                Event::Start(Tag::Heading { level, .. }) => {
                    out.push_str(match level {
                        pulldown_cmark::HeadingLevel::H1 => ColorTheme::HEADING_H1,
                        pulldown_cmark::HeadingLevel::H2 => ColorTheme::HEADING_H2,
                        pulldown_cmark::HeadingLevel::H3 => ColorTheme::HEADING_H3,
                        _ => ColorTheme::BOLD,
                    });
                }
                Event::End(TagEnd::Heading(_)) => {
                    out.push_str(ColorTheme::RESET);
                    out.push('\n');
                }

                // ---- 行内样式（样式栈确保嵌套不丢失）----
                Event::Start(Tag::Emphasis) => {
                    style_stack.push(ColorTheme::ITALIC);
                    out.push_str(ColorTheme::ITALIC);
                }
                Event::End(TagEnd::Emphasis) => {
                    style_stack.retain(|&s| s != ColorTheme::ITALIC);
                    out.push_str(ColorTheme::RESET);
                    for &s in &style_stack { out.push_str(s); }
                }
                Event::Start(Tag::Strong) => {
                    style_stack.push(ColorTheme::BOLD);
                    out.push_str(ColorTheme::BOLD);
                }
                Event::End(TagEnd::Strong) => {
                    style_stack.retain(|&s| s != ColorTheme::BOLD);
                    out.push_str(ColorTheme::RESET);
                    for &s in &style_stack { out.push_str(s); }
                }
                Event::Code(c) => out.push_str(&format!("{}`{c}`{}", ColorTheme::CODE_INLINE, ColorTheme::RESET)),
                Event::Start(Tag::Strikethrough) => {
                    style_stack.push(ColorTheme::STRIKETHROUGH);
                    out.push_str(ColorTheme::STRIKETHROUGH);
                }
                Event::End(TagEnd::Strikethrough) => {
                    style_stack.retain(|&s| s != ColorTheme::STRIKETHROUGH);
                    out.push_str(ColorTheme::RESET);
                    for &s in &style_stack { out.push_str(s); }
                }

                // ---- 引用 ----
                Event::Start(Tag::BlockQuote(_)) => out.push_str(ColorTheme::QUOTE),
                Event::End(TagEnd::BlockQuote(_)) => out.push_str(ColorTheme::RESET),

                // ---- 链接（样式栈保留外层样式）----
                Event::Start(Tag::Link { dest_url, .. }) => {
                    link_url = dest_url.to_string();
                    style_stack.push(ColorTheme::LINK);
                    style_stack.push(ColorTheme::UNDERLINE);
                    out.push_str(ColorTheme::LINK);
                    out.push_str(ColorTheme::UNDERLINE);
                }
                Event::End(TagEnd::Link) => {
                    style_stack.retain(|&s| s != ColorTheme::LINK && s != ColorTheme::UNDERLINE);
                    out.push_str(ColorTheme::RESET);
                    for &s in &style_stack { out.push_str(s); }
                    // 链接后附 URL
                    out.push_str(&format!("{} ({}){}", ColorTheme::DIM, link_url, ColorTheme::RESET));
                    link_url.clear();
                }

                // ---- 列表（栈追踪嵌套）----
                Event::Start(Tag::List(number)) => {
                    let start = number.unwrap_or(1);
                    list_stack.push((number, start));
                    // 嵌套缩进：每层 2 空格
                    let indent = "  ".repeat(list_stack.len().saturating_sub(1));
                    out.push_str(&indent);
                }
                Event::End(TagEnd::List(_)) => {
                    list_stack.pop();
                    // 最外层列表结束时加空行
                    if list_stack.is_empty() {
                        out.push('\n');
                    }
                }
                Event::Start(Tag::Item) => {
                    let indent = "  ".repeat(list_stack.len().saturating_sub(1));
                    if let Some((kind, idx)) = list_stack.last_mut() {
                        match kind {
                            Some(_) => {
                                // 有序列表：自动编号
                                out.push_str(&format!(
                                    "{indent}{bold}{accent}{idx}.{rst} ",
                                    bold = ColorTheme::BOLD,
                                    accent = ColorTheme::ACCENT,
                                    rst = ColorTheme::RESET,
                                ));
                                *idx += 1;
                            }
                            None => {
                                // 无序列表
                                out.push_str(&format!(
                                    "{indent}{bullet}·{rst} ",
                                    bullet = ColorTheme::BULLET,
                                    rst = ColorTheme::RESET,
                                ));
                            }
                        }
                    }
                }
                Event::End(TagEnd::Item) => out.push('\n'),

                // ---- 任务列表 ----
                Event::TaskListMarker(checked) => {
                    if checked {
                        out.push_str(&format!("{}[x]{} ", ColorTheme::SUCCESS, ColorTheme::RESET));
                    } else {
                        out.push_str(&format!("{}[ ]{} ", ColorTheme::DIM, ColorTheme::RESET));
                    }
                }

                // ---- 段落 / 换行 ----
                Event::End(TagEnd::Paragraph) => out.push('\n'),
                Event::HardBreak => out.push('\n'),
                Event::SoftBreak => out.push(' '),

                _ => {}
            }
        }
        out
    }

    /// 渲染表格：计算列宽 + 绘制 Unicode 制表符边框
    fn render_table(&self, _aligns: &[pulldown_cmark::Alignment], rows: &[Vec<String>]) -> String {
        if rows.is_empty() {
            return String::new();
        }

        let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(1);
        // 计算每列的显示宽度（去除 ANSI 转义码）
        let mut col_widths = vec![3usize; ncols]; // 最小 3
        for row in rows {
            for (ci, cell) in row.iter().enumerate() {
                let w = visible_width(cell).max(3);
                if w > col_widths[ci] {
                    col_widths[ci] = w.min(50); // 单列最大 50
                }
            }
        }

        let mut out = String::new();
        out.push('\n');

        let is_header = rows.len() >= 1;

        for (ri, row) in rows.iter().enumerate() {
            let is_hdr = ri == 0 && is_header;
            // 顶部边框（仅表头行前）
            if ri == 0 {
                out.push_str(&table_border_top(&col_widths));
            }
            // 行内容
            for (ci, w) in col_widths.iter().enumerate() {
                let cell = row.get(ci).map(|s| s.as_str()).unwrap_or("");
                let pad = visible_width(cell);
                let extra = if pad < *w { w - pad } else { 0 };
                let pad_str = spacer(extra);
                if is_hdr {
                    out.push_str(&format!(
                        "{dim}│{rst} {bold}{cell}{rst}{pad_str} ",
                        dim = ColorTheme::DIM,
                        rst = ColorTheme::RESET,
                        bold = ColorTheme::BOLD,
                    ));
                } else {
                    out.push_str(&format!(
                        "{dim}│{rst} {cell}{pad_str} ",
                        dim = ColorTheme::DIM,
                        rst = ColorTheme::RESET,
                    ));
                }
            }
            out.push_str(&format!("{dim}│{rst}\n", dim = ColorTheme::DIM, rst = ColorTheme::RESET));

            // 表头分隔线
            if is_hdr {
                out.push_str(&table_border_sep(&col_widths));
            }
        }
        // 底部边框
        out.push_str(&table_border_bottom(&col_widths));
        out
    }

    fn code_block(&self, lang: &str, code: &str) -> String {
        let syntax = if lang.is_empty() {
            self.syntax_set.find_syntax_plain_text()
        } else {
            self.syntax_set
                .find_syntax_by_token(lang)
                .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text())
        };
        let theme = &self.theme_set.themes["base16-ocean.dark"];
        let mut h = HighlightLines::new(syntax, theme);
        let w = code.lines().next().map_or(0, |l| l.len()).max(40);

        let dim = ColorTheme::DIM;
        let rst = ColorTheme::RESET;
        let bg = ColorTheme::CODE_BG;

        let mut out = String::new();
        out.push_str(&format!("{dim}╭{}╮{rst}\n", "─".repeat(w + 2)));

        for line in code.lines() {
            match h.highlight_line(line, &self.syntax_set) {
                Ok(ranges) => {
                    let styled: String = ranges
                        .iter()
                        .map(|(s, t)| format!("\x1b[38;2;{};{};{}m{bg}{t}", s.foreground.r, s.foreground.g, s.foreground.b))
                        .collect();
                    out.push_str(&format!("{dim}│{rst} {styled}{rst}\n"));
                }
                Err(_) => out.push_str(&format!("{dim}│{rst} {bg}{line}{rst}\n")),
            }
        }

        out.push_str(&format!("{dim}╰{}╯{rst}\n", "─".repeat(w + 2)));
        out
    }
}

// =============================================================================
// 表格渲染辅助
// =============================================================================

/// 计算字符串的可见字符宽度（跳过 ANSI 转义码）
fn visible_width(s: &str) -> usize {
    let mut w = 0usize;
    let mut in_esc = false;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if in_esc {
            if bytes[i] == b'm' {
                in_esc = false;
            }
            i += 1;
            continue;
        }
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            in_esc = true;
            i += 2;
            continue;
        }
        let ch = s[i..].chars().next().unwrap_or(' ');
        w += char_display_width(ch);
        i += ch.len_utf8();
    }
    w
}

/// 终端列宽：East Asian Wide/Fullwidth 计 2，其余计 1。
/// 注意：箭头(→)、项目符号(•)、表格边框(│) 等符号虽码位 > 0x2000，但终端按 1 列显示，
/// 不能简单按 `c > 0x2000` 判 2 列，否则填充多算导致表格列错位。
fn char_display_width(c: char) -> usize {
    let u = c as u32;
    // 零宽字符
    if u == 0 || (0x0300..=0x036F).contains(&u) {
        return 0;
    }
    let wide = matches!(u,
        0x1100..=0x115F |   // 谚文 Jamo
        0x2E80..=0x303E |   // CJK 部首、康熙部首、CJK 符号与标点
        0x3041..=0x33FF |   // 平假名、片假名、注音、CJK 兼容
        0x3400..=0x4DBF |   // CJK 扩展 A
        0x4E00..=0x9FFF |   // CJK 统一表意文字
        0xA000..=0xA4CF |   // 彝文
        0xAC00..=0xD7A3 |   // 谚文音节
        0xF900..=0xFAFF |   // CJK 兼容表意文字
        0xFE10..=0xFE19 |   // 竖排标点
        0xFE30..=0xFE6F |   // CJK 兼容形式、小写变体
        0xFF00..=0xFF60 |   // 全角 ASCII、全角标点
        0xFFE0..=0xFFE6 |   // 全角符号
        0x1F300..=0x1FAFF | // emoji 及符号
        0x20000..=0x3FFFD   // CJK 扩展 B 及以上
    );
    if wide { 2 } else { 1 }
}

fn spacer(n: usize) -> String {
    " ".repeat(n)
}

fn table_border_top(widths: &[usize]) -> String {
    let mut s = String::from(ColorTheme::DIM);
    s.push('┌');
    for (i, w) in widths.iter().enumerate() {
        s.push_str(&"─".repeat(w + 2));
        if i + 1 < widths.len() { s.push('┬'); }
    }
    s.push('┐');
    s.push_str(ColorTheme::RESET);
    s.push('\n');
    s
}

fn table_border_sep(widths: &[usize]) -> String {
    let mut s = String::from(ColorTheme::DIM);
    s.push('├');
    for (i, w) in widths.iter().enumerate() {
        s.push_str(&"─".repeat(w + 2));
        if i + 1 < widths.len() { s.push('┼'); }
    }
    s.push('┤');
    s.push_str(ColorTheme::RESET);
    s.push('\n');
    s
}

fn table_border_bottom(widths: &[usize]) -> String {
    let mut s = String::from(ColorTheme::DIM);
    s.push('└');
    for (i, w) in widths.iter().enumerate() {
        s.push_str(&"─".repeat(w + 2));
        if i + 1 < widths.len() { s.push('┴'); }
    }
    s.push('┘');
    s.push_str(ColorTheme::RESET);
    s.push('\n');
    s
}

// =============================================================================
// rustyline Helper
// =============================================================================

struct ReplHelper {
    bracket: MatchingBracketHighlighter,
    validator: MatchingBracketValidator,
    hinter: HistoryHinter,
    commands: Vec<&'static str>,
}

impl ReplHelper {
    fn new() -> Self {
        Self {
            bracket: MatchingBracketHighlighter::new(),
            validator: MatchingBracketValidator::new(),
            hinter: HistoryHinter::new(),
            commands: vec!["/quit", "/exit", "/clear", "/compact", "/cost", "/help"],
        }
    }
}

impl Helper for ReplHelper {}

impl Completer for ReplHelper {
    type Candidate = Pair;

    fn complete(&self, line: &str, pos: usize, _: &Context<'_>) -> Result<(usize, Vec<Pair>), ReadlineError> {
        if !line.starts_with('/') {
            return Ok((0, vec![]));
        }
        let matches = self.commands.iter()
            .filter(|c| c.starts_with(&line[..pos]))
            .map(|c| Pair { display: c.to_string(), replacement: format!("{c} ") })
            .collect();
        Ok((0, matches))
    }
}

impl Hinter for ReplHelper {
    type Hint = String;
    fn hint(&self, line: &str, pos: usize, ctx: &Context<'_>) -> Option<String> {
        self.hinter.hint(line, pos, ctx)
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
    fn validate(&self, ctx: &mut rustyline::validate::ValidationContext) -> Result<ValidationResult, ReadlineError> {
        self.validator.validate(ctx)
    }
    fn validate_while_typing(&self) -> bool {
        self.validator.validate_while_typing()
    }
}

// =============================================================================
// 工具事件解析
// =============================================================================

// =============================================================================
// 流式 Markdown 渲染（延迟提交，参考 claw-code）
// =============================================================================

/// 累积流式文本，只在遇到"安全边界"（空行分隔的段落、闭合的代码围栏）时
/// 提交渲染，已提交内容不再重绘。流结束时 flush 剩余部分。
struct MarkdownStreamState {
    pending: String,
}

impl MarkdownStreamState {
    fn new() -> Self {
        Self { pending: String::new() }
    }

    /// 追加增量，返回本次可安全输出的已渲染 ANSI（可能为空）
    fn push(&mut self, delta: &str, renderer: &Renderer) -> String {
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
    fn flush(&mut self, renderer: &Renderer) -> String {
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
    let after: String = trimmed[count..].chars().take_while(|&c| c != '\n' && c != '\r').collect();
    if after.chars().any(|c| c == ch) {
        return None; // 混杂字符，不是纯围栏
    }
    Some(FenceMarker { character: ch, length: count })
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
            let is_close = fences.iter().rev().any(|(_, f, is_open)| *is_open && f.character == fm.character && f.length == fm.length);
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

    let new_len = max_len + max_depth;
    let mut result = String::with_capacity(text.len() + fences.len() * 3);
    let mut prev_line = 0usize;

    // 重建文本，替换外层的围栏长度
    for (li, fm, is_open) in &fences {
        if fm.length < new_len {
            // 推入此行之前的内容
            for l in &lines[prev_line..*li] {
                result.push_str(l);
                result.push('\n');
            }
            // 替换围栏行
            let old_fence: String = std::iter::repeat(fm.character).take(fm.length).collect();
            let new_fence: String = std::iter::repeat(fm.character).take(new_len).collect();
            let line = lines[*li].replacen(&old_fence, &new_fence, 1);
            result.push_str(&line);
            result.push('\n');
            prev_line = *li + 1;
        } else if *is_open {
            // 推入之前的行（包括当前围栏行）
            for l in &lines[prev_line..=*li] {
                result.push_str(l);
                result.push('\n');
            }
            prev_line = *li + 1;
        }
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

// =============================================================================
// 工具事件解析与美化
// =============================================================================

fn parse_tool_call(content: Option<&str>) -> (String, String) {
    match content.and_then(|s| serde_json::from_str::<raven_types::ToolCall>(s).ok()) {
        Some(tc) => (tc.function.name, tc.function.arguments),
        None => ("tool".into(), String::new()),
    }
}

fn parse_tool_result(content: Option<&str>) -> (String, String, bool) {
    match content.and_then(|s| serde_json::from_str::<raven_types::ToolResult>(s).ok()) {
        Some(tr) => (tr.name, tr.content, tr.is_error),
        None => (String::new(), content.unwrap_or_default().into(), false),
    }
}

/// 工具图标 —— 让工具调用一眼可辨（无图标时回退到通用符号）
fn tool_icon(name: &str) -> &'static str {
    match name {
        "file_read" | "view" | "read" => "📄",
        "file_write" | "write" => "✍",
        "file_edit" | "edit" => "✏",
        "search" | "grep" => "🔍",
        "list_dir" | "ls" => "📂",
        "shell" | "bash" | "exec" => "❯_",
        "git" => "⎇",
        "web_search" => "🌐",
        "fetch_url" | "fetch" => "⬇",
        _ => "◆",
    }
}

/// 工具调用：单行摘要，工具图标 + spinner + 工具名 + 紧凑参数
fn render_tool_call(name: &str, args: &str, spin_idx: u32) -> String {
    let brief = brief_args(args);
    let spin = spinner_frame(spin_idx);
    let icon = tool_icon(name);
    if brief.is_empty() {
        format!("\n{accent}{spin}{rst} {icon} {bold}{name}{rst} {dim}(no args){rst}\n",
            accent = ColorTheme::ACCENT, spin = spin, rst = ColorTheme::RESET,
            bold = ColorTheme::BOLD, dim = ColorTheme::DIM, icon = icon, name = name)
    } else {
        format!("\n{accent}{spin}{rst} {icon} {bold}{name}{rst} {dim}{brief}{rst}\n",
            accent = ColorTheme::ACCENT, spin = spin, rst = ColorTheme::RESET,
            bold = ColorTheme::BOLD, dim = ColorTheme::DIM, icon = icon, name = name, brief = brief)
    }
}

/// 工具结果：缩进预览，成功灰色 / 失败红色高亮
fn render_tool_result(_name: &str, output: &str, is_error: bool) -> String {
    let preview = truncate_lines(output.trim_end(), 10);
    let dim = ColorTheme::DIM;
    let rst = ColorTheme::RESET;
    if is_error {
        format!("  {err}✗{rst} {dim}{preview}{rst}\n", err = ColorTheme::ERROR, rst = rst, dim = dim)
    } else {
        let mut out = String::new();
        let mut lines = preview.lines();
        if let Some(first) = lines.next() {
            out.push_str(&format!("  {dim}└{rst} {dim}{first}{rst}\n"));
        } else {
            out.push_str(&format!("  {dim}└{rst}\n"));
        }
        for line in lines {
            out.push_str(&format!("    {dim}{line}{rst}\n"));
        }
        out
    }
}

/// 把 JSON 参数压成一行简短摘要（最多 ~60 字符）
fn brief_args(args: &str) -> String {
    let trimmed = args.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return String::new();
    }
    let compact = match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(serde_json::Value::Object(map)) => {
            let parts: Vec<String> = map.iter().map(|(k, v)| {
                let vs = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                format!("{k}={vs}")
            }).collect();
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

fn truncate_lines(s: &str, max: usize) -> String {
    let lines: Vec<&str> = s.lines().take(max).collect();
    let cut = lines.len() < s.lines().count();
    let mut r = lines.join("\n");
    if cut {
        r.push_str(" ...");
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_width_is_one_per_char() {
        assert_eq!(visible_width("hello"), 5);
        assert_eq!(visible_width(""), 0);
    }

    #[test]
    fn cjk_chars_are_two_columns() {
        assert_eq!(visible_width("中文"), 4);
        assert_eq!(visible_width("a中b"), 4); // 1 + 2 + 1
    }

    #[test]
    fn symbols_above_0x2000_stay_one_column() {
        // 回归：旧逻辑 `c > 0x2000 即算 2 列` 会把这些误判成 2，导致表格列错位
        assert_eq!(char_display_width('→'), 1); // U+2192 箭头
        assert_eq!(char_display_width('•'), 1); // U+2022 项目符号
        assert_eq!(char_display_width('│'), 1); // U+2502 表格边框
        assert_eq!(char_display_width('—'), 1); // U+2014 破折号
        assert_eq!(char_display_width('“'), 1); // U+201C 左引号
    }

    #[test]
    fn fullwidth_and_emoji_are_two_columns() {
        assert_eq!(char_display_width('，'), 2); // U+FF0C 全角逗号
        assert_eq!(char_display_width('🦀'), 2); // emoji
    }

    #[test]
    fn ansi_escapes_have_zero_width() {
        let styled = format!("{}中{}", ColorTheme::BOLD, ColorTheme::RESET);
        assert_eq!(visible_width(&styled), 2);
    }
}
