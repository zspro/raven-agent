//! raven-types 单元测试
//!
//! 核心类型的序列化、反序列化和验证测试。

#[cfg(test)]
mod tests {
    use crate::*;

    // ===================================================================
    // Message 测试
    // ===================================================================

    #[test]
    fn test_message_user() {
        let msg = Message::user("Hello");
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.content, "Hello");
        assert!(msg.tool_calls.is_none());
    }

    #[test]
    fn test_message_assistant() {
        let msg = Message::assistant("Hi there");
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.content, "Hi there");
    }

    #[test]
    fn test_message_system() {
        let msg = Message::system("You are a helpful assistant");
        assert_eq!(msg.role, Role::System);
        assert_eq!(msg.content, "You are a helpful assistant");
    }

    #[test]
    fn test_message_tool_result() {
        let msg = Message::tool_result("call_1", "file_read", "content");
        assert_eq!(msg.role, Role::Tool);
        assert_eq!(msg.content, "content");
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(msg.name.as_deref(), Some("file_read"));
    }

    #[test]
    fn test_message_serialization() {
        let msg = Message::user("Test");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"content\":\"Test\""));
    }

    #[test]
    fn test_message_deserialization() {
        let json = r#"{"role":"assistant","content":"Hello"}"#;
        let msg: Message = serde_json::from_str(json).unwrap();
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.content, "Hello");
    }

    // ===================================================================
    // ToolCall / ToolResult 测试
    // ===================================================================

    #[test]
    fn test_tool_call() {
        let call = ToolCall {
            index: 0,
            id: "call_123".to_string(),
            call_type: "function".to_string(),
            function: ToolCallFunction {
                name: "file_read".to_string(),
                arguments: r#"{"path":"test.txt"}"#.to_string(),
            },
        };
        assert_eq!(call.function.name, "file_read");
        assert_eq!(call.id, "call_123");
        assert_eq!(call.call_type, "function");
    }

    #[test]
    fn test_tool_call_arguments_from_string() {
        // OpenAI 标准格式: arguments 为 JSON 字符串
        let json = r#"{"name":"shell","arguments":"{\"command\":\"ls\"}"}"#;
        let f: ToolCallFunction = serde_json::from_str(json).unwrap();
        assert_eq!(f.name, "shell");
        assert_eq!(f.arguments, r#"{"command":"ls"}"#);
    }

    #[test]
    fn test_tool_call_arguments_from_object() {
        // NewAPI/OneAPI 代理格式: arguments 为 JSON 对象
        let json = r#"{"name":"shell","arguments":{"command":"ls"}}"#;
        let f: ToolCallFunction = serde_json::from_str(json).unwrap();
        assert_eq!(f.name, "shell");
        assert_eq!(f.arguments, r#"{"command":"ls"}"#);
    }

    #[test]
    fn test_tool_result_success() {
        let result = ToolResult {
            tool_call_id: "call_123".to_string(),
            name: "file_read".to_string(),
            content: "file content".to_string(),
            is_error: false,
        };
        assert!(!result.is_error);
    }

    #[test]
    fn test_tool_result_error() {
        let result = ToolResult {
            tool_call_id: "call_123".to_string(),
            name: "shell".to_string(),
            content: "Permission denied".to_string(),
            is_error: true,
        };
        assert!(result.is_error);
    }

    // ===================================================================
    // Config 测试
    // ===================================================================

    #[test]
    fn test_config_default() {
        let cfg = Config::default();
        assert_eq!(cfg.model, "gpt-4o");
        assert_eq!(cfg.permission.mode, "ask");
        assert_eq!(cfg.context.max_tokens, 128_000);
        assert_eq!(cfg.context.keep_rounds, 6);
        assert_eq!(cfg.token_budget, 0);
        assert_eq!(cfg.log_level, "info");
    }

    #[test]
    fn test_config_serialization() {
        let cfg = Config::default();
        let json = serde_json::to_string_pretty(&cfg).unwrap();
        assert!(json.contains("\"model\": \"gpt-4o\""));
        assert!(json.contains("\"permission\""));
    }

    // ===================================================================
    // StreamEvent 测试
    // ===================================================================

    #[test]
    fn test_stream_event_text() {
        let event = StreamEvent::text("Hello");
        assert_eq!(event.event_type, "text");
        assert_eq!(event.content, Some("Hello".to_string()));
    }

    #[test]
    fn test_stream_event_done() {
        let event = StreamEvent::done();
        assert_eq!(event.event_type, "done");
        assert!(event.content.is_none());
    }

    #[test]
    fn test_stream_event_error() {
        let event = StreamEvent::error("Something went wrong");
        assert_eq!(event.event_type, "error");
        assert_eq!(event.content, Some("Something went wrong".to_string()));
    }

    #[test]
    fn test_stream_event_serialization() {
        let event = StreamEvent::text("test");
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"content\":\"test\""));
    }

    // ===================================================================
    // AgentError 测试
    // ===================================================================

    #[test]
    fn test_error_config() {
        let err = AgentError::config("invalid key", "check your config");
        let msg = format!("{}", err);
        assert!(msg.contains("invalid key"));
        assert!(msg.contains("check your config"));
    }

    #[test]
    fn test_error_network() {
        let err = AgentError::Network {
            message: "timeout".to_string(),
            fix: "check connection".to_string(),
        };
        let msg = format!("{}", err);
        assert!(msg.contains("timeout"));
    }

    #[test]
    fn test_error_permission() {
        let err = AgentError::permission("shell");
        let msg = format!("{}", err);
        assert!(msg.contains("shell"));
    }

    #[test]
    fn test_error_budget() {
        let err = AgentError::budget(100, 100);
        let msg = format!("{}", err);
        assert!(msg.contains("100"));
    }

    #[test]
    fn test_error_cancelled() {
        let err = AgentError::Cancelled;
        let msg = format!("{}", err);
        assert!(msg.contains("已取消"));
    }

    // ===================================================================
    // GitFirstConfig 测试
    // ===================================================================

    #[test]
    fn test_git_first_config_default() {
        let cfg = GitFirstConfig::default();
        assert!(cfg.enabled);
        assert!(cfg.auto_commit);
        assert_eq!(cfg.commit_prefix, "raven");
    }

    #[test]
    fn test_git_first_config_toml() {
        let toml = r#"
enabled = false
auto_commit = false
commit_prefix = "ai"
"#;
        let cfg: GitFirstConfig = toml::from_str(toml).unwrap();
        assert!(!cfg.enabled);
        assert!(!cfg.auto_commit);
        assert_eq!(cfg.commit_prefix, "ai");
    }

    // ===================================================================
    // TokenUsage 测试
    // ===================================================================

    #[test]
    fn test_token_usage() {
        let usage = TokenUsage {
            input: 100,
            output: 50,
            total: 150,
            cached: None,
        };
        assert_eq!(usage.input, 100);
        assert_eq!(usage.output, 50);
        assert_eq!(usage.total, 150);
        assert!(usage.cached.is_none());
    }

    #[test]
    fn test_token_usage_serialization() {
        // 字段通过 serde rename 输出为 input_tokens/output_tokens/total_tokens
        let usage = TokenUsage {
            input: 10,
            output: 5,
            total: 15,
            cached: None,
        };
        let json = serde_json::to_string(&usage).unwrap();
        assert!(json.contains("\"input_tokens\":10"));
        assert!(json.contains("\"output_tokens\":5"));
        assert!(json.contains("\"total_tokens\":15"));
    }

    // ===================================================================
    // ProviderConfig 测试
    // ===================================================================

    #[test]
    fn test_provider_config_toml() {
        let toml = r#"
name = "deepseek"
base_url = "https://api.deepseek.com/v1"
api_key = "sk-xxx"
models = ["deepseek-chat", "deepseek-coder"]
"#;
        let cfg: ProviderConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.name, "deepseek");
        assert_eq!(cfg.models.len(), 2);
    }

    // ===================================================================
    // ChatResponse 测试
    // ===================================================================

    #[test]
    fn test_chat_response() {
        let resp = ChatResponse {
            content: "Hello".to_string(),
            tool_calls: vec![],
            model: "gpt-4o".to_string(),
            usage: TokenUsage {
                input: 10,
                output: 5,
                total: 15,
                cached: None,
            },
            finish_reason: "stop".to_string(),
        };
        assert_eq!(resp.content, "Hello");
        assert_eq!(resp.usage.total, 15);
        assert_eq!(resp.finish_reason, "stop");
    }
}
