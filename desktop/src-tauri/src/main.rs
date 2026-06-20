#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use raven_core::Agent;
use raven_types::*;
use std::sync::Arc;
use tauri::State;
use tokio::sync::Mutex;

struct AppState {
    agent: Arc<Mutex<Agent>>,
}

// Tauri 命令
#[tauri::command]
async fn chat(
    state: State<'_, AppState>,
    message: String,
) -> Result<ChatResponsePayload, String> {
    let agent = state.agent.lock().await;
    let response = agent.run(&message).await.map_err(|e| e.to_string())?;
    
    Ok(ChatResponsePayload {
        content: response.content,
        model: response.model,
        input_tokens: response.usage.input_tokens,
        output_tokens: response.usage.output_tokens,
    })
}

#[tauri::command]
async fn chat_stream(
    state: State<'_, AppState>,
    message: String,
) -> Result<Vec<StreamEventPayload>, String> {
    let agent = state.agent.lock().await;
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    
    // 收集所有事件
    let mut events = Vec::new();
    
    // 由于 Tauri 的 IPC 限制，这里用同步方式收集
    // 实际生产环境应该用 WebSocket 或 SSE
    match agent.run(&message).await {
        Ok(response) => {
            events.push(StreamEventPayload {
                event_type: "text".to_string(),
                content: Some(response.content),
            });
            events.push(StreamEventPayload {
                event_type: "usage".to_string(),
                content: Some(format!("{} in / {} out", 
                    response.usage.input_tokens,
                    response.usage.output_tokens)),
            });
            events.push(StreamEventPayload {
                event_type: "done".to_string(),
                content: None,
            });
        }
        Err(e) => {
            events.push(StreamEventPayload {
                event_type: "error".to_string(),
                content: Some(e.to_string()),
            });
        }
    }
    
    Ok(events)
}

#[tauri::command]
async fn get_models(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    // 从 agent 配置中获取模型信息
    let agent = state.agent.lock().await;
    let cfg = agent.config();
    Ok(vec![cfg.model])
}

#[tauri::command]
async fn clear_session(state: State<'_, AppState>) -> Result<(), String> {
    let agent = state.agent.lock().await;
    agent.clear_session().await;
    Ok(())
}

#[tauri::command]
async fn check_health() -> Result<HealthStatus, String> {
    Ok(HealthStatus {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

// 响应类型
#[derive(serde::Serialize)]
struct ChatResponsePayload {
    content: String,
    model: String,
    input_tokens: usize,
    output_tokens: usize,
}

#[derive(serde::Serialize)]
struct StreamEventPayload {
    event_type: String,
    content: Option<String>,
}

#[derive(serde::Serialize)]
struct HealthStatus {
    status: String,
    version: String,
}

#[tokio::main]
async fn main() {
    // 初始化 Agent
    let config_system = config_system::ConfigSystem::load()
        .unwrap_or_else(|_| config_system::ConfigSystem::default());
    
    let agent = Agent::from_config(&config_system)
        .await
        .expect("Failed to initialize agent");

    let state = AppState {
        agent: Arc::new(Mutex::new(agent)),
    };

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            chat,
            chat_stream,
            get_models,
            clear_session,
            check_health,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
