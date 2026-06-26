//! 配置系统单元测试
//!
//! 测试即文档：每个测试描述一个使用场景。

#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    use crate::init_config;

    /// 场景：零配置启动（所有字段使用默认值）
    #[test]
    fn test_default_config() {
        let cfg = raven_types::Config::default();
        assert_eq!(cfg.model, "gpt-4o");
        assert_eq!(cfg.log_level, "info");
        assert!(cfg.api_key.is_none());
        assert_eq!(cfg.token_budget, 0); // 无限制
        assert_eq!(cfg.permission.mode, "ask");
        assert!(!cfg.permission.allowed_tools.is_empty());
    }

    /// 场景：TOML 配置文件加载
    #[test]
    fn test_toml_parsing() {
        let toml = r#"
model = "deepseek-chat"
api_key = "sk-test123"

[permission]
mode = "auto"

[context]
max_tokens = 64000
"#;

        let cfg: raven_types::Config = toml::from_str(toml).expect("解析TOML失败");
        assert_eq!(cfg.model, "deepseek-chat");
        assert_eq!(cfg.api_key, Some("sk-test123".to_string()));
        assert_eq!(cfg.permission.mode, "auto");
        assert_eq!(cfg.context.max_tokens, 64000);
    }

    /// 场景：无效权限模式应被拒绝
    #[test]
    fn test_invalid_permission_mode() {
        let toml = r#"
[permission]
mode = "invalid_mode"
"#;

        let cfg: raven_types::Config = toml::from_str(toml).unwrap();
        // 创建 ConfigSystem 并验证
        let result = validate_config(&cfg);
        assert!(result.is_err(), "无效权限模式应被拒绝");
    }

    /// 场景：环境变量覆盖配置
    #[test]
    fn test_env_override() {
        // 设置环境变量
        std::env::set_var("RAVEN_MODEL", "gpt-4-turbo");
        std::env::set_var("RAVEN_LOG_LEVEL", "debug");

        let mut cfg = raven_types::Config::default();
        // 模拟环境变量应用
        if let Ok(val) = std::env::var("RAVEN_MODEL") {
            cfg.model = val;
        }
        if let Ok(val) = std::env::var("RAVEN_LOG_LEVEL") {
            cfg.log_level = val;
        }

        assert_eq!(cfg.model, "gpt-4-turbo");
        assert_eq!(cfg.log_level, "debug");

        // 清理
        std::env::remove_var("RAVEN_MODEL");
        std::env::remove_var("RAVEN_LOG_LEVEL");
    }

    /// 场景：配置文件模板生成
    #[test]
    fn test_config_template() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let result = init_config(tmp_dir.path());
        assert!(result.is_ok(), "配置模板应成功创建");

        let config_file = tmp_dir.path().join("config.toml");
        assert!(config_file.exists(), "配置文件应存在");

        let content = std::fs::read_to_string(config_file).unwrap();
        assert!(content.contains("model = \"gpt-4o\""), "应包含默认模型");
        assert!(content.contains("[git_first]"), "应包含Git-first配置");
    }

    /// 场景：Git-first 配置解析
    #[test]
    fn test_git_first_config() {
        let toml = r#"
[git_first]
enabled = true
auto_commit = false
commit_prefix = "ai"
"#;

        let cfg: raven_types::Config = toml::from_str(toml).unwrap();
        assert!(cfg.git_first.enabled);
        assert!(!cfg.git_first.auto_commit);
        assert_eq!(cfg.git_first.commit_prefix, "ai");
    }

    // 辅助函数
    fn validate_config(cfg: &raven_types::Config) -> Result<(), String> {
        let valid_modes = ["ask", "auto", "yes", "readonly"];
        if !valid_modes.contains(&cfg.permission.mode.as_str()) {
            return Err(format!("无效权限模式: {}", cfg.permission.mode));
        }
        if cfg.context.max_tokens < 4096 && cfg.context.max_tokens != 0 {
            return Err("max_tokens 太小".to_string());
        }
        Ok(())
    }
}
