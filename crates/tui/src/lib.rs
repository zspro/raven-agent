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
//! 模块划分：theme（颜色/spinner/围栏标记）、latex（数学预处理）、render（Markdown→ANSI）、
//! helper（rustyline Helper）、stream（流式延迟提交）、tool_display（工具事件美化）。
//!
//! 启动: raven tui

mod helper;
mod latex;
mod latex_unicode;
mod render;
mod stream;
mod theme;
mod tool_display;
mod width;

use helper::{ExpandHandler, ReplHelper};
use raven_core::Agent;
use render::Renderer;
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;
use rustyline::{Config, EditMode, Editor, Event, EventHandler, KeyEvent};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use stream::MarkdownStreamState;
use theme::ColorTheme;
use tool_display::{parse_tool_call, parse_tool_result, render_result_only, render_tool_header};

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

    handle
        .join()
        .map_err(|e| anyhow::anyhow!("TUI 线程 panic: {e:?}"))?
}

// =============================================================================
// Repl — 核心结构，持有所有状态
// =============================================================================

/// 一轮里展示过的工具调用，供 Ctrl+O 展开时按原序重打完整输出。
struct ToolRecord {
    name: String,
    args: String,
    output: String,
    is_error: bool,
}

struct Repl {
    agent: Arc<Agent>,
    rt: tokio::runtime::Runtime,
    editor: Editor<ReplHelper, DefaultHistory>,
    renderer: Renderer,
    /// 工具输出折叠后保留的预览行数（`/preview <n>` 可改，0 表示不折叠）。
    preview_lines: usize,
    /// 上一轮的工具调用记录，Ctrl+O 时重打其完整输出。
    last_tools: Vec<ToolRecord>,
    /// Ctrl+O 按下标志：处理器置位，readline 返回后主循环检测并清零。
    expand_flag: Arc<AtomicBool>,
}

impl Repl {
    fn new(agent: Arc<Agent>, rt: tokio::runtime::Runtime) -> Self {
        let config = Config::builder().edit_mode(EditMode::Emacs).build();

        let mut editor = Editor::with_config(config).expect("初始化 rustyline 失败");
        editor.set_helper(Some(ReplHelper::new()));

        // Ctrl+O：展开上一轮被折叠的工具输出。处理器与主循环共享标志位。
        let expand_flag = Arc::new(AtomicBool::new(false));
        editor.bind_sequence(
            Event::KeySeq(vec![KeyEvent::ctrl('o')]),
            EventHandler::Conditional(Box::new(ExpandHandler::new(expand_flag.clone()))),
        );

        // 加载历史
        let history_path = Self::history_path();
        let _ = editor.load_history(&history_path);

        // 折叠行数从持久化配置读取（/preview 修改后会写回并热重载）。
        let preview_lines = agent.config().tui.preview_lines;

        Self {
            agent,
            rt,
            editor,
            renderer: Renderer::new(),
            preview_lines,
            last_tools: Vec::new(),
            expand_flag,
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

            // Ctrl+O：展开上一轮折叠的工具输出（处理器提交空行 + 置位标志）。
            if self.expand_flag.swap(false, Ordering::Relaxed) {
                self.expand_last_tools();
                continue;
            }

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
                println!(
                    "{err}错误: {e}{rst}\n",
                    err = ColorTheme::ERROR,
                    rst = ColorTheme::RESET
                );
                return;
            }
        };

