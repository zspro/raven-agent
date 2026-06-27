//! 交互式设置界面与配置文件写入

use crate::config_io::{save_config_section, save_config_value, save_full_config, truncate_24};

/// 交互式设置界面（类似 Claude Code 的 /settings）
pub(crate) async fn cmd_settings(agent: &raven_core::Agent) {
    use std::io::Write;

    let cfg = agent.config();
    let config_path = dirs::home_dir()
        .map(|h| h.join(".raven").join("config.toml"))
        .unwrap_or_else(|| std::path::PathBuf::from("config.toml"));

    loop {
        let api_key_display = cfg
            .api_key
            .as_ref()
            .map(|k| {
                format!(
                    "{}...{}",
                    &k[..4.min(k.len())],
                    &k[k.len().saturating_sub(4)..]
                )
            })
            .unwrap_or_else(|| "未设置".to_string());
        let base_url_display = cfg
            .base_url
            .as_ref()
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
        println!(
            "║  5. 允许工具: {:24} ║",
            truncate_24(&cfg.permission.allowed_tools.join(", "))
        );
        println!("║                                        ║");
        println!("║  [上下文]                              ║");
        println!("║  6. 上下文上限: {:22} ║", cfg.context.max_tokens);
        println!("║  7. 压缩阈值: {:24} ║", cfg.context.compact_threshold);
        println!("║  8. 保留轮数: {:24} ║", cfg.context.keep_rounds);
        println!(
            "║  9. Token预算: {:23} ║",
            if cfg.token_budget == 0 {
                "无限制".to_string()
            } else {
                cfg.token_budget.to_string()
            }
        );
        println!("║                                        ║");
        let git_first_status = if cfg.git_first.enabled {
            if cfg.git_first.auto_commit {
                "开启(自动)"
            } else {
                "开启(手动)"
            }
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
                            println!("✓ API Key 已保存，退出设置后生效");
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
                        println!("✓ Base URL 已保存，退出设置后生效");
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
                        println!("✓ 模型已保存，退出设置后生效");
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
                        println!("✓ 权限模式已保存，退出设置后生效");
                    } else if !val.is_empty() {
                        println!("✗ 无效的模式: {}", val);
                    }
                }
            }
            "5" => {
                println!("可用工具: file_read, file_write, shell, search, list_dir, git");
                print!(
                    "输入允许的工具，逗号分隔 (当前: {}): ",
                    cfg.permission.allowed_tools.join(", ")
                );
                std::io::stdout().flush().unwrap();
                let mut val = String::new();
                if std::io::stdin().read_line(&mut val).is_ok() {
                    let val = val.trim();
                    if !val.is_empty() {
                        let tools: Vec<&str> = val
                            .split(',')
                            .map(|s| s.trim())
                            .filter(|s| !s.is_empty())
                            .collect();
                        let formatted = tools
                            .iter()
                            .map(|s| format!("\"{}\"", s))
                            .collect::<Vec<_>>()
                            .join(", ");
                        save_config_section(
                            &config_path,
                            "permission",
                            "allowed_tools",
                            &format!("[{}]", formatted),
                        );
                        println!("✓ 工具列表已保存，退出设置后生效");
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
                            save_config_section(
                                &config_path,
                                "context",
                                "max_tokens",
                                &n.to_string(),
                            );
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
                        save_config_section(
                            &config_path,
                            "context",
                            "compact_threshold",
                            &n.to_string(),
                        );
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
                println!(
                    "当前状态: {}",
                    if cfg.git_first.enabled {
                        if cfg.git_first.auto_commit {
                            "开启（自动提交）"
                        } else {
                            "开启（手动提交）"
                        }
                    } else {
                        "关闭"
                    }
                );
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
                            println!("✓ Git-first 已开启（自动提交），退出设置后生效");
                        }
                        "2" => {
                            save_config_section(&config_path, "git_first", "enabled", "true");
                            save_config_section(&config_path, "git_first", "auto_commit", "false");
                            println!("✓ Git-first 已开启（手动提交），退出设置后生效");
                        }
                        "3" => {
                            save_config_section(&config_path, "git_first", "enabled", "false");
                            save_config_section(&config_path, "git_first", "auto_commit", "false");
                            println!("✓ Git-first 已关闭，退出设置后生效");
                        }
                        _ => println!("无效选择"),
                    }
                }
            }
            "s" | "S" => match save_full_config(&config_path, &cfg) {
                Ok(_) => println!("✓ 配置已保存到: {}", config_path.display()),
                Err(e) => println!("✗ 保存失败: {}", e),
            },
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

    // 退出设置时重新加载配置并应用到运行中的 Agent，使本次改动立即生效
    // （无需等待热重载的 5 秒轮询，也无需重启）。
    match config_system::ConfigSystem::load() {
        Ok(sys) => {
            agent.apply_config(sys.config().clone()).await;
            println!("\x1b[2m配置已即时生效。\x1b[0m");
        }
        Err(e) => {
            println!(
                "\x1b[33m配置重新加载失败（{}），改动将在下次启动生效。\x1b[0m",
                e
            );
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
                if name.is_empty() {
                    continue;
                }

                print!("Base URL (如 https://api.deepseek.com/v1): ");
                std::io::stdout().flush().unwrap();
                let mut url = String::new();
                std::io::stdin().read_line(&mut url).unwrap();
                let url = url.trim();
                if url.is_empty() {
                    continue;
                }

                print!("API Key: ");
                std::io::stdout().flush().unwrap();
                let mut key = String::new();
                std::io::stdin().read_line(&mut key).unwrap();
                let key = key.trim();

                print!("模型列表 (逗号分隔，如 deepseek-chat,deepseek-reasoner): ");
                std::io::stdout().flush().unwrap();
                let mut models = String::new();
                std::io::stdin().read_line(&mut models).unwrap();
                let models_list: Vec<String> = models
                    .trim()
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();

                // 追加到配置文件
                let provider_toml = format!(
                    r#"
[[providers]]
name = "{}"
base_url = "{}"
api_key = "{}"
models = [{}]
"#,
                    name,
                    url,
                    key,
                    models_list
                        .iter()
                        .map(|m| format!("\"{}\"", m))
                        .collect::<Vec<_>>()
                        .join(", ")
                );

                let content = std::fs::read_to_string(config_path).unwrap_or_default();
                let _ = std::fs::write(
                    config_path,
                    format!("{}\n{}", content.trim_end(), provider_toml),
                );
                println!("✓ 提供商 '{}' 已添加，退出设置后生效", name);
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
                        println!("删除提供商 '{}'，退出设置后生效", cfg.providers[n - 1].name);
                        // 注：简单实现是让用户手动编辑文件
                        println!(
                            "(请手动编辑 {} 删除对应 [[providers]] 段)",
                            config_path.display()
                        );
                    }
                }
            }
            "q" | "Q" | "" => break,
            _ => {}
        }
    }
}
