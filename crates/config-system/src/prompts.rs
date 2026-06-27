//! 系统提示词模板
//!
//! 预设的系统提示词（角色设定），用户可以快速切换场景。
//! 在交互模式中使用 /prompt 命令切换。
//!
//! ## 自定义模板（文件加载）
//!
//! 在 `~/.raven/prompts/` 目录下放置 `*.md` 文件即可新增或覆盖模板：
//! - 文件名（去掉 `.md`）即模板名，与内置同名则覆盖内置。
//! - 文件首个 Markdown 一级标题行 `# 描述` 作为模板描述（可选）。
//! - 其余正文为提示词内容。
//!
//! ## 占位符
//!
//! 提示词正文中可使用以下占位符，应用时按当前运行环境替换：
//! `{{os}}` `{{arch}}` `{{shell}}` `{{cwd}}` `{{path_sep}}`

use std::path::PathBuf;

/// 提示词模板（owned，可来自内置常量或用户文件）
#[derive(Debug, Clone)]
pub struct PromptTemplate {
    pub name: String,
    pub description: String,
    pub prompt: String,
}

/// 内置提示词模板（编译期常量，零配置可用）
struct BuiltinPrompt {
    name: &'static str,
    description: &'static str,
    prompt: &'static str,
}

const BUILTIN_PROMPTS: &[BuiltinPrompt] = &[
    BuiltinPrompt {
        name: "default",
        description: "通用助手",
        prompt: "你是 Raven，一个跨平台的 AI 助手，可以查看/编辑文件、执行命令、检索网络。当前运行在 {{os}}（{{arch}}），默认 shell 为 {{shell}}，工作目录 {{cwd}}。\n\n工具使用:\n1. 读文件/看目录优先用 view，搜索代码用 search，列目录用 list_dir——这些跨平台一致，优先于 shell 命令。\n2. 改已有文件用 file_edit（精确替换），新建或整体重写才用 file_write；编辑前先 view 确认内容。\n3. 用 shell 时按本机系统选命令（{{os}} 下 shell 为 {{shell}}）。\n4. 需要最新信息时用 web_search，再用 fetch_url 读正文。\n\n回答规则:\n1. 简洁准确，先给结论再展开。\n2. 不确定时诚实说明，不臆造。\n3. 技术问题给出可运行的示例，带文件路径。\n4. 使用中文回答。",
    },
    BuiltinPrompt {
        name: "coder",
        description: "编程专家（Claude Code 风格）",
        prompt: "你是一个专业的编程助手。你可以查看和编辑代码文件、执行命令、使用 Git。\n\n规则:\n1. 优先使用 view 和 file_edit 工具操作代码\n2. 修改前先用 view 查看文件内容\n3. 使用 file_edit 时确保 old_string 精确匹配\n4. 提供代码时带行号和文件路径\n5. 解释修改的原因\n6. 一次只做一个逻辑修改",
    },
    BuiltinPrompt {
        name: "reviewer",
        description: "代码审查员",
        prompt: "你是一个严格的代码审查员。你会审查代码质量、安全性、性能。\n\n审查维度:\n1. 正确性 - 是否有 bug 或逻辑错误\n2. 安全性 - 是否有注入、越界、竞态条件\n3. 性能 - 是否有不必要的分配、复杂度问题\n4. 可维护性 - 命名、注释、结构\n5. 测试 - 边界条件是否覆盖\n\n输出格式:\n- [严重] / [建议] 问题描述\n- 具体位置（文件:行号）\n- 修复建议（带代码示例）",
    },
    BuiltinPrompt {
        name: "architect",
        description: "架构师",
        prompt: "你是一个资深软件架构师。你帮助设计系统架构、评估技术方案。\n\n能力:\n1. 分析需求并提出架构方案\n2. 评估技术选型的利弊\n3. 识别潜在的风险和瓶颈\n4. 给出可扩展的设计建议\n5. 生成系统架构图（Mermaid 格式）\n\n回答风格:\n- 先给出结论，再展开说明\n- 对比不同方案的优劣\n- 考虑实际工程约束（团队规模、维护成本）",
    },
    BuiltinPrompt {
        name: "debugger",
        description: "调试专家",
        prompt: "你是一个调试专家。你帮助定位和修复 bug。\n\n调试流程:\n1. 先复现问题\n2. 使用工具收集信息（日志、变量值、调用栈）\n3. 提出假设并验证\n4. 定位根因（不只是症状）\n5. 给出修复方案\n\n工具使用:\n- view: 查看可疑代码\n- shell: 运行测试、查看日志\n- search: 搜索相关代码\n- git: 查看变更历史",
    },
    BuiltinPrompt {
        name: "rust_expert",
        description: "Rust 专家",
        prompt: "你是一个 Rust 语言专家。你精通所有权、生命周期、异步编程。\n\n专长:\n1. 解决 borrow checker 错误\n2. 优化性能（零成本抽象）\n3. 异步代码设计\n4. unsafe 代码审查\n5. 宏编程\n\n回答规则:\n1. 给出编译通过的代码\n2. 解释为什么这样写\n3. 标注潜在的性能影响\n4. 推荐相关的 crate",
    },
    BuiltinPrompt {
        name: "writer",
        description: "技术写作",
        prompt: "你是一个技术写作专家。你帮助撰写文档、README、API 文档。\n\n写作原则:\n1. 简洁清晰，避免冗余\n2. 代码示例完整可运行\n3. 先给结论，再给细节\n4. 使用 Markdown 格式\n5. 适当使用图表（Mermaid）\n\n输出格式:\n- 标题层级清晰\n- 关键概念加粗\n- 示例代码带注释",
    },
];

