//! 系统提示词构建：角色基底 + 运行环境上下文 + 可用工具清单，以及提示词模板切换。

use crate::Agent;
use raven_types::ToolSchema;

impl Agent {
    /// 应用环境感知的系统提示词。
    ///
    /// 以 `template`（模板名，None 用默认模板）为角色设定基底，
    /// 自动拼接当前运行环境（OS / Shell / 工作目录）和可用工具清单，
    /// 让模型据此选择平台正确的命令（如 Windows 用 `dir` 而非 `ls`）。
    /// readonly 模式不列出工具（纯对话，模型拿不到工具）。
    pub async fn apply_system_prompt(&self, template: Option<&str>) {
        let base = template
            .and_then(config_system::prompts::find_prompt)
            .map(|t| t.prompt)
            .unwrap_or_else(config_system::prompts::default_prompt);
        let schemas = if self.permission.is_readonly() {
            Vec::new()
        } else {
            self.collect_tool_schemas().await
        };
        let full = build_system_prompt(&base, &schemas);
        self.context.set_system_prompt(full).await;
    }

    /// 使用模板设置系统提示词（同样附带环境与工具上下文）
    pub async fn set_prompt_template(&self, name: &str) -> Result<String, String> {
        match config_system::prompts::find_prompt(name) {
            Some(template) => {
                let schemas = if self.permission.is_readonly() {
                    Vec::new()
                } else {
                    self.collect_tool_schemas().await
                };
                let full = build_system_prompt(&template.prompt, &schemas);
                self.context.set_system_prompt(full).await;
                Ok(format!(
                    "已切换提示词模板: {}\n{}",
                    template.name, template.description
                ))
            }
            None => {
                let available: Vec<String> = config_system::prompts::list_prompts()
                    .into_iter()
                    .map(|p| p.name)
                    .collect();
                Err(format!(
                    "未知模板 '{}'. 可用: {}",
                    name,
                    available.join(", ")
                ))
            }
        }
    }

    /// 列出提示词模板（内置 + 用户自定义）
    pub fn list_prompt_templates() -> Vec<config_system::prompts::PromptTemplate> {
        config_system::prompts::list_prompts()
    }
}

