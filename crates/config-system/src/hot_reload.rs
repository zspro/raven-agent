//! 配置热重载
//!
//! 自动检测配置文件变更并重新加载，无需重启进程。
//! 适用于：切换模型、调整权限、修改API Key 等场景。

use crate::ConfigSystem;
use raven_types::Config;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::RwLock;
use tokio::time::interval;
use tracing::{debug, info, warn};

/// 配置变更时执行的回调（接收新配置）。返回 Future 以便异步应用（如重建提供商）。
pub type ReloadCallback =
    Arc<dyn Fn(Config) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// 热重载管理器
pub struct HotReload {
    config_system: Arc<RwLock<ConfigSystem>>,
    watch_paths: Vec<PathBuf>,
    last_modified: Arc<RwLock<Vec<Option<SystemTime>>>>,
    /// 配置成功重载后回调（把新配置应用到运行中的 Agent）。
    on_reload: Option<ReloadCallback>,
}

impl HotReload {
    /// 创建热重载管理器
    pub fn new(config_system: Arc<RwLock<ConfigSystem>>) -> Self {
        let watch_paths = vec![
            // 全局配置
            dirs::home_dir()
                .map(|h| h.join(".raven").join("config.toml"))
                .unwrap_or_default(),
            // 项目配置
            PathBuf::from(".raven").join("config.toml"),
        ];

        let last_modified: Vec<Option<SystemTime>> = watch_paths.iter().map(|_| None).collect();

        Self {
            config_system,
            watch_paths,
            last_modified: Arc::new(RwLock::new(last_modified)),
            on_reload: None,
        }
    }

    /// 设置配置重载后的回调（用于把新配置应用到运行中的 Agent）。
    pub fn on_reload(mut self, cb: ReloadCallback) -> Self {
        self.on_reload = Some(cb);
        self
    }

    /// 启动后台热重载任务
    pub fn spawn(self) {
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(5));
            info!("配置热重载已启动 (每5秒检测)");

            loop {
                ticker.tick().await;

                if let Err(e) = self.check_and_reload().await {
                    debug!("热重载检测失败: {}", e);
                }
            }
        });
    }

    /// 手动触发重载
    pub async fn reload_now(&self) -> Result<String, String> {
        self.do_reload().await
    }

    // ===================================================================
    // 内部方法
    // ===================================================================

    /// 检查文件变更并重新加载
    async fn check_and_reload(&self) -> Result<(), String> {
        let mut changed = false;
        let mut last_modified = self.last_modified.write().await;

        for (i, path) in self.watch_paths.iter().enumerate() {
            if !path.exists() {
                continue;
            }

            let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();

            let prev = last_modified.get(i).copied().flatten();

            if prev != modified {
                // 文件发生了变化
                if prev.is_some() {
                    // 不是首次检测，确实是变更
                    info!("配置文件变更检测: {}", path.display());
                    changed = true;
                }
                // 更新记录
                if let Some(slot) = last_modified.get_mut(i) {
                    *slot = modified;
                }
            }
        }

        drop(last_modified);

        if changed {
            self.do_reload().await?;
        }

        Ok(())
    }

    /// 执行实际重载
    async fn do_reload(&self) -> Result<String, String> {
        info!("正在重新加载配置...");

        match ConfigSystem::load() {
            Ok(new_system) => {
                let cfg = new_system.config().clone();

                // 更新配置系统
                {
                    let mut guard = self.config_system.write().await;
                    *guard = new_system;
                }

                // 把新配置应用到运行中的 Agent（如重建提供商、切换权限）
                if let Some(cb) = &self.on_reload {
                    cb(cfg.clone()).await;
                }

                info!(
                    "配置已重载: model={}, permission={}",
                    cfg.model, cfg.permission.mode
                );

                Ok(format!(
                    "配置已重载:\n- 模型: {}\n- 权限: {}\n- Base URL: {}\n- 日志级别: {}",
                    cfg.model,
                    cfg.permission.mode,
                    cfg.base_url.as_deref().unwrap_or("默认"),
                    cfg.log_level
                ))
            }
            Err(e) => {
                warn!("配置重载失败: {}", e);
                Err(format!("配置验证失败: {}", e))
            }
        }
    }
}

/// 启动热重载（便捷函数）
pub fn spawn_hot_reload(config_system: Arc<RwLock<ConfigSystem>>) {
    let reloader = HotReload::new(config_system);
    reloader.spawn();
}

/// 启动热重载并在配置变更时回调（便捷函数）。
/// 回调用于把新配置应用到运行中的 Agent。
pub fn spawn_hot_reload_with_callback(
    config_system: Arc<RwLock<ConfigSystem>>,
    on_reload: ReloadCallback,
) {
    let reloader = HotReload::new(config_system).on_reload(on_reload);
    reloader.spawn();
}
