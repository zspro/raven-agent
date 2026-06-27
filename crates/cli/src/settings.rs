//! 交互式设置界面与配置文件写入

use crate::config_io::{
    save_config_section, save_config_section_raw, save_config_value, save_config_value_raw,
    save_full_config, truncate_24,
};

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
        let mp = &cfg.model_params;
        let fmt_opt_f = |v: Option<f32>| v.map(|x| x.to_string()).unwrap_or_else(|| "默认".into());
        let fmt_opt_u = |v: Option<u32>| v.map(|x| x.to_string()).unwrap_or_else(|| "默认".into());
        println!("║  [模型参数]                            ║");
        println!("║  13. temperature: {:20} ║", fmt_opt_f(mp.temperature));
        println!("║  14. max_tokens:  {:20} ║", fmt_opt_u(mp.max_tokens));
        println!("║  15. top_p:       {:20} ║", fmt_opt_f(mp.top_p));
        println!(
            "║  16. freq_penalty:{:20} ║",
            fmt_opt_f(mp.frequency_penalty)
        );
        println!(
            "║  17. pres_penalty:{:20} ║",
            fmt_opt_f(mp.presence_penalty)
        );
        println!("║                                        ║");
        println!("║  [界面]                                ║");
        let preview_display = if cfg.tui.preview_lines == 0 {
            "不折叠".to_string()
        } else {
            format!("{} 行", cfg.tui.preview_lines)
        };
        println!("║  18. 工具输出折叠: {:19} ║", preview_display);
        println!("║                                        ║");
        let api = &cfg.api;
        println!("║  [API]                                 ║");
        println!("║  19. 请求超时: {}s{:20} ║", api.timeout, "");
        println!("║  20. 重试次数: {:23} ║", api.max_retries);
        println!(
            "║  21. 流式输出: {:23} ║",
            if api.stream { "开启" } else { "关闭" }
        );
        println!("║                                        ║");
        println!("║  s. 保存完整配置                       ║");
        println!("║  d. 诊断检查                           ║");
        println!("║  q. 返回                               ║");
        println!("╚════════════════════════════════════════╝");
        print!("\n选择要修改的项 (1-21, s, q): ");
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
                        save_config_section_raw(
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
                            save_config_section_raw(
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
                        save_config_section_raw(
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
                        save_config_section_raw(
                            &config_path,
                            "context",
                            "keep_rounds",
                            &n.to_string(),
                        );
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
                        save_config_value_raw(&config_path, "token_budget", &n.to_string());
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
                            save_config_section_raw(&config_path, "git_first", "enabled", "true");
                            save_config_section_raw(
                                &config_path,
                                "git_first",
                                "auto_commit",
                                "true",
                            );
                            println!("✓ Git-first 已开启（自动提交），退出设置后生效");
                        }
                        "2" => {
                            save_config_section_raw(&config_path, "git_first", "enabled", "true");
                            save_config_section_raw(
                                &config_path,
                                "git_first",
                                "auto_commit",
                                "false",
                            );
                            println!("✓ Git-first 已开启（手动提交），退出设置后生效");
                        }
                        "3" => {
                            save_config_section_raw(&config_path, "git_first", "enabled", "false");
                            save_config_section_raw(
                                &config_path,
                                "git_first",
                                "auto_commit",
                                "false",
                            );
                            println!("✓ Git-first 已关闭，退出设置后生效");
                        }
                        _ => println!("无效选择"),
                    }
                }
            }
            // ---- 模型参数 ----
            "13" => {
                read_model_param_f32(
                    &config_path,
                    "temperature",
                    "采样温度 (0~2，越高越随机)",
                    cfg.model_params.temperature,
                    0.0,
                    2.0,
                );
            }
            "14" => {
                print!(
                    "输入 max_tokens 单次最大生成 (当前: {}): ",
                    cfg.model_params
                        .max_tokens
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "默认".into())
                );
                std::io::stdout().flush().unwrap();
                let mut val = String::new();
                if std::io::stdin().read_line(&mut val).is_ok() {
                    let val = val.trim();
                    if let Ok(n) = val.parse::<u32>() {
                        if n > 0 {
                            save_config_section_raw(
                                &config_path,
                                "model_params",
                                "max_tokens",
                                &n.to_string(),
                            );
                            println!("✓ 已保存，退出设置后生效");
                        } else {
                            println!("✗ 必须 > 0");
                        }
                    } else if !val.is_empty() {
                        println!("✗ 无效的数字");
                    }
                }
            }
            "15" => {
                read_model_param_f32(
                    &config_path,
                    "top_p",
                    "核采样 top_p (0~1)",
                    cfg.model_params.top_p,
                    0.0,
                    1.0,
                );
            }
            "16" => {
                read_model_param_f32(
                    &config_path,
                    "frequency_penalty",
                    "频率惩罚 (-2~2)",
                    cfg.model_params.frequency_penalty,
                    -2.0,
                    2.0,
                );
            }
            "17" => {
                read_model_param_f32(
                    &config_path,
                    "presence_penalty",
                    "存在惩罚 (-2~2)",
                    cfg.model_params.presence_penalty,
                    -2.0,
                    2.0,
                );
            }
            // ---- 界面 ----
            "18" => {
                print!(
                    "工具输出折叠行数 (0=不折叠, 当前: {}): ",
                    cfg.tui.preview_lines
                );
                std::io::stdout().flush().unwrap();
                let mut val = String::new();
                if std::io::stdin().read_line(&mut val).is_ok() {
                    let val = val.trim();
                    if let Ok(n) = val.parse::<usize>() {
                        save_config_section_raw(
                            &config_path,
                            "tui",
                            "preview_lines",
                            &n.to_string(),
                        );
                        println!("✓ 已保存，退出设置后生效");
                    } else if !val.is_empty() {
                        println!("✗ 无效的数字");
                    }
                }
            }
            // ---- API ----
            "19" => {
                print!("请求超时秒数 (当前: {}): ", cfg.api.timeout);
                std::io::stdout().flush().unwrap();
                let mut val = String::new();
                if std::io::stdin().read_line(&mut val).is_ok() {
                    let val = val.trim();
                    if let Ok(n) = val.parse::<u64>() {
                        if n > 0 {
                            save_config_section_raw(&config_path, "api", "timeout", &n.to_string());
                            println!("✓ 已保存，退出设置后生效");
                        } else {
                            println!("✗ 必须 > 0");
                        }
                    } else if !val.is_empty() {
                        println!("✗ 无效的数字");
                    }
                }
            }
            "20" => {
                print!("失败重试次数 (0=不重试, 当前: {}): ", cfg.api.max_retries);
                std::io::stdout().flush().unwrap();
                let mut val = String::new();
                if std::io::stdin().read_line(&mut val).is_ok() {
                    let val = val.trim();
                    if let Ok(n) = val.parse::<u32>() {
                        save_config_section_raw(&config_path, "api", "max_retries", &n.to_string());
                        println!("✓ 已保存，退出设置后生效");
                    } else if !val.is_empty() {
                        println!("✗ 无效的数字");
                    }
                }
            }
            "21" => {
                println!("流式输出 (SSE)：开启=逐字显示；关闭=整段返回（兼容不支持流式的端点）");
                print!(
                    "输入 on 开启 / off 关闭 (当前: {}): ",
                    if cfg.api.stream { "开启" } else { "关闭" }
                );
                std::io::stdout().flush().unwrap();
                let mut val = String::new();
                if std::io::stdin().read_line(&mut val).is_ok() {
                    match val.trim().to_lowercase().as_str() {
                        "on" | "true" | "1" | "开启" => {
                            save_config_section_raw(&config_path, "api", "stream", "true");
                            println!("✓ 流式已开启，退出设置后生效");
                        }
                        "off" | "false" | "0" | "关闭" => {
                            save_config_section_raw(&config_path, "api", "stream", "false");
                            println!("✓ 流式已关闭，退出设置后生效");
                        }
                        "" => {}
                        _ => println!("✗ 请输入 on 或 off"),
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
            _ => println!("无效选择，请输入 1-21, s, d, q"),
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

/// 读取一个 f32 模型参数并写入 `[model_params]`（带范围校验）。空输入保持不变。
fn read_model_param_f32(
    config_path: &std::path::Path,
    key: &str,
    desc: &str,
    current: Option<f32>,
    min: f32,
    max: f32,
) {
    use std::io::Write;
    let cur = current
        .map(|v| v.to_string())
        .unwrap_or_else(|| "默认".into());
    print!("输入 {} (当前: {}): ", desc, cur);
    std::io::stdout().flush().unwrap();
    let mut val = String::new();
    if std::io::stdin().read_line(&mut val).is_ok() {
        let val = val.trim();
        if val.is_empty() {
            return;
        }
        match val.parse::<f32>() {
            Ok(n) if n >= min && n <= max => {
                save_config_section_raw(config_path, "model_params", key, &n.to_string());
                println!("✓ 已保存，退出设置后生效");
            }
            Ok(_) => println!("✗ 超出范围 [{}, {}]", min, max),
            Err(_) => println!("✗ 无效的数字"),
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
                        let name = &cfg.providers[n - 1].name;
                        if remove_provider_block(config_path, name) {
                            println!("✓ 提供商 '{}' 已删除，退出设置后生效", name);
                        } else {
                            println!("✗ 未在配置文件中找到 '{}' 对应的段", name);
                        }
                    } else {
                        println!("✗ 编号超出范围");
                    }
                }
            }
            "q" | "Q" | "" => break,
            _ => {}
        }
    }
}