        let mut rx = rx;
        let mut stream = MarkdownStreamState::new();
        // 待配对的工具调用 (name, args)，FIFO：core 按调用顺序发回 tool_result
        let mut pending_calls: std::collections::VecDeque<(String, String)> =
            std::collections::VecDeque::new();
        let mut tool_count = 0u32;
        let mut had_text = false;
        let mut interrupted = false;
        // 本轮工具记录清零，逐个收集供 Ctrl+O 展开
        self.last_tools.clear();
        // 预览行数：0 表示不折叠（传 None）
        let preview = if self.preview_lines == 0 {
            None
        } else {
            Some(self.preview_lines)
        };

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
                    // 工具调用前先 flush 已缓冲的文本，再立即打印「● 工具」标题行，
                    // 让用户瞬间看到工具已启动（尤其并行 task 执行期间不再长时间空白）。
                    let rest = stream.flush(&self.renderer);
                    if !rest.is_empty() {
                        print!("{rest}");
                    }
                    let (name, args) = parse_tool_call(event.content.as_deref());
                    print!("{}", render_tool_header(&name, &args));
                    let _ = std::io::stdout().flush();
                    pending_calls.push_back((name, args));
                    tool_count += 1;
                }
                "tool_result" => {
                    let (name, output, is_error) = parse_tool_result(event.content.as_deref());
                    // 标题行已在 tool_call 时打印，这里只补结果体（按 preview 行数折叠）。
                    // 配对队首调用以记录其 name/args，供 Ctrl+O 展开重打完整输出。
                    let (call_name, args) = pending_calls
                        .pop_front()
                        .unwrap_or_else(|| (name.clone(), String::new()));
                    print!("{}", render_result_only(&output, is_error, preview));
                    let _ = std::io::stdout().flush();
                    self.last_tools.push(ToolRecord {
                        name: if call_name.is_empty() {
                            name
                        } else {
                            call_name
                        },
                        args,
                        output,
                        is_error,
                    });
                }
                "error" => {
                    let rest = stream.flush(&self.renderer);
                    if !rest.is_empty() {
                        print!("{rest}");
                    }
                    let msg = event.content.unwrap_or_default();
                    println!(
                        "{err}错误: {msg}{rst}\n",
                        err = ColorTheme::ERROR,
                        rst = ColorTheme::RESET
                    );
                    return;
                }
                "done" => {}
                _ => {}
            }
        }

        // 收尾：中断时可能有未配对的工具调用（已发 call 未收 result），丢弃即可
        pending_calls.clear();

        // flush 缓冲区剩余内容
        let rest = stream.flush(&self.renderer);
        if !rest.is_empty() {
            print!("{rest}");
        }

        if interrupted {
            // 主动 drop 接收端，让上游 spawn 任务自然结束
            drop(rx);
            println!(
                "\n{dim}  (已中断){rst}",
                dim = ColorTheme::DIM,
                rst = ColorTheme::RESET
            );
        } else if !had_text && tool_count == 0 {
            println!(
                "{dim}  (空回复){rst}",
                dim = ColorTheme::DIM,
                rst = ColorTheme::RESET
            );
        } else {
            println!();
        }

        self.print_info_bar();
    }

    /// Ctrl+O：把上一轮所有工具的**完整输出**重新打印到下方（不折叠）。
    ///
    /// 受行式 REPL 所限无法原地展开已滚走的内容，故重打整组。
    fn expand_last_tools(&self) {
        if self.last_tools.is_empty() {
            println!(
                "{dim}  (无可展开的工具输出){rst}\n",
                dim = ColorTheme::DIM,
                rst = ColorTheme::RESET
            );
            return;
        }
        for t in &self.last_tools {
            print!("{}", render_tool_header(&t.name, &t.args));
            print!("{}", render_result_only(&t.output, t.is_error, None));
        }
        println!();
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

    /// 处理斜杠命令，返回 true 表示应退出 REPL
    fn handle_command(&mut self, cmd: &str) -> bool {
        // 带参数的命令先单独处理
        if let Some(arg) = cmd.strip_prefix("/preview") {
            self.set_preview(arg.trim());
            return false;
        }
        match cmd {
            "/quit" | "/exit" | "/q" => return true,
            "/clear" => {
                self.rt.block_on(self.agent.clear());
                println!(
                    "{ok}已清空会话{rst}\n",
                    ok = ColorTheme::SUCCESS,
                    rst = ColorTheme::RESET
                );
            }
            "/compact" => match self.rt.block_on(self.agent.compact()) {
                Ok(()) => {
                    let stats = self.rt.block_on(self.agent.stats());
                    println!(
                        "{ok}已压缩上下文{rst} {dim}(ctx {ctx} tokens){rst}\n",
                        ok = ColorTheme::SUCCESS,
                        rst = ColorTheme::RESET,
                        dim = ColorTheme::DIM,
                        ctx = stats.current_context_tokens
                    );
                }
                Err(e) => {
                    println!(
                        "{err}压缩失败: {e}{rst}\n",
                        err = ColorTheme::ERROR,
                        rst = ColorTheme::RESET
                    );
                }
            },
            "/cost" => {
                let stats = self.rt.block_on(self.agent.stats());
                let total = stats.total_input_tokens + stats.total_output_tokens;
                println!(
                    "{dim}  ctx {ctx} · {inp}↑/{out}↓ ({total} tokens){rst}\n",
                    dim = ColorTheme::DIM,
                    rst = ColorTheme::RESET,
                    ctx = stats.current_context_tokens,
                    inp = stats.total_input_tokens,
                    out = stats.total_output_tokens,
                );
            }
            "/help" => {
                println!(
                    "{dim}  /clear      清空会话\n  \
                     /compact    压缩上下文\n  \
                     /cost       查看 token 用量\n  \
                     /preview <n> 工具输出折叠行数（0=不折叠，当前 {pv}）\n  \
                     /quit       退出 (Ctrl+D){rst}\n  \
                     {dim}Ctrl+O 展开上一轮工具的完整输出{rst}\n",
                    dim = ColorTheme::DIM,
                    rst = ColorTheme::RESET,
                    pv = self.preview_lines,
                );
            }
            other => {
                println!(
                    "{err}未知命令: {other}{rst} {dim}(/help 查看命令){rst}\n",
                    err = ColorTheme::ERROR,
                    rst = ColorTheme::RESET,
                    dim = ColorTheme::DIM
                );
            }
        }
        false
    }

    /// 设置工具输出折叠的预览行数（`/preview <n>`）。空参数则显示当前值。
    fn set_preview(&mut self, arg: &str) {
        if arg.is_empty() {
            println!(
                "{dim}  当前折叠行数: {pv}（/preview <n> 修改，0=不折叠）{rst}\n",
                dim = ColorTheme::DIM,
                rst = ColorTheme::RESET,
                pv = self.preview_lines,
            );
            return;
        }
        match arg.parse::<usize>() {
            Ok(n) => {
                self.preview_lines = n;
                self.persist_preview_lines(n);
                let desc = if n == 0 {
                    "不折叠，完整显示".to_string()
                } else {
                    format!("折叠为 {n} 行预览")
                };
                println!(
                    "{ok}已设置工具输出{desc}{rst} {dim}(已持久化){rst}\n",
                    ok = ColorTheme::SUCCESS,
                    rst = ColorTheme::RESET,
                    dim = ColorTheme::DIM,
                );
            }
            Err(_) => {
                println!(
                    "{err}无效行数: {arg}{rst} {dim}(需要非负整数){rst}\n",
                    err = ColorTheme::ERROR,
                    rst = ColorTheme::RESET,
                    dim = ColorTheme::DIM,
                );
            }
        }
    }

    /// 把折叠行数写入 `~/.raven/config.toml` 的 `[tui]` 段，使其跨重启保留。
    ///
    /// 本会话已直接更新 `self.preview_lines`，运行时无其他读取者，故只落盘、
    /// 不触发 Agent 热重载（避免为改个行数而重新注册所有提供商）。
    fn persist_preview_lines(&self, n: usize) {
        let Some(path) = dirs::home_dir().map(|h| h.join(".raven").join("config.toml")) else {
            return;
        };
        Self::write_tui_preview_lines(&path, n);
    }

    /// 在 config.toml 的 `[tui]` 段写入/替换 `preview_lines`（值不加引号）。
    /// 复用 cli 的解析思路，但 tui 不依赖 cli，故在此内联一份精简实现。
    fn write_tui_preview_lines(path: &std::path::Path, n: usize) {
        let content = if path.exists() {
            std::fs::read_to_string(path).unwrap_or_default()
        } else {
            String::new()
        };
        let new_line = format!("preview_lines = {n}");
        let is_section = |l: &str| l.trim_start().starts_with('[');
        let is_key = |l: &str| {
            let t = l.trim_start();
            t.starts_with("preview_lines =") || t.starts_with("preview_lines=")
        };
        let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
        let header = "[tui]";
        let hidx = lines.iter().position(|l| l.trim() == header);
        let updated = match hidx {
            Some(h) => {
                let mut end = lines.len();
                for (i, l) in lines.iter().enumerate().skip(h + 1) {
                    if is_section(l) {
                        end = i;
                        break;
                    }
                }
                match (h + 1..end).find(|&i| is_key(&lines[i])) {
                    Some(i) => lines[i] = new_line,
                    None => {
                        let mut at = end;
                        while at > h + 1 && lines[at - 1].trim().is_empty() {
                            at -= 1;
                        }
                        lines.insert(at, new_line);
                    }
                }
                lines.join("\n") + "\n"
            }
            None => format!("{}\n{}\n{}\n", content.trim_end(), header, new_line),
        };
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")));
        let _ = std::fs::write(path, updated);
    }

    fn history_path() -> std::path::PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| ".".into())
            .join(".raven")
            .join("tui_history")
    }

    fn print_welcome(&self) {
        let art = [
            r"       ___      ",
            r"      (o,o)     ",
            r#"      {  "  }    "#,
            r#"    ---"-"---   "#,
        ];
        let tag = ["", "Raven 🐦‍⬛", "Think like a raven.", "Code like the wind."];
        let accent = ColorTheme::ACCENT;
        let dim = ColorTheme::DIM;
        let rst = ColorTheme::RESET;
        for (i, line) in art.iter().enumerate() {
            let t = tag.get(i).copied().unwrap_or("");
            println!("{accent}{line}{rst}  {dim}{t}{rst}");
        }
        println!(
            "\n{dim}  {model} · {mode}{rst}",
            dim = dim,
            rst = rst,
            model = self.agent.config().model,
            mode = self.agent.permission_mode(),
        );
        println!(
            "{dim}  /help 查看命令 · Ctrl+O 展开工具输出 · Ctrl+C 中断 · Ctrl+D 退出{rst}\n",
            dim = dim,
            rst = rst
        );
    }
}
