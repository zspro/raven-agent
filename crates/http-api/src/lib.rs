//! # http-api
//! HTTP API 服务器（axum + SSE）

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{sse::Event, Sse},
    routing::{get, post},
    Json, Router,
};
use raven_core::{Agent, DoctorResult};
use raven_types::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;
use tracing::info;

/// 创建 API 路由
pub fn create_routes(agent: Arc<Agent>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/api/v1/chat", post(handle_chat))
        .route("/api/v1/chat/stream", post(handle_chat_stream))
        .route("/api/v1/models", get(handle_list_models))
        .route("/api/v1/models/verify", post(handle_verify_models))
        .route("/api/v1/tools", get(handle_list_tools))
        .route("/api/v1/doctor", get(handle_doctor))
        .route("/api/v1/tokens", get(handle_token_usage))
        .route("/api/v1/session", get(handle_session_info))
        .route("/api/v1/session/clear", post(handle_clear_session))
        // 会话管理（Phase 4）
        .route(
            "/api/v1/sessions",
            get(handle_list_sessions).post(handle_create_session),
        )
        .route(
            "/api/v1/sessions/:id",
            get(handle_load_session).delete(handle_delete_session),
        )
        .route("/api/v1/sessions/current", get(handle_current_session))
        .route(
            "/api/v1/sessions/:id/messages",
            get(handle_session_messages),
        )
        .route("/health", get(handle_health))
        .nest_service(
            "/",
            ServeDir::new("web").append_index_html_on_directories(true),
        )
        .layer(cors)
        .with_state(agent)
}

/// 启动 HTTP 服务器
pub async fn serve(agent: Arc<Agent>, host: &str, port: u16) -> anyhow::Result<()> {
    let app = create_routes(agent);
    let addr = format!("{}:{}", host, port);

    info!("HTTP API 服务器启动: http://{}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

// =============================================================================
// 请求/响应类型
// =============================================================================

#[derive(Debug, Deserialize)]
struct ChatRequest {
    message: String,
    #[serde(default)]
    #[allow(dead_code)]
    model: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChatResponse {
    response: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ModelsResponse {
    models: Vec<ModelInfo>,
    count: usize,
}

#[derive(Debug, Serialize)]
struct DoctorResponse {
    healthy: bool,
    checks: Vec<DoctorResult>,
}

// =============================================================================
// 处理器
// =============================================================================

async fn handle_chat(
    State(agent): State<Arc<Agent>>,
    Json(req): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, StatusCode> {
    if let Some(prompt) = req.system_prompt {
        agent.set_system_prompt(prompt).await;
    }

    match agent.run(&req.message).await {
        Ok(response) => Ok(Json(ChatResponse {
            response,
            error: None,
        })),
        Err(e) => Ok(Json(ChatResponse {
            response: String::new(),
            error: Some(e.to_string()),
        })),
    }
}

async fn handle_chat_stream(
    State(agent): State<Arc<Agent>>,
    Json(req): Json<ChatRequest>,
) -> Sse<tokio_stream::wrappers::ReceiverStream<Result<Event, std::convert::Infallible>>> {
    if let Some(prompt) = req.system_prompt {
        agent.set_system_prompt(prompt).await;
    }

    let (tx, rx) = mpsc::channel::<Result<Event, std::convert::Infallible>>(32);

    tokio::spawn(async move {
        match agent.run_stream(&req.message).await {
            Ok(mut stream) => {
                while let Some(event) = stream.recv().await {
                    let is_end = event.event_type == "done" || event.event_type == "error";
                    let data = serde_json::to_string(&event).unwrap_or_default();
                    let sse_event = Event::default().data(data);
                    if tx.send(Ok(sse_event)).await.is_err() {
                        break;
                    }
                    if is_end {
                        break;
                    }
                }
            }
            Err(e) => {
                let data =
                    serde_json::to_string(&StreamEvent::error(e.to_string())).unwrap_or_default();
                let _ = tx.send(Ok(Event::default().data(data))).await;
            }
        }
    });

    Sse::new(ReceiverStream::new(rx))
}

async fn handle_list_models(State(agent): State<Arc<Agent>>) -> Json<ModelsResponse> {
    let models = agent.list_models().await;
    let count = models.len();
    Json(ModelsResponse { models, count })
}

async fn handle_verify_models(State(agent): State<Arc<Agent>>) -> Json<Vec<ProviderVerification>> {
    let results = agent.verify_providers().await;
    Json(results)
}

async fn handle_list_tools() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "tools": [
            { "name": "file_read", "description": "读取文件内容" },
            { "name": "file_write", "description": "写入文件内容" },
            { "name": "shell", "description": "执行 Shell 命令" },
            { "name": "search", "description": "搜索文件内容" },
            { "name": "list_dir", "description": "列出目录" },
            { "name": "git", "description": "执行 Git 命令" },
        ]
    }))
}

async fn handle_doctor(State(agent): State<Arc<Agent>>) -> Json<DoctorResponse> {
    let checks = agent.doctor();
    let healthy = checks.iter().all(|c| c.status == "ok");
    Json(DoctorResponse { healthy, checks })
}

async fn handle_token_usage(State(agent): State<Arc<Agent>>) -> Json<serde_json::Value> {
    let stats = agent.stats().await;
    Json(serde_json::json!(stats))
}

async fn handle_session_info(State(agent): State<Arc<Agent>>) -> Json<serde_json::Value> {
    let stats = agent.stats().await;
    let messages = agent.messages().await;
    Json(serde_json::json!({
        "stats": stats,
        "messages": messages.len(),
    }))
}

async fn handle_clear_session(State(agent): State<Arc<Agent>>) -> Json<serde_json::Value> {
    agent.clear().await;
    Json(serde_json::json!({
        "status": "ok",
        "message": "会话已清空",
    }))
}

async fn handle_health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "time": chrono::Utc::now().to_rfc3339(),
    }))
}