/// 用户自定义模板目录 `~/.raven/prompts/`
pub fn prompts_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".raven").join("prompts"))
}

/// 从单个 `.md` 文件解析模板。
///
/// 约定：若首行是 `# 一级标题` 则取作描述，其余作为正文；
/// 否则整个文件作为正文，描述回退为"自定义模板"。
fn parse_md_template(name: &str, content: &str) -> PromptTemplate {
    let trimmed = content.trim_start();
    if let Some(rest) = trimmed.strip_prefix("# ") {
        if let Some((desc, body)) = rest.split_once('\n') {
            return PromptTemplate {
                name: name.to_string(),
                description: desc.trim().to_string(),
                prompt: body.trim().to_string(),
            };
        }
    }
    PromptTemplate {
        name: name.to_string(),
        description: "自定义模板".to_string(),
        prompt: content.trim().to_string(),
    }
}

/// 加载 `~/.raven/prompts/*.md` 中的用户模板（目录不存在则返回空）。
fn load_user_prompts() -> Vec<PromptTemplate> {
    let Some(dir) = prompts_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Ok(content) = std::fs::read_to_string(&path) {
            out.push(parse_md_template(stem, &content));
        }
    }
    out
}

/// 合并内置与用户模板：用户同名模板覆盖内置；用户独有模板追加在后。
/// 返回顺序：内置（可能被覆盖）在前，用户新增在后。
fn merged_prompts() -> Vec<PromptTemplate> {
    let user = load_user_prompts();
    let mut merged: Vec<PromptTemplate> = BUILTIN_PROMPTS
        .iter()
        .map(|b| {
            // 同名用户模板覆盖
            if let Some(u) = user.iter().find(|u| u.name == b.name) {
                u.clone()
            } else {
                PromptTemplate {
                    name: b.name.to_string(),
                    description: b.description.to_string(),
                    prompt: b.prompt.to_string(),
                }
            }
        })
        .collect();
    // 追加用户独有模板
    for u in user {
        if !BUILTIN_PROMPTS.iter().any(|b| b.name == u.name) {
            merged.push(u);
        }
    }
    merged
}

/// 通过名称查找提示词模板（用户模板优先覆盖内置）
pub fn find_prompt(name: &str) -> Option<PromptTemplate> {
    merged_prompts().into_iter().find(|p| p.name == name)
}

/// 列出所有提示词模板（内置 + 用户自定义，已合并去重）
pub fn list_prompts() -> Vec<PromptTemplate> {
    merged_prompts()
}

/// 格式化提示词列表（用于 UI 显示）
pub fn format_prompt_list() -> String {
    let mut lines = vec!["可用提示词模板:".to_string()];
    for (i, p) in merged_prompts().iter().enumerate() {
        lines.push(format!("  {}. {} - {}", i + 1, p.name, p.description));
    }
    lines.join("\n")
}

/// 获取默认提示词内容
pub fn default_prompt() -> String {
    find_prompt("default")
        .map(|p| p.prompt)
        .unwrap_or_else(|| BUILTIN_PROMPTS[0].prompt.to_string())
}

/// 展开提示词中的环境占位符。
///
/// 支持：`{{os}}` `{{arch}}` `{{shell}}` `{{cwd}}` `{{path_sep}}`。
/// 使用双花括号以避免误伤正文中的单花括号（代码、JSON 等）。
/// 未识别的 `{{...}}` 原样保留。
pub fn expand_placeholders(prompt: &str) -> String {
    let p = crate::platform::current();
    let cwd = std::env::current_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|_| "(未知)".to_string());
    prompt
        .replace("{{os}}", p.name())
        .replace("{{arch}}", crate::platform::arch())
        .replace("{{shell}}", p.default_shell())
        .replace("{{cwd}}", &cwd)
        .replace("{{path_sep}}", &p.path_sep().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_default_found() {
        let p = find_prompt("default").expect("内置 default 应存在");
        assert_eq!(p.name, "default");
        assert!(!p.prompt.is_empty());
    }

    #[test]
    fn list_contains_all_builtins() {
        let names: Vec<String> = list_prompts().into_iter().map(|p| p.name).collect();
        for b in BUILTIN_PROMPTS {
            assert!(names.contains(&b.name.to_string()), "缺少内置: {}", b.name);
        }
    }

    #[test]
    fn parse_md_with_title_description() {
        let t = parse_md_template("security", "# 安全审计专家\n你是一个安全审计专家。");
        assert_eq!(t.name, "security");
        assert_eq!(t.description, "安全审计专家");
        assert_eq!(t.prompt, "你是一个安全审计专家。");
    }

    #[test]
    fn parse_md_without_title() {
        let t = parse_md_template("plain", "只有正文，没有标题。");
        assert_eq!(t.description, "自定义模板");
        assert_eq!(t.prompt, "只有正文，没有标题。");
    }

    #[test]
    fn expand_placeholders_replaces_known() {
        let out = expand_placeholders("系统={{os}} 架构={{arch}} shell={{shell}}");
        assert!(!out.contains("{{os}}"));
        assert!(!out.contains("{{arch}}"));
        assert!(!out.contains("{{shell}}"));
    }

    #[test]
    fn expand_placeholders_keeps_unknown() {
        let out = expand_placeholders("保留 {{unknown}} 占位");
        assert!(out.contains("{{unknown}}"));
    }

    #[test]
    fn expand_placeholders_ignores_single_brace() {
        // 正文里的单花括号（如 JSON）不应被当作占位符
        let out = expand_placeholders("代码 {os} 不替换");
        assert!(out.contains("{os}"));
    }

    #[test]
    fn default_prompt_non_empty() {
        assert!(!default_prompt().is_empty());
    }
}
