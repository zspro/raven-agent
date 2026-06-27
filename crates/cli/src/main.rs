//! CLI 入口

mod commands;
mod config_io;
mod input;
mod settings;

use clap::{Parser, Subcommand};
use commands::{
    cmd_chat, cmd_doctor, cmd_init, cmd_models, cmd_serve, cmd_single, cmd_tui,
    cmd_tui_with_opening, cmd_verify,
};
use input::{merge_prompt_with_stdin, read_piped_stdin};
use std::io::IsTerminal;

#[derive(Parser)]
#[command(name = "raven")]
#[command(
    about = "Raven 🐦‍⬛ — Think like a raven. Code like the wind. 轻量跨平台 AI Agent 终端助手。"
)]
#[command(version = "0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// 强制单次模式：执行完一轮任务即退出，不进入交互（对齐 claude/claw 的 -p）
    #[arg(short = 'p', long = "print")]
    print: bool,

    /// 直接输入消息（不使用子命令）
    #[arg(trailing_var_arg = true)]
    message: Vec<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// 交互式对话模式
    Chat,
    /// 启动 HTTP API 服务器
    Serve {
        #[arg(long, default_value = "0.0.0.0")]
        host: String,
        #[arg(long, default_value_t = 8080)]
        port: u16,
    },
    /// 诊断检查
    Doctor,
    /// 列出可用模型
    Models,
    /// 验证模型提供商
    Verify,
    /// 初始化配置文件
    Init,
    /// 启动 TUI 界面
    Tui,
}

#[tokio::main]
async fn main() {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // 让 Windows 旧版控制台也能渲染 ANSI 颜色（非 Windows 为空操作）。
    config_system::platform::enable_ansi_support();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Chat) => cmd_chat().await,
        Some(Commands::Serve { host, port }) => cmd_serve(host, port).await,
        Some(Commands::Doctor) => cmd_doctor().await,
        Some(Commands::Models) => cmd_models().await,
        Some(Commands::Verify) => cmd_verify().await,
        Some(Commands::Init) => cmd_init().await,
        Some(Commands::Tui) => cmd_tui().await,
        None => {
            // 没有子命令：合并「命令行消息」与「管道 stdin」，再分场景决策。
            // 语义对齐 claw-code/Claude Code：交互是默认，单次需显式意图（-p 或管道）。
            let cli_message = cli.message.join(" ");
            let piped = read_piped_stdin();
            let message = merge_prompt_with_stdin(&cli_message, piped.as_deref());
            let has_message = !message.trim().is_empty();
            let stdin_is_tty = std::io::stdin().is_terminal();

            if cli.print {
                // -p / --print：强制单次。无任何输入则报错（避免空跑）。
                if has_message {
                    cmd_single(message).await;
                } else {
                    eprintln!(
                        "interactive_only: -p/--print 需要 prompt。\n\
                         用 `raven -p \"任务\"` 或 `echo '任务' | raven -p` 传入。"
                    );
                    std::process::exit(2);
                }
            } else if !stdin_is_tty {
                // 非 TTY（管道/重定向/CI）：是脚本场景，按单次执行。
                // 有内容则单次，无内容报错（不启动会永久阻塞的 REPL）。
                if has_message {
                    cmd_single(message).await;
                } else {
                    eprintln!(
                        "interactive_only: 需要交互式终端。\n\
                         stdin 不是 TTY 且未提供 prompt — 用 `echo '任务' | raven` 传入，\
                         或在交互终端中运行 `raven`。"
                    );
                    std::process::exit(2);
                }
            } else if has_message {
                // TTY 且命令行带了文本：默认进 TUI，把这段文本作为开场白先发出去。
                // （对齐 claw-code：裸 `raven "问题"` 不是单次，而是交互的第一句）
                cmd_tui_with_opening(message).await;
            } else {
                // TTY 且无输入：默认进 TUI 全屏界面。
                // （朴素滚动交互仍可用 `raven chat` 显式进入）
                cmd_tui().await;
            }
        }
    }
}
