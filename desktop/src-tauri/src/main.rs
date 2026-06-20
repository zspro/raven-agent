#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use raven_core::Agent;
use std::sync::Arc;
use tokio::sync::Mutex;

struct AppState {
    agent: Arc<Mutex<Agent>>,
}

#[derive(serde::Serialize)]
struct ChatResponsePayload {
    content: String,
}

#[derive(serde::Serialize)]
struct HealthStatus {
    status: String,
    version: String,
}

// Tauri 命令
#[tauri::command]
async fn chat(
    state: tauri::State<'_, AppState>,
    message: String,
) -> Result<ChatResponsePayload, String> {
    let agent = state.agent.lock().await;
    let content = agent.run(&message).await.map_err(|e| e.to_string())?;
    Ok(ChatResponsePayload { content })
}

#[tauri::command]
async fn clear_session(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let agent = state.agent.lock().await;
    agent.clear().await;
    Ok(())
}

#[tauri::command]
async fn check_health() -> Result<HealthStatus, String> {
    Ok(HealthStatus {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

#[tokio::main]
async fn main() {
    // 初始化 Agent
    let config_system = config_system::ConfigSystem::load()
        .expect("Failed to load config. Run 'raven init' first or check ~/.raven/config.toml");

    let agent = Agent::from_config(&config_system)
        .await
        .expect("Failed to initialize agent");

    // 注入终端确认器（桌面端目前无交互确认，非 TTY 场景默认拒绝）
    // agent.set_confirmer(Arc::new(raven_core::StdinConfirmer));

    let state = AppState {
        agent: Arc::new(Mutex::new(agent)),
    };

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            chat,
            clear_session,
            check_health,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
