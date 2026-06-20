//! agent-types 单元测试
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
    fn test_message_serialization() {
        let msg = Message::user("Test");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"content\":\"Test\""));
    }

    #[test]
    fn test_message_deserialization() {
        let json = r#"{"role":"assistant","content":"Hello","tool_calls":null}"#;
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
            id: "call_123".to_string(),
            function: FunctionCall {
                name: "file_read".to_string(),
                arguments: serde_json::json!({"path": "test.txt"}),
            },
        };
        assert_eq!(call.function.name, "file_read");
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
        assert_eq!(cfg.context.max_tokens, 128000);
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
        let err = AgentError::Permission {
            message: "denied".to_string(),
            fix: "allow tool".to_string(),
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn test_error_cancelled() {
        let err = AgentError::Cancelled;
        assert!(!err.is_retryable());
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
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
        };
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.total_tokens, 150);
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
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            },
        };
        assert_eq!(resp.content, "Hello");
        assert_eq!(resp.usage.total_tokens, 15);
    }
}
