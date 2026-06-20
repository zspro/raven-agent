//! CLI 入口

use raven_core::Agent;
use clap::{Parser, Subcommand};
use std::io::{IsTerminal, Read};
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "raven")]
#[command(about = "Raven 🐦‍⬛ — Think like a raven. Code like the wind. 轻量跨平台 AI Agent 终端助手。")]
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

// =============================================================================
// 命令实现
// =============================================================================

/// 读取管道传入的 stdin 内容。
///
/// 当 stdin 是终端（交互式）、读取失败、或内容 trim 后为空时返回 `None`；
/// 否则返回 `Some(原始内容)`。
fn read_piped_stdin() -> Option<String> {
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
fn merge_prompt_with_stdin(prompt: &str, stdin_content: Option<&str>) -> String {
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

/// 初始化 Agent
async fn init_agent() -> Arc<Agent> {
    let cfg_sys = config_system::ConfigSystem::load()
        .unwrap_or_else(|e| {
            eprintln!("配置错误: {}", e);
            std::process::exit(1);
        });

    let mut agent = Agent::from_config(&cfg_sys).await
        .unwrap_or_else(|e| {
            eprintln!("初始化失败: {}", e);
            std::process::exit(1);
        });

    // 注入终端确认器：ask 模式下执行敏感工具前实时询问 y/n/a
    agent.set_confirmer(Arc::new(raven_core::StdinConfirmer));

    Arc::new(agent)
}

/// 交互式对话
async fn cmd_chat() {
    cmd_chat_with_opening(String::new()).await;
}

/// 交互式对话；若 `opening` 非空，先把它作为第一句发出去再进入循环。
async fn cmd_chat_with_opening(opening: String) {
    let agent = init_agent().await;

    // 青色加粗标题 + 暗色命令提示，避免宽字符撑破对齐的方框
    println!("\n  \x1b[1;36mRaven 🐦‍⬛\x1b[0m  \x1b[2m交互式对话\x1b[0m");
    println!("  \x1b[2m/quit 退出 · /clear 清空 · /compact 压缩 · /stats 统计 · /settings 设置 · /prompt 切换角色\x1b[0m\n");

    use std::io::Write;

    // 开场白：命令行带了文本时，先跑一轮再进入交互循环。
    let opening = opening.trim();
    if !opening.is_empty() {
        println!("> {}", opening);
        let start = std::time::Instant::now();
        match agent.run(opening).await {
            Ok(response) => {
                println!("\n{}", response);
                println!("\n[{}ms]", start.elapsed().as_millis());
            }
            Err(e) => {
                println!("\n[错误] {}", e);
            }
        }
    }

    loop {
        print!("\n> ");
        std::io::stdout().flush().unwrap();

        let mut input = String::new();
        match std::io::stdin().read_line(&mut input) {
            Ok(0) => break, // EOF
            Ok(_) => {
                let input = input.trim();
                if input.is_empty() {
                    continue;
                }

                // 处理命令
                match input {
                    "/quit" | "/exit" | "/q" => {
                        println!("再见!");
                        break;
                    }
                    "/clear" => {
                        agent.clear().await;
                        println!("上下文已清空");
                        continue;
                    }
                    "/compact" => {
                        match agent.compact().await {
                            Ok(_) => println!("上下文已压缩"),
                            Err(e) => println!("压缩失败: {}", e),
                        }
                        continue;
                    }
                    "/stats" => {
                        let stats = agent.stats().await;
                        println!("上下文: {} tokens ({} 消息)", stats.current_context_tokens, stats.message_count);
                        println!("总使用: {} in + {} out = {} tokens", stats.total_input_tokens, stats.total_output_tokens, stats.total_tokens);
                        println!("预算: {}", stats.budget_status);
                        continue;
                    }
                    "/settings" => {
                        cmd_settings(&agent).await;
                        continue;
                    }
                    "/prompt" => {
                        let templates = Agent::list_prompt_templates();
                        println!("\n可用提示词模板:");
                        for (i, t) in templates.iter().enumerate() {
                            println!("  {}. {} - {}", i + 1, t.name, t.description);
                        }
                        print!("选择 (名称或编号): ");
                        std::io::stdout().flush().unwrap();
                        let mut val = String::new();
                        if std::io::stdin().read_line(&mut val).is_ok() {
                            let val = val.trim();
                            let name = if let Ok(n) = val.parse::<usize>() {
                                templates.get(n.saturating_sub(1)).map(|t| t.name)
                            } else {
                                Some(val)
                            };
                            if let Some(name) = name {
                                match agent.set_prompt_template(name).await {
                                    Ok(msg) => println!("✓ {}", msg),
                                    Err(e) => println!("✗ {}", e),
                                }
                            }
                        }
                        continue;
                    }
                    "/help" => {
                        println!("\n交互命令:");
                        println!("  /quit      - 退出");
                        println!("  /clear     - 清空上下文");
                        println!("  /compact   - 压缩上下文（省 Token）");
                        println!("  /stats     - 显示 Token 使用统计");
                        println!("  /settings  - 打开设置界面（API Key、模型、权限等）");
                        println!("  /prompt    - 切换系统提示词模板");
                        println!("  /help      - 显示帮助");
                        continue;
                    }
                    _ => {}
                }

                // 执行对话
                let start = std::time::Instant::now();
                match agent.run(input).await {
                    Ok(response) => {
                        println!("\n{}", response);
                        println!("\n[{}ms]", start.elapsed().as_millis());
                    }
                    Err(e) => {
                        println!("\n[错误] {}", e);
                    }
                }
            }
            Err(e) => {
                eprintln!("读取输入失败: {}", e);
                break;
            }
        }
    }
}

/// 单次提问
async fn cmd_single(message: String) {
    let agent = init_agent().await;
    let start = std::time::Instant::now();

    match agent.run(&message).await {
        Ok(response) => {
            println!("{}", response);
            eprintln!("\n[{}ms]", start.elapsed().as_millis());
        }
        Err(e) => {
            eprintln!("错误: {}", e);
            std::process::exit(1);
        }
    }
}

/// 启动 HTTP 服务器
async fn cmd_serve(host: String, port: u16) {
    let agent = init_agent().await;

    println!("启动 HTTP API 服务器...");
    println!("地址: http://{}:{}", host, port);
    println!("健康: http://{}:{}/health", host, port);

    if let Err(e) = http_api::serve(agent, &host, port).await {
        eprintln!("服务器错误: {}", e);
        std::process::exit(1);
    }
}

/// 诊断
async fn cmd_doctor() {
    let agent = init_agent().await;

    println!("运行诊断检查...\n");

    let results = agent.doctor();
    let mut all_ok = true;

    for r in &results {
        let icon = if r.status == "ok" { "✓" } else { "✗" };
        println!("  {} {:15} {}", icon, r.check, r.message);
        if let Some(fix) = &r.fix {
            println!("    修复: {}", fix);
        }
        if r.status != "ok" {
            all_ok = false;
        }
    }

    println!();
    if all_ok {
        println!("所有检查通过 ✓");
    } else {
        println!("部分检查未通过，请根据修复建议处理");
        std::process::exit(1);
    }
}

/// 列出模型
async fn cmd_models() {
    let agent = init_agent().await;
    let models = agent.list_models().await;

    println!("可用模型 ({}):\n", models.len());
    for m in models {
        println!("  {}/{} (max_tokens: {})", m.provider, m.name, m.max_tokens);
    }
}

/// 验证提供商
async fn cmd_verify() {
    let agent = init_agent().await;

    println!("正在验证模型提供商...");
    println!("(可能需要几秒到几十秒)\n");

    let results = agent.verify_providers().await;

    for v in &results {
        if let Some(err) = &v.error {
            println!("  ✗ {:15} 错误: {}", v.provider, err);
            continue;
        }

        let status = if v.verified { "✓" } else { "✗" };
        let mut features = Vec::new();
        if v.features.streaming { features.push("stream"); }
        if v.features.tool_calling { features.push("tools"); }
        if v.features.vision { features.push("vision"); }

        println!("  {} {:15} {:5}ms  {} models  [{}]",
            status,
            v.provider,
            v.latency_ms,
            v.models.len(),
            features.join(",")
        );
    }
}

/// 启动 TUI
async fn cmd_tui() {
    cmd_tui_with_opening(String::new()).await;
}

/// 启动 TUI，并把 opening 作为开场白首条消息（为空则不发）
async fn cmd_tui_with_opening(opening: String) {
    let agent = init_agent().await;
    if let Err(e) = tui::run(agent, opening) {
        eprintln!("TUI 错误: {}", e);
        std::process::exit(1);
    }
}

/// 初始化配置
async fn cmd_init() {
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let dir = home.join(".raven");

    match config_system::init_config(&dir) {
        Ok(_) => {
            println!("配置文件模板已创建: {}", dir.join("config.toml").display());
            println!("请编辑配置文件设置你的 API Key");
        }
        Err(e) => {
            eprintln!("错误: {}", e);
            std::process::exit(1);
        }
    }
}

/// 交互式设置界面（类似 Claude Code 的 /settings）
async fn cmd_settings(agent: &raven_core::Agent) {
    use std::io::Write;

    let cfg = agent.config();
    let config_path = dirs::home_dir()
        .map(|h| h.join(".raven").join("config.toml"))
        .unwrap_or_else(|| std::path::PathBuf::from("config.toml"));

    loop {
        let api_key_display = cfg.api_key.as_ref()
            .map(|k| format!("{}...{}", &k[..4.min(k.len())], &k[k.len().saturating_sub(4)..]))
            .unwrap_or_else(|| "未设置".to_string());
        let base_url_display = cfg.base_url.as_ref()
            .map(|u| truncate_24(u))
            .unwrap_or_else(|| "默认 (OpenAI)".to_string());

        println!("\n╔════════════════════════════════════════╗");
        println!("║           设置 (Settings)              ║");
        println!("╠════════════════════════════════════════╣");
        println!("║  [连接]                                ║");
        println!("║  1. API Key:  {:24} ║", api_key_display);
        println!("║  2. Base URL: {:24} ║", base_url_display);
        println!("║  3. 模型:     {:24} ║", truncate_24(&cfg.model));
        println!("║                                        ║");
        println!("║  [安全]                                ║");
        println!("║  4. 权限模式: {:24} ║", truncate_24(&cfg.permission.mode));
        println!("║  5. 允许工具: {:24} ║", truncate_24(&cfg.permission.allowed_tools.join(", ")));
        println!("║                                        ║");
        println!("║  [上下文]                              ║");
        println!("║  6. 上下文上限: {:22} ║", cfg.context.max_tokens);
        println!("║  7. 压缩阈值: {:24} ║", cfg.context.compact_threshold);
        println!("║  8. 保留轮数: {:24} ║", cfg.context.keep_rounds);
        println!("║  9. Token预算: {:23} ║", if cfg.token_budget == 0 { "无限制".to_string() } else { cfg.token_budget.to_string() });
        println!("║                                        ║");
        let git_first_status = if cfg.git_first.enabled {
            if cfg.git_first.auto_commit { "开启(自动)" } else { "开启(手动)" }
        } else {
            "关闭"
        };

        println!("║  [高级]                                ║");
        println!("║  10. 日志级别: {:23} ║", truncate_24(&cfg.log_level));
        println!("║  11. 管理提供商 ({}个)          ║", cfg.providers.len());
        println!("║  12. Git-first: {:22} ║", git_first_status);
        println!("║                                        ║");
        println!("║  s. 保存完整配置                       ║");
        println!("║  d. 诊断检查                           ║");
        println!("║  q. 返回                               ║");
        println!("╚════════════════════════════════════════╝");
        print!("\n选择要修改的项 (1-7, s, q): ");
        std::io::stdout().flush().unwrap();

        let mut choice = String::new();
        if std::io::stdin().read_line(&mut choice).is_err() {
            break;
        }

        match choice.trim() {
            // ---- 连接 ----
            "1" => {
                println!("当前 API Key: {}", api_key_display);
                print!("输入新 API Key (留空保持不变): ");
                std::io::stdout().flush().unwrap();
                let mut val = String::new();
                if std::io::stdin().read_line(&mut val).is_ok() {
                    let val = val.trim();
                    if !val.is_empty() {
                        if val.starts_with("sk-") || val.len() > 20 {
                            save_config_value(&config_path, "api_key", val);
                            println!("✓ API Key 已保存，重启后生效");
                        } else {
                            println!("✗ API Key 格式不正确");
                        }
                    }
                }
            }
            "2" => {
                println!("常用 Base URL:");
                println!("  https://api.openai.com/v1          (OpenAI)");
                println!("  https://api.deepseek.com/v1        (DeepSeek)");
                println!("  https://api.anthropic.com/v1       (Anthropic)");
                println!("  https://api.groq.com/openai/v1     (Groq)");
                println!("  https://api.siliconflow.cn/v1      (硅基流动)");
                print!("输入新 Base URL (留空清除): ");
                std::io::stdout().flush().unwrap();
                let mut val = String::new();
                if std::io::stdin().read_line(&mut val).is_ok() {
                    let val = val.trim();
                    if val.is_empty() {
                        save_config_value(&config_path, "base_url", "");
                        println!("✓ Base URL 已清除，使用默认");
                    } else if val.starts_with("http") {
                        save_config_value(&config_path, "base_url", val);
                        println!("✓ Base URL 已保存，重启后生效");
                    } else {
                        println!("✗ URL 必须以 http:// 或 https:// 开头");
                    }
                }
            }
            "3" => {
                println!("常用模型:");
                println!("  gpt-4o, gpt-4o-mini, gpt-4-turbo");
                println!("  deepseek-chat, deepseek-reasoner");
                println!("  claude-3-5-sonnet, claude-3-opus");
                println!("  qwen2.5-72b, llama3.3-70b");
                print!("输入新模型ID (当前: {}): ", cfg.model);
                std::io::stdout().flush().unwrap();
                let mut val = String::new();
                if std::io::stdin().read_line(&mut val).is_ok() {
                    let val = val.trim();
                    if !val.is_empty() {
                        save_config_value(&config_path, "model", val);
                        println!("✓ 模型已保存，重启后生效");
                    }
                }
            }
            // ---- 安全 ----
            "4" => {
                println!("权限模式选项:");
                println!("  ask      - 只允许预设工具（安全，推荐）");
                println!("  auto     - 允许所有工具");
                println!("  yes      - 允许所有工具（宽松）");
                println!("  readonly - 禁止所有工具（只读）");
                print!("输入新模式 (当前: {}): ", cfg.permission.mode);
                std::io::stdout().flush().unwrap();
                let mut val = String::new();
                if std::io::stdin().read_line(&mut val).is_ok() {
                    let val = val.trim();
                    let valid = ["ask", "auto", "yes", "readonly"];
                    if valid.contains(&val) {
                        save_config_section(&config_path, "permission", "mode", val);
                        println!("✓ 权限模式已保存，重启后生效");
                    } else if !val.is_empty() {
                        println!("✗ 无效的模式: {}", val);
                    }
                }
            }
            "5" => {
                println!("可用工具: file_read, file_write, shell, search, list_dir, git");
                print!("输入允许的工具，逗号分隔 (当前: {}): ", cfg.permission.allowed_tools.join(", "));
                std::io::stdout().flush().unwrap();
                let mut val = String::new();
                if std::io::stdin().read_line(&mut val).is_ok() {
                    let val = val.trim();
                    if !val.is_empty() {
                        let tools: Vec<&str> = val.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
                        let formatted = tools.iter().map(|s| format!("\"{}\"", s)).collect::<Vec<_>>().join(", ");
                        save_config_section(&config_path, "permission", "allowed_tools", &format!("[{}]", formatted));
                        println!("✓ 工具列表已保存，重启后生效");
                    }
                }
            }
            // ---- 上下文 ----
            "6" => {
                print!("输入新上下文上限 (当前: {}): ", cfg.context.max_tokens);
                std::io::stdout().flush().unwrap();
                let mut val = String::new();
                if std::io::stdin().read_line(&mut val).is_ok() {
                    let val = val.trim();
                    if let Ok(n) = val.parse::<usize>() {
                        if n >= 4096 {
                            save_config_section(&config_path, "context", "max_tokens", &n.to_string());
                            println!("✓ 已保存，重启后生效");
                        } else {
                            println!("✗ 值太小，必须 >= 4096");
                        }
                    } else if !val.is_empty() {
                        println!("✗ 无效的数字");
                    }
                }
            }
            "7" => {
                print!("输入新压缩阈值 (当前: {}): ", cfg.context.compact_threshold);
                std::io::stdout().flush().unwrap();
                let mut val = String::new();
                if std::io::stdin().read_line(&mut val).is_ok() {
                    let val = val.trim();
                    if let Ok(n) = val.parse::<usize>() {
                        save_config_section(&config_path, "context", "compact_threshold", &n.to_string());
                        println!("✓ 已保存，重启后生效");
                    } else if !val.is_empty() {
                        println!("✗ 无效的数字");
                    }
                }
            }
            "8" => {
                print!("输入新保留轮数 (当前: {}): ", cfg.context.keep_rounds);
                std::io::stdout().flush().unwrap();
                let mut val = String::new();
                if std::io::stdin().read_line(&mut val).is_ok() {
                    let val = val.trim();
                    if let Ok(n) = val.parse::<usize>() {
                        save_config_section(&config_path, "context", "keep_rounds", &n.to_string());
                        println!("✓ 已保存，重启后生效");
                    } else if !val.is_empty() {
                        println!("✗ 无效的数字");
                    }
                }
            }
            "9" => {
                print!("输入Token预算 (0=无限制, 当前: {}): ", cfg.token_budget);
                std::io::stdout().flush().unwrap();
                let mut val = String::new();
                if std::io::stdin().read_line(&mut val).is_ok() {
                    let val = val.trim();
                    if let Ok(n) = val.parse::<usize>() {
                        save_config_value(&config_path, "token_budget", &n.to_string());
                        println!("✓ 已保存，重启后生效");
                    } else if !val.is_empty() {
                        println!("✗ 无效的数字");
                    }
                }
            }
            // ---- 高级 ----
            "10" => {
                println!("日志级别选项: debug / info / warn / error");
                print!("输入新级别 (当前: {}): ", cfg.log_level);
                std::io::stdout().flush().unwrap();
                let mut val = String::new();
                if std::io::stdin().read_line(&mut val).is_ok() {
                    let val = val.trim();
                    let valid = ["debug", "info", "warn", "error"];
                    if valid.contains(&val) {
                        save_config_value(&config_path, "log_level", val);
                        println!("✓ 已保存，重启后生效");
                    } else if !val.is_empty() {
                        println!("✗ 无效的级别: {}", val);
                    }
                }
            }
            "11" => {
                manage_providers(&config_path, &cfg);
            }
            "12" => {
                println!("\nGit-first 设计: 每次编辑文件后自动执行 git add + git commit");
                println!("当前状态: {}", if cfg.git_first.enabled { if cfg.git_first.auto_commit { "开启（自动提交）" } else { "开启（手动提交）" } } else { "关闭" });
                println!("\n选项:");
                println!("  1. 开启 + 自动提交");
                println!("  2. 开启 + 手动提交（只 add，不自动 commit）");
                println!("  3. 关闭");
                print!("选择: ");
                std::io::stdout().flush().unwrap();
                let mut val = String::new();
                if std::io::stdin().read_line(&mut val).is_ok() {
                    match val.trim() {
                        "1" => {
                            save_config_section(&config_path, "git_first", "enabled", "true");
                            save_config_section(&config_path, "git_first", "auto_commit", "true");
                            println!("✓ Git-first 已开启（自动提交），重启后生效");
                        }
                        "2" => {
                            save_config_section(&config_path, "git_first", "enabled", "true");
                            save_config_section(&config_path, "git_first", "auto_commit", "false");
                            println!("✓ Git-first 已开启（手动提交），重启后生效");
                        }
                        "3" => {
                            save_config_section(&config_path, "git_first", "enabled", "false");
                            save_config_section(&config_path, "git_first", "auto_commit", "false");
                            println!("✓ Git-first 已关闭，重启后生效");
                        }
                        _ => println!("无效选择"),
                    }
                }
            }
            "s" | "S" => {
                match save_full_config(&config_path, &cfg) {
                    Ok(_) => println!("✓ 配置已保存到: {}", config_path.display()),
                    Err(e) => println!("✗ 保存失败: {}", e),
                }
            }
            "d" | "D" => {
                let results = agent.doctor();
                println!("\n诊断结果:");
                for r in &results {
                    let icon = if r.status == "ok" { "✓" } else { "✗" };
                    println!("  {} {}: {}", icon, r.check, r.message);
                }
            }
            "q" | "Q" | "" => break,
            _ => println!("无效选择，请输入 1-12, s, d, q"),
        }
    }
}

/// 管理提供商（添加/删除/查看）
fn manage_providers(config_path: &std::path::Path, cfg: &raven_types::Config) {
    use std::io::Write;
    loop {
        println!("\n╔════════════════════════════════════════╗");
        println!("║         提供商管理                     ║");
        println!("╠════════════════════════════════════════╣");
        if cfg.providers.is_empty() {
            println!("║  (无额外提供商)                        ║");
        } else {
            for (i, p) in cfg.providers.iter().enumerate() {
                println!("║  {}. {} @ {}", i + 1, p.name, truncate_24(&p.base_url));
            }
        }
        println!("║                                        ║");
        println!("║  a. 添加提供商                         ║");
        println!("║  d. 删除提供商                         ║");
        println!("║  q. 返回                               ║");
        println!("╚════════════════════════════════════════╝");
        print!("\n选择: ");
        std::io::stdout().flush().unwrap();

        let mut choice = String::new();
        if std::io::stdin().read_line(&mut choice).is_err() {
            break;
        }

        match choice.trim() {
            "a" | "A" => {
                print!("提供商名称 (如 deepseek): ");
                std::io::stdout().flush().unwrap();
                let mut name = String::new();
                std::io::stdin().read_line(&mut name).unwrap();
                let name = name.trim();
                if name.is_empty() { continue; }

                print!("Base URL (如 https://api.deepseek.com/v1): ");
                std::io::stdout().flush().unwrap();
                let mut url = String::new();
                std::io::stdin().read_line(&mut url).unwrap();
                let url = url.trim();
                if url.is_empty() { continue; }

                print!("API Key: ");
                std::io::stdout().flush().unwrap();
                let mut key = String::new();
                std::io::stdin().read_line(&mut key).unwrap();
                let key = key.trim();

                print!("模型列表 (逗号分隔，如 deepseek-chat,deepseek-reasoner): ");
                std::io::stdout().flush().unwrap();
                let mut models = String::new();
                std::io::stdin().read_line(&mut models).unwrap();
                let models_list: Vec<String> = models.trim().split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();

                // 追加到配置文件
                let provider_toml = format!(r#"
[[providers]]
name = "{}"
base_url = "{}"
api_key = "{}"
models = [{}]
"#,
                    name, url, key,
                    models_list.iter().map(|m| format!("\"{}\"", m)).collect::<Vec<_>>().join(", ")
                );

                let content = std::fs::read_to_string(config_path).unwrap_or_default();
                let _ = std::fs::write(config_path, format!("{}\n{}", content.trim_end(), provider_toml));
                println!("✓ 提供商 '{}' 已添加，重启后生效", name);
            }
            "d" | "D" => {
                if cfg.providers.is_empty() {
                    println!("没有可删除的提供商");
                    continue;
                }
                print!("输入要删除的编号 (1-{}): ", cfg.providers.len());
                std::io::stdout().flush().unwrap();
                let mut num = String::new();
                std::io::stdin().read_line(&mut num).unwrap();
                if let Ok(n) = num.trim().parse::<usize>() {
                    if n > 0 && n <= cfg.providers.len() {
                        println!("删除提供商 '{}'，重启后生效", cfg.providers[n - 1].name);
                        // 注：简单实现是让用户手动编辑文件
                        println!("(请手动编辑 {} 删除对应 [[providers]] 段)", config_path.display());
                    }
                }
            }
            "q" | "Q" | "" => break,
            _ => {}
        }
    }
}

/// 截断字符串到24字符
fn truncate_24(s: &str) -> String {
    if s.chars().count() > 24 {
        s.chars().take(21).collect::<String>() + "..."
    } else {
        s.to_string()
    }
}

/// 保存顶层配置项
fn save_config_value(path: &std::path::Path, key: &str, value: &str) {
    let content = if path.exists() {
        std::fs::read_to_string(path).unwrap_or_default()
    } else {
        String::new()
    };

    let new_line = format!("{} = \"{}\"", key, value);
    let updated = if let Some(pos) = content.find(&format!("{} = ", key)) {
        let before = &content[..pos];
        let after_start = &content[pos..];
        if let Some(nl) = after_start.find('\n') {
            format!("{}{}\n{}", before, new_line, &content[pos + nl + 1..])
        } else {
            format!("{}{}", before, new_line)
        }
    } else {
        if content.is_empty() {
            format!("# Raven 配置\n{}\n", new_line)
        } else {
            format!("{}\n{}\n", content.trim_end(), new_line)
        }
    };

    let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")));
    let _ = std::fs::write(path, updated);
}

/// 保存 [section] 下的配置项
fn save_config_section(path: &std::path::Path, section: &str, key: &str, value: &str) {
    let content = if path.exists() {
        std::fs::read_to_string(path).unwrap_or_default()
    } else {
        String::new()
    };

    let section_header = format!("[{}]", section);
    let new_line = format!("{} = \"{}\"", key, value);

    let updated = if content.contains(&section_header) {
        let section_start = content.find(&section_header).unwrap();
        let after_header = &content[section_start + section_header.len()..];

        if let Some(key_pos) = after_header.find(&format!("{} = ", key)) {
            let abs_key_pos = section_start + section_header.len() + key_pos;
            let before = &content[..abs_key_pos];
            let after_key = &content[abs_key_pos..];
            if let Some(nl) = after_key.find('\n') {
                format!("{}{}\n{}", before, new_line, &content[abs_key_pos + nl + 1..])
            } else {
                format!("{}{}", before, new_line)
            }
        } else {
            let next_section = after_header.find('[').unwrap_or(after_header.len());
            let insert_pos = section_start + section_header.len() + next_section;
            let before = &content[..insert_pos];
            let after = &content[insert_pos..];
            format!("{}\n{}{}", before, new_line, after)
        }
    } else {
        format!("{}\n{}\n{}\n", content.trim_end(), section_header, new_line)
    };

    let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")));
    let _ = std::fs::write(path, updated);
}

/// 保存完整配置
fn save_full_config(path: &std::path::Path, cfg: &raven_types::Config) -> Result<(), String> {
    let content = toml::to_string_pretty(cfg)
        .map_err(|e| format!("序列化失败: {}", e))?;
    std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")))
        .map_err(|e| format!("创建目录失败: {}", e))?;
    std::fs::write(path, content)
        .map_err(|e| format!("写入失败: {}", e))?;
    Ok(())
}
