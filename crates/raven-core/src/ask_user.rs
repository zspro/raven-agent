//! `ask_user` 工具：模型主动向用户提问（仿 Claude Code 的 AskUserQuestion）。
//!
//! 模型在需要用户决策、澄清或在多个方案间选择时调用本工具，给出问题与若干选项，
//! UI 层（Confirmer）弹出方向键菜单让用户选择，选中结果回灌给模型继续推理。
//!
//! 与 `task` 一样在 Agent 层特殊路由：工具本身无状态、拿不到 confirmer，
//! 故不能做成普通 `Tool`，而由 run 循环识别 `ask_user` 后调用本模块。

use crate::confirm::AskRequest;
use crate::{Agent, Confirmer};
use raven_types::*;
use std::sync::Arc;

impl Agent {
    /// `ask_user` 工具的 schema。
    pub(crate) fn ask_user_tool_schema() -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "ask_user".to_string(),
                description: "向用户提出一个问题并给出若干选项，等待用户选择后再继续。\
                    当你需要用户做决策、在多个方案间选择、或澄清模糊需求时使用——\
                    不要猜测用户意图，直接问。用户的选择会作为工具结果返回给你。"
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "question": {
                            "type": "string",
                            "description": "要问用户的问题，清晰具体"
                        },
                        "options": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "供用户选择的候选项（2-6 个），每项简短明确"
                        },
                        "multi_select": {
                            "type": "boolean",
                            "description": "是否允许多选（默认 false 单选）"
                        },
                        "allow_custom": {
                            "type": "boolean",
                            "description": "是否额外允许用户手动输入自定义答案（默认 true）"
                        }
                    },
                    "required": ["question", "options"]
                }),
            },
        }
    }

    /// 执行一次 `ask_user`：解析参数、调 confirmer 提问、把用户选择包装成 ToolResult。
    pub(crate) async fn run_ask_user(
        confirmer: Option<&Arc<dyn Confirmer>>,
        call: &ToolCall,
    ) -> ToolResult {
        let args: serde_json::Value =
            serde_json::from_str(&call.function.arguments).unwrap_or_default();
        let question = args
            .get("question")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let options: Vec<String> = args
            .get("options")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let multi_select = args
            .get("multi_select")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // 默认允许自定义输入，贴合 Claude Code 的「其他」语义
        let allow_custom = args
            .get("allow_custom")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let err = |msg: &str| ToolResult {
            tool_call_id: call.id.clone(),
            name: "ask_user".to_string(),
            content: msg.to_string(),
            is_error: true,
        };

        if question.is_empty() || options.is_empty() {
            return err("ask_user 需要 question 和至少一个 option");
        }
        let Some(confirmer) = confirmer else {
            return err(
                "当前环境无法交互提问（非交互终端）。请基于已有信息自行决策，不要再调用 ask_user。",
            );
        };

        let req = AskRequest {
            question,
            options,
            multi_select,
            allow_custom,
        };
        match confirmer.ask(&req).await {
            Some(picked) => ToolResult {
                tool_call_id: call.id.clone(),
                name: "ask_user".to_string(),
                content: format!("用户选择: {}", picked.join("; ")),
                is_error: false,
            },
            None => err("用户取消了选择（未作答）。请基于已有信息继续，不要重复提问。"),
        }
    }
}
