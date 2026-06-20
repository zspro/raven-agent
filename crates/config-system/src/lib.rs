//! # config-system
//!
//! 强类型、分层、即时验证的配置管理。
//! 吸取 OpenClaw 的教训：配置项不要过多，验证要在加载时就做。

pub mod hot_reload;
pub mod prompts;
pub mod platform;

#[cfg(test)]
mod tests;

pub use platform::Platform;

use raven_types::Config;
use std::path::PathBuf;
use tracing::{debug, info};

/// 配置系统
pub struct ConfigSystem {
    config: Config,
    loaded_from: Option<PathBuf>,
}

impl ConfigSystem {
    /// 加载配置
    /// 加载顺序（后加载的覆盖先加载的）：
    /// 1. 内置默认值
    /// 2. 全局配置文件 (~/.raven/config.toml)
    /// 3. 项目级配置文件 (./.raven/config.toml)
    /// 4. 环境变量
    pub fn load() -> Result<Self, raven_types::AgentError> {
        let mut system = Self {
            config: Config::default(),
            loaded_from: None,
        };

        // 2. 加载全局配置
        if let Some(home) = dirs::home_dir() {
            let global = home.join(".raven").join("config.toml");
            if let Err(e) = system.load_file(&global) {
                debug!("全局配置不存在或无效: {}", e);
            } else {
                system.loaded_from = Some(global);
            }
        }

        // 3. 加载项目级配置
        let local = PathBuf::from(".raven").join("config.toml");
        if let Err(e) = system.load_file(&local) {
            debug!("项目配置不存在或无效: {}", e);
        } else {
            system.loaded_from = Some(local);
        }

        // 4. 环境变量覆盖
        system.apply_env();

        // 验证配置
        system.validate()?;

        info!("配置加载完成，模型: {}", system.config.model);
        Ok(system)
    }

    /// 获取当前配置
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// 获取可变配置引用
    pub fn config_mut(&mut self) -> &mut Config {
        &mut self.config
    }

    /// 获取配置文件路径
    pub fn loaded_from(&self) -> Option<&PathBuf> {
        self.loaded_from.as_ref()
    }

    /// 保存配置到指定路径
    pub fn save_to(&self, path: &std::path::Path) -> Result<(), String> {
        let content = toml::to_string_pretty(&self.config)
            .map_err(|e| format!("序列化失败: {}", e))?;
        std::fs::write(path, content)
            .map_err(|e| format!("写入失败: {}", e))?;
        Ok(())
    }