/// 从配置文件中删除 name 匹配的 `[[providers]]` 段。删除成功返回 true。
///
/// 按行扫描：定位每个 `[[providers]]` header，在其块内（到下一个以 `[` 开头的
/// section/数组表 header 或文件末尾）查找 `name = "<目标>"`，命中则连 header 一并删除。
/// 只删第一个匹配，避免误删同名块以外的内容。
fn remove_provider_block(path: &std::path::Path, target: &str) -> bool {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let lines: Vec<&str> = content.lines().collect();
    let is_section = |l: &str| l.trim_start().starts_with('[');

    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == "[[providers]]" {
            // 该块范围：header 之后到下一个 section header（或文件末尾）
            let mut end = lines.len();
            for (j, l) in lines.iter().enumerate().skip(i + 1) {
                if is_section(l) {
                    end = j;
                    break;
                }
            }
            // 块内匹配 name
            let matched = (i + 1..end).any(|k| {
                let t = lines[k].trim();
                t.starts_with("name") && t.contains(&format!("\"{}\"", target))
            });
            if matched {
                let mut kept: Vec<&str> = Vec::new();
                kept.extend_from_slice(&lines[..i]);
                kept.extend_from_slice(&lines[end..]);
                let mut out = kept.join("\n");
                if !out.is_empty() {
                    out.push('\n');
                }
                return std::fs::write(path, out).is_ok();
            }
            i = end;
        } else {
            i += 1;
        }
    }
    false
}