// =============================================================================
// 会话管理（Phase 4）
// =============================================================================

async fn handle_list_sessions(State(agent): State<Arc<Agent>>) -> Json<serde_json::Value> {
    let sessions = agent.list_sessions();
    Json(serde_json::json!({
        "sessions": sessions,
        "count": sessions.len(),
    }))
}

async fn handle_create_session(State(agent): State<Arc<Agent>>) -> Json<serde_json::Value> {
    let session_id = agent.create_session().await;
    Json(serde_json::json!({
        "status": "ok",
        "session_id": session_id,
        "message": "新会话已创建",
    }))
}

async fn handle_load_session(
    State(agent): State<Arc<Agent>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match agent.load_session(&id).await {
        Ok(messages) => Ok(Json(serde_json::json!({
            "status": "ok",
            "session_id": id,
            "messages": messages.len(),
        }))),
        Err(e) => Ok(Json(serde_json::json!({
            "status": "error",
            "message": e,
        }))),
    }
}

async fn handle_delete_session(
    State(agent): State<Arc<Agent>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    match agent.delete_session(&id) {
        Ok(_) => Json(serde_json::json!({
            "status": "ok",
            "message": format!("会话 {} 已删除", id),
        })),
        Err(e) => Json(serde_json::json!({
            "status": "error",
            "message": e,
        })),
    }
}

async fn handle_current_session(State(agent): State<Arc<Agent>>) -> Json<serde_json::Value> {
    let session_id = agent.current_session_id().await;
    Json(serde_json::json!({
        "session_id": session_id,
    }))
}

async fn handle_session_messages(
    State(agent): State<Arc<Agent>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    match agent.load_session(&id).await {
        Ok(messages) => Json(serde_json::json!({
            "status": "ok",
            "session_id": id,
            "messages": messages,
            "count": messages.len(),
        })),
        Err(e) => Json(serde_json::json!({
            "status": "error",
            "message": e,
        })),
    }
}