/// 构建完整系统提示词：角色基底 + 运行环境上下文 + 可用工具清单。
///
/// 解决的问题：模型默认不知道自己跑在什么系统上，会在 Windows 下
/// 调用 `pwd`/`ls` 这类 Unix 命令导致失败。这里把平台、Shell、工作目录、
/// 以及每个工具的原始 name/description/参数拼进去，让模型据此决策。
pub(crate) fn build_system_prompt(base: &str, schemas: &[ToolSchema]) -> String {
    use config_system::platform;

    let p = platform::current();
    let cwd = std::env::current_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|_| "(未知)".to_string());

    // 平台特定的命令约束，避免跨平台命令误用。给出"该用什么/不该用什么"的对照。
    let shell_hint = match p {
        platform::Platform::Windows => {
            "本机是 Windows，shell 工具走 cmd.exe，只能用 Windows 命令，不要用 Unix 命令：\n\
             - 列目录: 用 `dir`，不要用 `ls`\n\
             - 当前目录: 用 `cd`，不要用 `pwd`\n\
             - 删除文件: 用 `del`，不要用 `rm`\n\
             - 查看文件: 用 `type`，不要用 `cat`\n\
             - 路径分隔符是 `\\`（如 `crates\\cli\\src`）\n\
             更推荐：读文件用 view，列目录用 list_dir，搜索用 search，改文件用 file_edit——\
             这些内置工具跨平台一致，应优先于 shell 命令。"
        }
        _ => {
            "本机是类 Unix 系统，shell 工具走 bash，可用 ls/cat/pwd/grep 等常见命令，\
             路径分隔符是 `/`。更推荐：读文件用 view，列目录用 list_dir，搜索用 search，\
             改文件用 file_edit——这些内置工具跨平台一致，应优先于 shell 命令。"
        }
    };

    let mut out = String::with_capacity(base.len() + 768 + schemas.len() * 128);
    // 第一段：基础提示词（先展开用户模板里的 {{os}}/{{shell}}/{{cwd}} 等环境占位符）
    out.push_str(&config_system::prompts::expand_placeholders(base));

    // 第二段：运行环境 + 命令约束
    out.push_str("\n\n# 运行环境\n");
    out.push_str("你正运行在以下环境中，所有命令和路径都必须与之匹配：\n");
    out.push_str(&format!(
        "- 操作系统: {} ({})\n",
        p.name(),
        platform::arch()
    ));
    out.push_str(&format!("- 默认 Shell: {}\n", p.default_shell()));
    out.push_str(&format!("- 路径分隔符: {}\n", p.path_sep()));
    out.push_str(&format!("- 工作目录: {}\n", cwd));
    out.push('\n');
    out.push_str(shell_hint);

    // 第三段：可用工具
    if !schemas.is_empty() {
        out.push_str("\n\n# 可用工具\n");
        out.push_str(
            "你可以调用以下工具。每个工具的完整参数 schema 已随请求下发，\
             调用时严格按 schema 提供 JSON 参数：\n\n",
        );
        for s in schemas {
            let f = &s.function;
            out.push_str(&format!("## {}\n{}\n", f.name, f.description));
            // 列出参数名，方便模型直接对齐（完整 schema 已在 tool definition 中下发）
            if let Some(props) = f.parameters.get("properties").and_then(|v| v.as_object()) {
                let names: Vec<&str> = props.keys().map(|k| k.as_str()).collect();
                if !names.is_empty() {
                    out.push_str(&format!("参数: {}\n", names.join(", ")));
                }
            }
            out.push('\n');
        }

        // task 工具的使用指引：何时派生并行子 agent
        if schemas.iter().any(|s| s.function.name == "task") {
            out.push_str(
                "## 关于 task（并行子 agent）\n\
                 当任务能拆成多个**互不依赖**的子任务时（如同时调查多个文件/模块、\
                 并行收集多处信息），在一条消息里同时发出多个 task 调用，它们会并发执行，\
                 显著快于逐个串行。每个 task 都是独立子 agent：有完整工具集、独立上下文，\
                 只把最终结论文本返回给你，看不到你的对话历史，也不能再派生 task。\
                 因此 prompt 必须自包含、明确说清要做什么和要返回什么。\
                 子任务有先后依赖、或需要你逐步决策时，不要用 task，直接自己调工具。\n\n",
            );
        }

        // ask_user 工具的使用指引
        if schemas.iter().any(|s| s.function.name == "ask_user") {
            out.push_str(
                "## 关于 ask_user（向用户提问）\n\
                 当你需要用户做决策、在多个方案间选择、或澄清模糊需求时,\
                 调用 ask_user 给出问题和 2-6 个选项,而不是自己猜测或直接假设。\
                 用户的选择会作为工具结果返回。只在真正需要用户拍板时用,\
                 别为已经明确的事情反复提问。\n\n",
            );
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use raven_types::FunctionSchema;

    #[test]
    fn build_system_prompt_includes_env_and_tools() {
        let schema = ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "list_dir".to_string(),
                description: "列出目录内容".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } }
                }),
            },
        };
        let out = build_system_prompt("你是助手", &[schema]);
        assert!(out.starts_with("你是助手"));
        assert!(out.contains("# 运行环境"));
        assert!(out.contains("操作系统:"));
        assert!(out.contains("# 可用工具"));
        assert!(out.contains("list_dir"));
        assert!(out.contains("参数: path"));
    }

    #[test]
    fn build_system_prompt_omits_tools_when_empty() {
        let out = build_system_prompt("base", &[]);
        assert!(out.contains("# 运行环境"));
        assert!(!out.contains("# 可用工具"));
    }
}
