//! 诊断检查：汇总 API Key / 模型 / 提供商 / 工具 / 平台 / Git-first 状态。

use crate::Agent;

/// 诊断结果
#[derive(Debug, serde::Serialize)]
pub struct DoctorResult {
    pub check: String,
    pub status: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Agent {
    /// 运行诊断
    pub fn doctor(&self) -> Vec<DoctorResult> {
        let mut results = Vec::new();
        let cfg = self.config.read().unwrap();

        // API Key
        if cfg.api_key.is_none() {
            results.push(DoctorResult {
                check: "API Key".to_string(),
                status: "fail".to_string(),
                message: "未设置 API Key".to_string(),
                fix: Some("设置 RAVEN_API_KEY 环境变量或在配置文件中指定".to_string()),
            });
        } else {
            results.push(DoctorResult {
                check: "API Key".to_string(),
                status: "ok".to_string(),
                message: "已设置".to_string(),
                fix: None,
            });
        }

        // 模型
        results.push(DoctorResult {
            check: "模型".to_string(),
            status: "ok".to_string(),
            message: cfg.model.clone(),
            fix: None,
        });

        // 提供商（异步检查简化版）
        results.push(DoctorResult {
            check: "提供商".to_string(),
            status: "ok".to_string(),
            message: format!("{} 个提供商已注册", cfg.providers.len() + 1),
            fix: None,
        });

        // 工具
        results.push(DoctorResult {
            check: "工具".to_string(),
            status: "ok".to_string(),
            message: "10 个内置工具可用".to_string(),
            fix: None,
        });

        // 平台信息
        let platform = config_system::Platform::detect();
        results.push(DoctorResult {
            check: "平台".to_string(),
            status: "ok".to_string(),
            message: platform.name().to_string(),
            fix: None,
        });

        // Git-first
        let gf_status = if cfg.git_first.enabled {
            if cfg.git_first.auto_commit {
                "开启（自动提交）"
            } else {
                "开启（手动提交）"
            }
        } else {
            "关闭"
        };
        results.push(DoctorResult {
            check: "Git-first".to_string(),
            status: if cfg.git_first.enabled { "ok" } else { "info" }.to_string(),
            message: gf_status.to_string(),
            fix: None,
        });

        results
    }
}
