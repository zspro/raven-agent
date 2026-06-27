//! Git-first 设计 - 借鉴 Aider
//!
//! 每次文件编辑操作后自动创建 git commit，所有修改都可回溯。
//! Aider 的核心设计原则：将 AI 编辑视为一等代码变更。

use tracing::{debug, info};

/// 按字符（而非字节）截断字符串，超长时追加 `...`。
/// 直接对含中文的字符串做 `&s[..n]` 会切在 UTF-8 字符中间导致 panic，故按 char 处理。
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let t: String = s.chars().take(max).collect();
        format!("{}...", t)
    } else {
        s.to_string()
    }
}

/// 取短 hash 前缀（hash 为 ASCII，但长度可能不足 7，故用 min 防越界）。
fn short_hash(hash: &str) -> &str {
    &hash[..7.min(hash.len())]
}

/// 把文件路径拆成 (git -C 的目录, 相对该目录的 pathspec)。
///
/// git 的 `-C <dir>` 会先切到 `dir`，其后的 pathspec 相对 `dir` 解析。
/// 因此若用 `path.parent()` 作 `-C` 目录，pathspec 必须是相对该目录的部分
/// （即文件名），否则会出现 `git -C src add src/main.rs` → 找 src/src/main.rs 的错误。
fn split_repo_and_file(path: &str) -> (String, String) {
    let p = std::path::Path::new(path);
    let dir = match p.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_string_lossy().into_owned(),
        _ => ".".to_string(),
    };
    let file = p
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    (dir, file)
}

/// Git-first 管理器
pub struct GitFirst {
    /// 用 Atomic 存放，便于配置热重载在运行时（通过 `&self`）切换开关，
    /// 无需 `&mut self` 或锁。
    enabled: std::sync::atomic::AtomicBool,
    /// 是否自动提交（true=每次编辑后自动commit，false=手动commit）
    auto_commit: std::sync::atomic::AtomicBool,
    /// 提交信息前缀
    commit_prefix: String,
}

impl GitFirst {
    /// 创建 Git-first 管理器
    pub fn new(enabled: bool) -> Self {
        use std::sync::atomic::AtomicBool;
        Self {
            enabled: AtomicBool::new(enabled),
            auto_commit: AtomicBool::new(enabled),
            commit_prefix: "raven".to_string(),
        }
    }

    /// 运行时重新配置开关（供配置热重载调用）。
    pub fn reconfigure(&self, enabled: bool, auto_commit: bool) {
        use std::sync::atomic::Ordering;
        self.enabled.store(enabled, Ordering::Relaxed);
        self.auto_commit.store(auto_commit, Ordering::Relaxed);
    }

    /// 当前是否启用。
    pub fn enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 当前是否自动提交。
    pub fn auto_commit(&self) -> bool {
        self.auto_commit.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 检查是否在 git 仓库中
    pub fn is_git_repo(&self, path: &std::path::Path) -> bool {
        if !self.enabled() {
            return false;
        }
        let repo_path = if path.is_file() {
            path.parent().unwrap_or(std::path::Path::new("."))
        } else {
            path
        };

        std::process::Command::new("git")
            .args([
                "-C",
                repo_path.to_str().unwrap_or("."),
                "rev-parse",
                "--git-dir",
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// 编辑前：保存当前状态（用于回滚）
    pub fn pre_edit(&self, path: &str) -> Result<Option<String>, String> {
        if !self.enabled() || !self.is_git_repo(std::path::Path::new(path)) {
            return Ok(None);
        }

        let (repo_str, _) = split_repo_and_file(path);

        // 获取当前 HEAD hash（用于回滚）
        let output = std::process::Command::new("git")
            .args(["-C", &repo_str, "rev-parse", "HEAD"])
            .output()
            .map_err(|e| format!("git 失败: {}", e))?;

        if !output.status.success() {
            return Ok(None);
        }

        let head = String::from_utf8_lossy(&output.stdout).trim().to_string();
        debug!("pre-edit HEAD: {} ({})", short_hash(&head), path);

        Ok(Some(head))
    }

    /// 编辑后：自动 add + commit
    pub fn post_edit(
        &self,
        path: &str,
        tool_name: &str,
        description: &str,
    ) -> Result<String, String> {
        if !self.enabled() || !self.auto_commit() {
            return Ok("Git-first 已禁用".to_string());
        }

        let (repo_str, file_spec) = split_repo_and_file(path);

        // 1. git add
        let add_output = std::process::Command::new("git")
            .args(["-C", &repo_str, "add", &file_spec])
            .output()
            .map_err(|e| format!("git add 失败: {}", e))?;

        if !add_output.status.success() {
            let stderr = String::from_utf8_lossy(&add_output.stderr);
            return Err(format!("git add 失败: {}", stderr));
        }

        // 2. 检查是否有变更要提交
        let diff_check = std::process::Command::new("git")
            .args(["-C", &repo_str, "diff", "--cached", "--quiet"])
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
            truncate_chars(description, 50)
        );

        let commit_output = std::process::Command::new("git")
            .args(["-C", &repo_str, "commit", "-m", &commit_msg])
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
            .args(["-C", &repo_str, "rev-parse", "--short", "HEAD"])
            .output()
            .map_err(|e| format!("获取 commit hash 失败: {}", e))?;

        let hash = String::from_utf8_lossy(&hash_output.stdout)
            .trim()
            .to_string();

        info!("Git-first: 已提交 {} ({})", hash, path);

        Ok(format!(
            "已自动提交: {} ({})",
            hash,
            truncate_chars(&commit_msg, 60)
        ))
    }

    /// 回滚到编辑前的状态
    pub fn rollback(&self, path: &str, before_hash: &str) -> Result<String, String> {
        let (repo_str, file_spec) = split_repo_and_file(path);

        // git checkout 文件到之前的版本
        let output = std::process::Command::new("git")
            .args(["-C", &repo_str, "checkout", before_hash, "--", &file_spec])
            .output()
            .map_err(|e| format!("回滚失败: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("回滚失败: {}", stderr));
        }

        info!("Git-first: 已回滚 {} 到 {}", path, short_hash(before_hash));
        Ok(format!("已回滚到 {}", short_hash(before_hash)))
    }

    /// 获取最近的提交历史
    pub fn recent_commits(&self, path: &str, count: usize) -> Result<String, String> {
        let (repo_str, _) = split_repo_and_file(path);

        let output = std::process::Command::new("git")
            .args([
                "-C",
                &repo_str,
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
