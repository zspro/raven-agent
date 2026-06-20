//! 崩溃恢复系统 - 借鉴 DeepSeek-TUI
//!
//! 在每次用户输入前写入 checkpoint，崩溃后可以恢复到之前的状态。
//! checkpoint 包含：会话 ID、消息历史、当前配置。

use raven_types::Message;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Checkpoint 数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// 会话 ID
    pub session_id: String,
    /// 消息历史（用户输入前的快照）
    pub messages: Vec<Message>,
    /// 系统提示词
    pub system_prompt: Option<String>,
    /// Token 使用统计
    pub input_tokens: usize,
    pub output_tokens: usize,
    /// 创建时间
    pub created_at: String,
    /// 序列号（用于增量恢复）
    pub seq: u64,
}

/// Checkpoint 管理器
pub struct CheckpointManager {
    dir: PathBuf,
    seq: u64,
}

impl CheckpointManager {
    /// 创建 checkpoint 管理器
    pub fn new(dir: impl AsRef<Path>) -> Result<Self, String> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("创建 checkpoint 目录失败: {}", e))?;

        // 加载最新的序列号
        let seq = Self::load_latest_seq(&dir);

        info!("Checkpoint 系统已启动 (seq={})", seq);

        Ok(Self { dir, seq })
    }

    /// 从默认位置创建
    pub fn default() -> Result<Self, String> {
        let home = dirs::home_dir().ok_or("无法获取家目录")?;
        Self::new(home.join(".raven").join("checkpoints"))
    }

    /// 写入 checkpoint（用户输入前调用）
    pub fn write(
        &mut self,
        session_id: &str,
        messages: &[Message],
        system_prompt: Option<&str>,
        input_tokens: usize,
        output_tokens: usize,
    ) -> Result<(), String> {
        self.seq += 1;

        let checkpoint = Checkpoint {
            session_id: session_id.to_string(),
            messages: messages.to_vec(),
            system_prompt: system_prompt.map(|s| s.to_string()),
            input_tokens,
            output_tokens,
            created_at: chrono::Local::now().to_rfc3339(),
            seq: self.seq,
        };

        // 写入文件
        let path = self.checkpoint_path(self.seq);
        let content = serde_json::to_string_pretty(&checkpoint)
            .map_err(|e| format!("序列化失败: {}", e))?;

        std::fs::write(&path, content)
            .map_err(|e| format!("写入 checkpoint 失败: {}", e))?;

        // 同时写入 latest（软链接的替代）
        let latest_path = self.dir.join("latest.json");
        let _ = std::fs::write(&latest_path, serde_json::to_string_pretty(&checkpoint).unwrap_or_default());

        // 清理旧的 checkpoint（保留最近 10 个）
        self.gc_checkpoints(10);

        debug!("Checkpoint #{} 已写入", self.seq);
        Ok(())
    }

    /// 清除 checkpoint（成功完成后调用）
    pub fn clear(&self) -> Result<(), String> {
        let latest_path = self.dir.join("latest.json");
        if latest_path.exists() {
            std::fs::remove_file(&latest_path)
                .map_err(|e| format!("清除 checkpoint 失败: {}", e))?;
        }
        info!("Checkpoint 已清除（会话成功完成）");
        Ok(())
    }

    /// 恢复最新的 checkpoint
    pub fn recover(&self) -> Option<Checkpoint> {
        let latest_path = self.dir.join("latest.json");
        if !latest_path.exists() {
            return None;
        }

        match std::fs::read_to_string(&latest_path) {
            Ok(content) => {
                match serde_json::from_str::<Checkpoint>(&content) {
                    Ok(cp) => {
                        info!(
                            "发现未完成的会话 checkpoint #{} ({} 条消息)",
                            cp.seq,
                            cp.messages.len()
                        );
                        Some(cp)
                    }
                    Err(e) => {
                        warn!("Checkpoint 解析失败: {}", e);
                        None
                    }
                }
            }
            Err(e) => {
                warn!("读取 checkpoint 失败: {}", e);
                None
            }
        }
    }

    /// 列出所有 checkpoint
    pub fn list(&self) -> Vec<(u64, PathBuf)> {
        let mut checkpoints = Vec::new();

        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(_) => return checkpoints,
        };

        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if let Some(stem) = path.file_stem() {
                let name = stem.to_string_lossy();
                if name != "latest" {
                    if let Ok(seq) = name.parse::<u64>() {
                        checkpoints.push((seq, path));
                    }
                }
            }
        }

        checkpoints.sort_by(|a, b| b.0.cmp(&a.0));
        checkpoints
    }

    // ===================================================================
    // 内部方法
    // ===================================================================

    fn checkpoint_path(&self, seq: u64) -> PathBuf {
        self.dir.join(format!("{:08}.json", seq))
    }

    fn load_latest_seq(dir: &Path) -> u64 {
        let mut max_seq = 0;
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                if let Some(stem) = entry.path().file_stem() {
                    if let Ok(seq) = stem.to_string_lossy().parse::<u64>() {
                        max_seq = max_seq.max(seq);
                    }
                }
            }
        }
        max_seq
    }

    fn gc_checkpoints(&self, keep: usize) {
        let mut checkpoints = self.list();
        if checkpoints.len() > keep {
            checkpoints.truncate(checkpoints.len() - keep);
            for (_, path) in checkpoints {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}
