//! Git 操作工具：GitTool

use crate::Tool;
use async_trait::async_trait;
use raven_types::{FunctionSchema, ToolSchema};
use serde_json::json;

use super::shell::decode_console_bytes;

pub struct GitTool;

#[async_trait]
impl Tool for GitTool {
    fn name(&self) -> &str {
        "git"
    }
    fn description(&self) -> &str {
        "执行 Git 命令"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "git".to_string(),
                description:
                    "执行 Git 操作。支持只读查询（status、log、diff、branch、show、blame、remote、config）以及写操作（add、commit、init）。\n\n何时使用：查看仓库状态/历史/差异，或在用户要求时暂存与提交。\n注意：仅在用户明确要求时才创建提交；commit 前先用 status/diff 确认改动范围。不要自行改动 git 配置。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Git 子命令，取值之一：status、log、diff、branch、show、blame、remote、config、add、commit、init。" },
                        "args": { "type": "string", "description": "传给该子命令的额外参数（可选，按空格分隔），如 commit 的 -m \"信息\"。" }
                    },
                    "required": ["command"]
                }),
            },
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let command = args["command"].as_str().ok_or("缺少 command 参数")?;
        let extra = args["args"].as_str().unwrap_or("");

        // 安全检查
        let allowed = [
            "status", "log", "diff", "branch", "show", "blame", "remote", "config", "commit",
            "add", "init",
        ];
        if !allowed.contains(&command) {
            return Err(format!("不支持的 git 命令: {}", command));
        }

        let mut cmd = tokio::process::Command::new("git");
        cmd.arg(command);
        if !extra.is_empty() {
            cmd.args(tokenize_args(extra));
        }

        let output = cmd
            .output()
            .await
            .map_err(|e| format!("git 执行失败: {}", e))?;

        let stdout = decode_console_bytes(&output.stdout);
        let stderr = decode_console_bytes(&output.stderr);

        if !output.status.success() {
            return Err(stderr);
        }

        Ok(if stderr.is_empty() {
            stdout
        } else {
            format!("{}\n[stderr]: {}", stdout, stderr)
        })
    }
}

/// 把参数字符串拆成 argv，尊重单/双引号（引号内空格不切分），引号本身被剥除。
/// 这样 `commit -m "提交信息"` 会得到 `["-m", "提交信息"]`，而不是被空格切碎。
fn tokenize_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut has_token = false;
    for c in s.chars() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
                has_token = true;
            }
            '"' if !in_single => {
                in_double = !in_double;
                has_token = true;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if has_token {
                    out.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            c => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        out.push(cur);
    }
    out
}
