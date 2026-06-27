//! 各子命令实现与 Agent 初始化、热重载、崩溃恢复

use crate::settings::cmd_settings;
use raven_core::Agent;
use std::sync::Arc;

// PLACEHOLDER_INIT

/// 初始化 Agent
pub(crate) async fn init_agent() -> Arc<Agent> {
    let cfg_sys = config_system::ConfigSystem::load().unwrap_or_else(|e| {
        eprintln!("配置错误: {}", e);
        std::process::exit(1);
    });

    let mut agent = Agent::from_config(&cfg_sys).await.unwrap_or_else(|e| {
        eprintln!("初始化失败: {}", e);
        std::process::exit(1);
    });

    // 注入终端确认器：ask 模式下执行敏感工具前实时询问 y/n/a
    agent.set_confirmer(Arc::new(raven_core::StdinConfirmer));

    // 应用环境感知的系统提示词：拼接平台/Shell/工作目录 + 工具清单，
    // 让模型在 Windows 下用 dir/cd 而非 pwd/ls。
    agent.apply_system_prompt(None).await;

    Arc::new(agent)
}

/// 为长时运行的场景（交互/serve/tui）启动配置热重载。
///
/// 检测到 `~/.raven/config.toml` 或项目 `.raven/config.toml` 变更后，
/// 重新加载并调用 `Agent::apply_config`，让切模型/调权限/改 API Key 无需重启即生效。
/// 单次执行（cmd_single）不需要，故不在 init_agent 内部启动。
pub(crate) fn start_hot_reload(agent: &Arc<Agent>) {
    use std::sync::Arc as StdArc;
    use tokio::sync::RwLock as TokioRwLock;

    let cfg_sys = match config_system::ConfigSystem::load() {
        Ok(c) => c,
        Err(_) => return, // 加载失败就不启热重载，不影响主流程
    };
    let shared = StdArc::new(TokioRwLock::new(cfg_sys));
    let agent = agent.clone();
    let callback: config_system::hot_reload::ReloadCallback = StdArc::new(move |new_cfg| {
        let agent = agent.clone();
        Box::pin(async move {
            agent.apply_config(new_cfg).await;
        })
    });
    config_system::hot_reload::spawn_hot_reload_with_callback(shared, callback);
}

/// 启动时检查是否有未完成的会话 checkpoint，交互式询问用户是否恢复。
/// 非 TTY 环境直接跳过（不阻塞管道/CI）。
async fn maybe_recover(agent: &Arc<Agent>) {
    use std::io::{IsTerminal, Write};

    if !std::io::stdin().is_terminal() {
        return;
    }
    if !agent.has_recoverable().await {
        return;
    }

    print!("\x1b[33m发现上次未完成的会话，是否恢复？[Y/n] \x1b[0m");
    let _ = std::io::stdout().flush();

    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return;
    }
    let ans = line.trim().to_lowercase();
    if ans.is_empty() || ans == "y" || ans == "yes" {
        let n = agent.recover_checkpoint().await;
        println!("\x1b[2m已恢复 {} 条历史消息。\x1b[0m\n", n);
    } else {
        println!("\x1b[2m已跳过恢复，开始新会话。\x1b[0m\n");
    }
}
// PLACEHOLDER_CHAT

/// 交互式对话
pub(crate) async fn cmd_chat() {
    cmd_chat_with_opening(String::new()).await;
}

/// 交互式对话；若 `opening` 非空，先把它作为第一句发出去再进入循环。
pub(crate) async fn cmd_chat_with_opening(opening: String) {
    let agent = init_agent().await;
    start_hot_reload(&agent);

    // 青色加粗标题 + 暗色命令提示，避免宽字符撑破对齐的方框
    println!("\n  \x1b[1;36mRaven 🐦‍⬛\x1b[0m  \x1b[2m交互式对话\x1b[0m");
    println!("  \x1b[2m/quit 退出 · /clear 清空 · /compact 压缩 · /stats 统计 · /settings 设置 · /prompt 切换角色\x1b[0m\n");

    // 崩溃恢复：发现上次未完成的会话时，询问是否恢复
    maybe_recover(&agent).await;

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
                        println!(
                            "上下文: {} tokens ({} 消息)",
                            stats.current_context_tokens, stats.message_count
                        );
                        println!(
                            "总使用: {} in + {} out = {} tokens",
                            stats.total_input_tokens, stats.total_output_tokens, stats.total_tokens
                        );
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
                            let name: Option<&str> = if let Ok(n) = val.parse::<usize>() {
                                templates.get(n.saturating_sub(1)).map(|t| t.name.as_str())
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
// PLACEHOLDER_REST

/// 单次提问
pub(crate) async fn cmd_single(message: String) {
    let agent = init_agent().await;
    let start = std::time::Instant::now();

    match agent.run(&message).await {
        Ok(response) => {
            // 用容错写入：下游管道提前关闭（如 `raven -p x | head`）时
            // println! 会 panic，这里把 BrokenPipe 当作正常退出处理。
            use std::io::Write;
            let mut out = std::io::stdout();
            if let Err(e) = writeln!(out, "{}", response) {
                if e.kind() == std::io::ErrorKind::BrokenPipe {
                    return;
                }
            }
            let _ = out.flush();
            eprintln!("\n[{}ms]", start.elapsed().as_millis());
        }
        Err(e) => {
            eprintln!("错误: {}", e);
            std::process::exit(1);
        }
    }
}

/// 启动 HTTP 服务器
pub(crate) async fn cmd_serve(host: String, port: u16) {
    let agent = init_agent().await;
    start_hot_reload(&agent);

    println!("启动 HTTP API 服务器...");
    println!("地址: http://{}:{}", host, port);
    println!("健康: http://{}:{}/health", host, port);

    if let Err(e) = http_api::serve(agent, &host, port).await {
        eprintln!("服务器错误: {}", e);
        std::process::exit(1);
    }
}

/// 诊断
pub(crate) async fn cmd_doctor() {
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
pub(crate) async fn cmd_models() {
    let agent = init_agent().await;
    let models = agent.list_models().await;

    println!("可用模型 ({}):\n", models.len());
    for m in models {
        println!("  {}/{} (max_tokens: {})", m.provider, m.name, m.max_tokens);
    }
}

/// 验证提供商
pub(crate) async fn cmd_verify() {
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
        if v.features.streaming {
            features.push("stream");
        }
        if v.features.tool_calling {
            features.push("tools");
        }
        if v.features.vision {
            features.push("vision");
        }

        println!(
            "  {} {:15} {:5}ms  {} models  [{}]",
            status,
            v.provider,
            v.latency_ms,
            v.models.len(),
            features.join(",")
        );
    }
}

/// 启动 TUI
pub(crate) async fn cmd_tui() {
    cmd_tui_with_opening(String::new()).await;
}

/// 启动 TUI，并把 opening 作为开场白首条消息（为空则不发）
pub(crate) async fn cmd_tui_with_opening(opening: String) {
    let agent = init_agent().await;
    start_hot_reload(&agent);
    if let Err(e) = tui::run(agent, opening) {
        eprintln!("TUI 错误: {}", e);
        std::process::exit(1);
    }
}

/// 初始化配置
pub(crate) async fn cmd_init() {
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