    /// 保存配置到默认路径 (~/.raven/config.toml)
    pub fn save(&self) -> Result<(), String> {
        let home = dirs::home_dir()
            .ok_or("无法获取家目录")?;
        let dir = home.join(".raven");
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("创建目录失败: {}", e))?;
        self.save_to(&dir.join("config.toml"))
    }

    /// 从文件加载配置
    fn load_file(&mut self, path: &PathBuf) -> Result<(), String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("读取失败: {}", e))?;

        let file_cfg: Config = toml::from_str(&content)
            .map_err(|e| format!("解析失败: {}", e))?;

        self.merge(file_cfg);
        debug!("已加载配置: {}", path.display());
        Ok(())
    }

    /// 合并配置（非空值覆盖）
    fn merge(&mut self, other: Config) {
        if !other.model.is_empty() && other.model != "gpt-4o" {
            self.config.model = other.model;
        }
        if other.api_key.is_some() {
            self.config.api_key = other.api_key;
        }
        if other.base_url.is_some() {
            self.config.base_url = other.base_url;
        }
        if other.log_level != "info" {
            self.config.log_level = other.log_level;
        }
        if other.token_budget > 0 {
            self.config.token_budget = other.token_budget;
        }
        if !other.providers.is_empty() {
            self.config.providers.extend(other.providers);
        }
        // 深度合并
        self.merge_permission(other.permission);
        self.merge_context(other.context);
        self.merge_tools(other.tools);
        self.merge_server(other.server);
    }

    fn merge_permission(&mut self, other: raven_types::PermissionConfig) {
        if !other.mode.is_empty() {
            self.config.permission.mode = other.mode;
        }
        if !other.allowed_tools.is_empty() {
            self.config.permission.allowed_tools = other.allowed_tools;
        }
        if !other.denied_tools.is_empty() {
            self.config.permission.denied_tools = other.denied_tools;
        }
    }

    fn merge_context(&mut self, other: raven_types::ContextConfig) {
        if other.max_tokens > 0 {
            self.config.context.max_tokens = other.max_tokens;
        }
        if other.compact_threshold > 0 {
            self.config.context.compact_threshold = other.compact_threshold;
        }
        if other.keep_rounds > 0 {
            self.config.context.keep_rounds = other.keep_rounds;
        }
    }

    fn merge_tools(&mut self, other: raven_types::ToolsConfig) {
        if !other.enabled.is_empty() {
            self.config.tools.enabled = other.enabled;
        }
        if !other.shell.allowed.is_empty() {
            self.config.tools.shell.allowed = other.shell.allowed;
        }
        if other.shell.timeout > 0 {
            self.config.tools.shell.timeout = other.shell.timeout;
        }
    }

    fn merge_server(&mut self, other: raven_types::ServerConfig) {
        if !other.host.is_empty() && other.host != "0.0.0.0" {
            self.config.server.host = other.host;
        }
        if other.port > 0 && other.port != 8080 {
            self.config.server.port = other.port;
        }
    }

    /// 从环境变量加载
    fn apply_env(&mut self) {
        if let Ok(key) = std::env::var("RAVEN_API_KEY") {
            if !key.is_empty() {
                self.config.api_key = Some(key);
                debug!("从 RAVEN_API_KEY 加载 API Key");
            }
        }
        if let Ok(url) = std::env::var("RAVEN_BASE_URL") {
            if !url.is_empty() {
                self.config.base_url = Some(url);
            }
        }
        if let Ok(model) = std::env::var("RAVEN_MODEL") {
            if !model.is_empty() {
                self.config.model = model;
            }
        }
        if let Ok(level) = std::env::var("RAVEN_LOG_LEVEL") {
            if !level.is_empty() {
                self.config.log_level = level;
            }
        }
    }

    /// 验证配置
    /// 任何错误立即返回，不静默失败
    pub fn validate(&self) -> Result<(), raven_types::AgentError> {
        let cfg = &self.config;

        // 验证权限模式
        let valid_modes = ["ask", "auto", "yes", "readonly"];
        if !valid_modes.contains(&cfg.permission.mode.as_str()) {
            return Err(raven_types::AgentError::config(
                format!("无效的权限模式 '{}'", cfg.permission.mode),
                "可选: ask / auto / yes / readonly",
            ));
        }

        // 验证日志级别
        let valid_levels = ["debug", "info", "warn", "error"];
        if !valid_levels.contains(&cfg.log_level.as_str()) {
            return Err(raven_types::AgentError::config(
                format!("无效的日志级别 '{}'", cfg.log_level),
                "可选: debug / info / warn / error",
            ));
        }

        // 验证上下文配置
        if cfg.context.max_tokens < 4096 && cfg.context.max_tokens != 0 {
            return Err(raven_types::AgentError::config(
                "max_tokens 太小",
                "建议至少 4096",
            ));
        }

        // 验证提供商配置
        for (i, p) in cfg.providers.iter().enumerate() {
            if p.name.is_empty() {
                return Err(raven_types::AgentError::config(
                    format!("providers[{}] 缺少 name 字段", i),
                    "为每个提供商指定一个唯一名称",
                ));
            }
            if p.base_url.is_empty() {
                return Err(raven_types::AgentError::config(
                    format!("providers[{}] 缺少 base_url", i),
                    "指定 API 的基础 URL",
                ));
            }
        }

        // 验证 git_first 配置
        if cfg.git_first.commit_prefix.is_empty() {
            return Err(raven_types::AgentError::config(
                "git_first.commit_prefix 不能为空",
                "指定一个提交前缀，如 'agent'",
            ));
        }

        Ok(())
    }
}

/// 初始化配置文件模板
pub fn init_config(dir: impl AsRef<std::path::Path>) -> Result<(), String> {
    let dir = dir.as_ref();
    let path = dir.join("config.toml");

    if path.exists() {
        return Err(format!("配置文件已存在: {}", path.display()));
    }

    std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {}", e))?;

    let template = r#"# Raven 配置文件
# 所有字段都是可选的，省略的字段使用默认值
# 运行 'raven doctor' 检查配置

# 默认模型ID (格式: "模型名" 或 "提供商/模型名")
model = "gpt-4o"

# API密钥 (也可通过 RAVEN_API_KEY 环境变量设置)
# api_key = "sk-..."

# API基础URL (使用第三方 OpenAI 兼容端点时设置)
# base_url = "https://api.openai.com/v1"

[permission]
mode = "ask"  # ask / auto / yes / readonly
allowed_tools = ["file_read", "file_write", "file_edit", "view", "search", "list_dir", "git", "web_search", "fetch_url"]

[context]
max_tokens = 128000
compact_threshold = 100000
keep_rounds = 6

# Token预算 (0 = 无限制)
token_budget = 0

[server]
host = "0.0.0.0"
port = 8080

# Git-first 设计：每次编辑后自动 git commit
[git_first]
enabled = true       # 启用 Git-first
auto_commit = true   # 自动提交（false=只add，手动commit）
commit_prefix = "raven"  # 提交信息前缀

# 额外提供商 (可选)
# [[providers]]
# name = "deepseek"
# base_url = "https://api.deepseek.com/v1"
# api_key = "sk-..."
# models = ["deepseek-chat", "deepseek-reasoner"]
"#;

    std::fs::write(&path, template).map_err(|e| format!("写入失败: {}", e))?;
    Ok(())
}
