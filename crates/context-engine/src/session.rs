//! 会话持久化
//!
//! 将会话历史保存到 `~/.raven/sessions/` 目录下的 JSON 文件。
//! 每个会话一个文件，格式简洁，便于人工查看和迁移。

use chrono::{DateTime, Local};
use raven_types::Message;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, error, info};

/// 会话元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    /// 会话唯一 ID (ULID 格式)
    pub id: String,
    /// 会话标题（自动从第一条用户消息提取）
    pub title: String,
    /// 创建时间
    pub created_at: DateTime<Local>,
    /// 最后更新时间
    pub updated_at: DateTime<Local>,
    /// 消息数量
    pub message_count: usize,
    /// 当前模型
    pub model: String,
}

/// 会话数据（完整内容）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub meta: SessionMeta,
    pub messages: Vec<Message>,
}

/// 会话存储
pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    /// 创建会话存储（目录不存在则自动创建）
    pub fn new(dir: impl AsRef<Path>) -> Result<Self, String> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建会话目录失败: {}", e))?;
        Ok(Self { dir })
    }

    /// 从默认位置创建 (~/.raven/sessions)
    #[allow(clippy::should_implement_trait)]
    pub fn default() -> Result<Self, String> {
        let home = dirs::home_dir().ok_or("无法获取家目录")?;
        Self::new(home.join(".raven").join("sessions"))
    }

    /// 创建新会话
    pub fn create(&self, model: impl Into<String>) -> SessionMeta {
        let now = Local::now();
        let id = generate_id();
        let meta = SessionMeta {
            id: id.clone(),
            title: "新会话".to_string(),
            created_at: now,
            updated_at: now,
            message_count: 0,
            model: model.into(),
        };
        debug!("创建新会话: {}", id);
        meta
    }

    /// 保存会话（自动更新标题和消息数）
    pub fn save(&self, meta: &mut SessionMeta, messages: &[Message]) -> Result<(), String> {
        meta.message_count = messages.len();
        meta.updated_at = Local::now();

        // 自动提取标题（从第一条用户消息）
        if meta.title == "新会话" {
            for msg in messages {
                if let Ok(json) = serde_json::to_string(msg) {
                    if json.contains("\"role\":\"user\"") || json.contains("\"role\": \"user\"") {
                        let preview: String = msg.content.chars().take(30).collect();
                        if !preview.is_empty() {
                            meta.title = preview;
                        }
                        break;
                    }
                }
            }
        }

        let session = Session {
            meta: meta.clone(),
            messages: messages.to_vec(),
        };

        let path = self.session_path(&meta.id);
        let content =
            serde_json::to_string_pretty(&session).map_err(|e| format!("序列化失败: {}", e))?;

        std::fs::write(&path, content).map_err(|e| format!("写入失败: {}", e))?;

        info!("会话已保存: {} ({} 条消息)", meta.id, meta.message_count);
        Ok(())
    }

    /// 加载会话
    pub fn load(&self, session_id: &str) -> Result<Session, String> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Err(format!("会话不存在: {}", session_id));
        }

        let content = std::fs::read_to_string(&path).map_err(|e| format!("读取失败: {}", e))?;

        let session: Session =
            serde_json::from_str(&content).map_err(|e| format!("解析失败: {}", e))?;

        debug!(
            "会话已加载: {} ({} 条消息)",
            session_id, session.meta.message_count
        );
        Ok(session)
    }

    /// 列出所有会话（按更新时间倒序）
    pub fn list(&self) -> Vec<SessionMeta> {
        let mut sessions = Vec::new();

        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) => {
                error!("读取会话目录失败: {}", e);
                return sessions;
            }
        };

        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    if let Ok(session) = serde_json::from_str::<Session>(&content) {
                        sessions.push(session.meta);
                    }
                }
                Err(e) => {
                    error!("读取会话文件失败 {}: {}", path.display(), e);
                }
            }
        }

        // 按更新时间倒序
        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        sessions
    }

    /// 删除会话
    pub fn delete(&self, session_id: &str) -> Result<(), String> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Err(format!("会话不存在: {}", session_id));
        }

        std::fs::remove_file(&path).map_err(|e| format!("删除失败: {}", e))?;

        info!("会话已删除: {}", session_id);
        Ok(())
    }

    /// 检查会话是否存在
    pub fn exists(&self, session_id: &str) -> bool {
        self.session_path(session_id).exists()
    }

    /// 获取会话文件路径
    fn session_path(&self, session_id: &str) -> PathBuf {
        self.dir.join(format!("{}.json", session_id))
    }
}

// =============================================================================
// 工具函数
// =============================================================================

/// 生成唯一会话 ID（时间戳 + 随机数）
pub(crate) fn generate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let rand = std::process::id();
    format!("{:x}{:x}", ts, rand)
}
