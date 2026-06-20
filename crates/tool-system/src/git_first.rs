//! Git-first 设计 - 借鉴 Aider
//!
//! 每次文件编辑操作后自动创建 git commit，所有修改都可回溯。
//! Aider 的核心设计原则：将 AI 编辑视为一等代码变更。

use tracing::{debug, info};

/// Git-first 管理器
pub struct GitFirst {
    enabled: bool,
    /// 是否自动提交（true=每次编辑后自动commit，false=手动commit）
    auto_commit: bool,
    /// 提交信息前缀
    commit_prefix: String,
}

impl GitFirst {
    /// 创建 Git-first 管理器
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            auto_commit: enabled,
            commit_prefix: "raven".to_string(),
        }
    }

    /// 检查是否在 git 仓库中
    pub fn is_git_repo(&self, path: &std::path::Path) -> bool {
        if !self.enabled {
            return false;
        }
        let repo_path = if path.is_file() {
            path.parent().unwrap_or(std::path::Path::new("."))
        } else {
            path
        };

        std::process::Command::new("git")
            .args(&["-C", repo_path.to_str().unwrap_or("."), "rev-parse", "--git-dir"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// 编辑前：保存当前状态（用于回滚）
    pub fn pre_edit(&self, path: &str) -> Result<Option<String>, String> {
        if !self.enabled || !self.is_git_repo(std::path::Path::new(path)) {
            return Ok(None);
        }

        let repo_path = std::path::Path::new(path)
            .parent()
            .unwrap_or(std::path::Path::new("."));

        // 获取当前 HEAD hash（用于回滚）
        let output = std::process::Command::new("git")
            .args(&[
                "-C",
                repo_path.to_str().unwrap_or("."),
                "rev-parse",
                "HEAD",
            ])
            .output()
            .map_err(|e| format!("git 失败: {}", e))?;

        if !output.status.success() {
            return Ok(None);
        }

        let head = String::from_utf8_lossy(&output.stdout).trim().to_string();
        debug!("pre-edit HEAD: {} ({})", &head[..7.min(head.len())], path);

        Ok(Some(head))
    }

    /// 编辑后：自动 add + commit
    pub fn post_edit(
        &self,
        path: &str,
        tool_name: &str,
        description: &str,
    ) -> Result<String, String> {
        if !self.enabled || !self.auto_commit {
            return Ok("Git-first 已禁用".to_string());
        }

        let repo_path = std::path::Path::new(path)
            .parent()
            .unwrap_or(std::path::Path::new("."));

        let repo_str = repo_path.to_str().unwrap_or(".");

        // 1. git add
        let add_output = std::process::Command::new("git")
            .args(&["-C", repo_str, "add", path])
            .output()
            .map_err(|e| format!("git add 失败: {}", e))?;

        if !add_output.status.success() {
            let stderr = String::from_utf8_lossy(&add_output.stderr);
            return Err(format!("git add 失败: {}", stderr));
        }

        // 2. 检查是否有变更要提交
        let diff_check = std::process::Command::new("git")
            .args(&["-C", repo_str, "diff", "--cached", "--quiet"])
            .output()
            .map_err(|e| format!("git diff 失败: {}", e))?;

        if diff_check.status.success() {
            // 没有变更（文件内容没变）
            return Ok("无变更需要提交".to_string());
        }

        // 3. git commit
        let commit_msg = format!(
            "[{}] {}: {}",
            self.commit_prefix,
            tool_name,
            if description.len() > 50 {
                format!("{}...", &description[..50])
            } else {
                description.to_string()
            }
        );

        let commit_output = std::process::Command::new("git")
            .args(&["-C", repo_str, "commit", "-m", &commit_msg])
            .output()
            .map_err(|e| format!("git commit 失败: {}", e))?;

        if !commit_output.status.success() {
            let stderr = String::from_utf8_lossy(&commit_output.stderr);
            // 如果是因为没有变更，不算错误
            if stderr.contains("nothing to commit") {
                return Ok("无变更".to_string());
            }
            return Err(format!("git commit 失败: {}", stderr));
        }

        // 获取新的 commit hash
        let hash_output = std::process::Command::new("git")
            .args(&["-C", repo_str, "rev-parse", "--short", "HEAD"])
            .output()
            .map_err(|e| format!("获取 commit hash 失败: {}", e))?;

        let hash = String::from_utf8_lossy(&hash_output.stdout)
            .trim()
            .to_string();

        info!("Git-first: 已提交 {} ({}", hash, path);

        Ok(format!(
            "已自动提交: {} ({}",
            hash,
            if commit_msg.len() > 60 {
                format!("{}...", &commit_msg[..60])
            } else {
                commit_msg
            }
        ))
    }

    /// 回滚到编辑前的状态
    pub fn rollback(&self, path: &str, before_hash: &str) -> Result<String, String> {
        let repo_path = std::path::Path::new(path)
            .parent()
            .unwrap_or(std::path::Path::new("."));

        let repo_str = repo_path.to_str().unwrap_or(".");

        // git checkout 文件到之前的版本
        let output = std::process::Command::new("git")
            .args(&["-C", repo_str, "checkout", before_hash, "--", path])
            .output()
            .map_err(|e| format!("回滚失败: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("回滚失败: {}", stderr));
        }

        info!("Git-first: 已回滚 {} 到 {}", path, &before_hash[..7]);
        Ok(format!("已回滚到 {}", &before_hash[..7]))
    }

    /// 获取最近的提交历史
    pub fn recent_commits(&self, path: &str, count: usize) -> Result<String, String> {
        let repo_path = std::path::Path::new(path)
            .parent()
            .unwrap_or(std::path::Path::new("."));

        let output = std::process::Command::new("git")
            .args(&[
                "-C",
                repo_path.to_str().unwrap_or("."),
                "log",
                "--oneline",
                "-n",
                &count.to_string(),
                "--grep",
                &format!(r"\[{}\]", self.commit_prefix),
            ])
            .output()
            .map_err(|e| format!("获取提交历史失败: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() {
            Ok("暂无 Agent 提交记录".to_string())
        } else {
            Ok(format!("Agent 提交历史:\n{}", stdout))
        }
    }
}

/// 便捷函数：为 file_edit 工具创建 Git-first 包装
pub fn create_for_edit() -> GitFirst {
    GitFirst::new(true)
}
