//! 跨平台支持模块
//!
//! 检测运行平台，提供平台特定的路径、命令和配置。
//! 支持: Linux / macOS / Windows / Android (Termux)

/// 支持的平台
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Platform {
    Linux,
    MacOS,
    Windows,
    Android, // Termux
    Unknown,
}

impl Platform {
    /// 检测当前平台
    pub fn detect() -> Self {
        #[cfg(target_os = "android")]
        return Platform::Android;

        #[cfg(target_os = "linux")]
        return Platform::Linux;

        #[cfg(target_os = "macos")]
        return Platform::MacOS;

        #[cfg(target_os = "windows")]
        return Platform::Windows;

        #[cfg(not(any(
            target_os = "android",
            target_os = "linux",
            target_os = "macos",
            target_os = "windows"
        )))]
        return Platform::Unknown;
    }

    /// 平台名称
    pub fn name(&self) -> &'static str {
        match self {
            Platform::Linux => "Linux",
            Platform::MacOS => "macOS",
            Platform::Windows => "Windows",
            Platform::Android => "Android (Termux)",
            Platform::Unknown => "Unknown",
        }
    }

    /// 配置目录路径
    ///
    /// 所有平台统一使用 `~/.raven`（家目录下的 `.raven`），与配置加载
    /// (`ConfigSystem::load`)、会话 (`session.rs`)、checkpoint (`checkpoint.rs`)、
    /// 提示词模板 (`prompts.rs`) 的实际存储路径保持一致。Windows 上家目录由
    /// `USERPROFILE` 决定（`dirs::home_dir()` 已处理），不再使用 `%APPDATA%`，
    /// 避免配置写入 `~/.raven` 而此处却指向 `%APPDATA%\raven` 的路径分裂。
    pub fn config_dir(&self) -> std::path::PathBuf {
        match self {
            Platform::Android => {
                // Android Termux: $HOME/.raven
                dirs::home_dir()
                    .map(|h| h.join(".raven"))
                    .unwrap_or_else(|| {
                        std::path::PathBuf::from("/data/data/com.termux/files/home/.raven")
                    })
            }
            _ => {
                // Linux / macOS / Windows: ~/.raven
                dirs::home_dir()
                    .map(|h| h.join(".raven"))
                    .unwrap_or_else(|| std::path::PathBuf::from(".raven"))
            }
        }
    }

    /// 会话持久化目录
    pub fn sessions_dir(&self) -> std::path::PathBuf {
        self.config_dir().join("sessions")
    }

    /// Checkpoint 目录
    pub fn checkpoints_dir(&self) -> std::path::PathBuf {
        self.config_dir().join("checkpoints")
    }

    /// 是否支持 shell 工具
    pub fn has_shell(&self) -> bool {
        !matches!(self, Platform::Windows)
    }

    /// 默认 Shell
    pub fn default_shell(&self) -> &'static str {
        match self {
            Platform::Windows => "cmd",
            Platform::Android => "bash",
            _ => "bash",
        }
    }

    /// 路径分隔符
    pub fn path_sep(&self) -> char {
        match self {
            Platform::Windows => '\\',
            _ => '/',
        }
    }

    /// 是否支持 TUI（需要真正的终端）
    pub fn supports_tui(&self) -> bool {
        // 检查是否有 TTY
        atty::is(atty::Stream::Stdout)
    }

    /// 获取安装说明
    pub fn install_instructions(&self) -> &'static str {
        match self {
            Platform::Linux => "curl -sSL https://get.raven.dev | bash",
            Platform::MacOS => "curl -sSL https://get.raven.dev | bash\n# 或使用 Homebrew:\n# brew install raven",
            Platform::Windows => "# 使用 PowerShell:\niwr -useb https://get.raven.dev/install.ps1 | iex\n\n# 或使用 cargo:\ncargo install raven",
            Platform::Android => "pkg install rust\ncargo install raven",
            Platform::Unknown => "cargo install raven",
        }
    }
}

/// 获取当前平台
pub fn current() -> Platform {
    Platform::detect()
}

/// 当前 CPU 架构（用于平台信息与提示词占位符）
pub fn arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "arm") {
        "arm"
    } else {
        "unknown"
    }
}

/// 启用终端 ANSI 转义支持。
///
/// 现代终端（Windows Terminal、conhost 较新版本）默认开启虚拟终端处理，
/// 但旧版 Windows 控制台需要显式开启 `ENABLE_VIRTUAL_TERMINAL_PROCESSING`，
/// 否则我们直接写入的 ANSI 颜色码会原样显示。直接 FFI 调用 kernel32，
/// 不引入额外依赖。在非 Windows 平台为空操作。
#[cfg(windows)]
pub fn enable_ansi_support() {
    use std::os::raw::c_void;

    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    const INVALID_HANDLE: isize = -1;

    extern "system" {
        fn GetStdHandle(n_std_handle: u32) -> *mut c_void;
        fn GetConsoleMode(h_console_handle: *mut c_void, lp_mode: *mut u32) -> i32;
        fn SetConsoleMode(h_console_handle: *mut c_void, dw_mode: u32) -> i32;
    }

    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        if handle.is_null() || handle as isize == INVALID_HANDLE {
            return;
        }
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) != 0 {
            let _ = SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

/// 启用终端 ANSI 转义支持（非 Windows：空操作）。
#[cfg(not(windows))]
pub fn enable_ansi_support() {}

/// 获取平台信息摘要
pub fn info() -> String {
    let p = current();
    let arch = arch();

    format!(
        "平台: {} ({}-{})\n配置目录: {}\nShell: {}\nTUI 支持: {}",
        p.name(),
        arch,
        std::env::consts::OS,
        p.config_dir().display(),
        p.default_shell(),
        if p.supports_tui() { "是" } else { "否" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_detection() {
        let p = Platform::detect();
        assert!(
            matches!(
                p,
                Platform::Linux | Platform::MacOS | Platform::Windows | Platform::Android
            ),
            "应检测到已知平台"
        );
    }

    #[test]
    fn test_config_dir() {
        let p = Platform::detect();
        let dir = p.config_dir();
        assert!(!dir.to_string_lossy().is_empty());
    }
}
