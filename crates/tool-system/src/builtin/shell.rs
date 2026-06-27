//! Shell 执行工具：ShellTool 及命令安全校验辅助

use crate::Tool;
use async_trait::async_trait;
use raven_types::{FunctionSchema, ToolSchema};
use serde_json::json;

pub struct ShellTool {
    /// 允许执行的命令白名单。为空时回退到内置安全默认集。
    pub allowed: Vec<String>,
    /// 命令超时（秒）
    pub timeout: u64,
}

impl Default for ShellTool {
    fn default() -> Self {
        Self {
            allowed: default_shell_allowed(),
            timeout: 30,
        }
    }
}

impl ShellTool {
    /// 用配置构造（白名单为空则使用内置默认集）
    pub fn with_config(allowed: Vec<String>, timeout: u64) -> Self {
        Self {
            allowed: if allowed.is_empty() {
                default_shell_allowed()
            } else {
                allowed
            },
            timeout: if timeout == 0 { 30 } else { timeout },
        }
    }
}

/// 内置安全默认命令集（按平台区分）
fn default_shell_allowed() -> Vec<String> {
    #[cfg(windows)]
    let cmds = [
        "dir", "type", "findstr", "where", "git", "go", "npm", "node", "echo", "cd", "more",
        "tree", "curl", "python", "cargo",
    ];
    #[cfg(not(windows))]
    let cmds = [
        "ls", "cat", "grep", "find", "git", "go", "npm", "node", "echo", "pwd", "head", "tail",
        "wc", "mkdir", "touch", "cp", "mv", "curl",
    ];
    cmds.iter().map(|s| s.to_string()).collect()
}

/// 将复合命令行切成多段，以便对每段的命令名分别做白名单校验。
///
/// 切分点包括：控制操作符 `&&`/`||`/`|`/`;`/`&`、换行符（`\n`/`\r`，shell 视为
/// 命令分隔）、命令替换 `$(...)` 与反引号 `` `...` ``（内层也是一条待执行命令）。
/// 引号（'...' / "..."）内的这些字符视为普通文本，不切分。这是语法上的保守切分，
/// 不求完全等价于 shell 解析，但足以阻止"用首个安全命令绕过白名单"的情况
/// （如 `echo ok && rm -rf /`、`echo ok$(curl evil)`、多行命令）。
fn split_command_segments(command: &str) -> Vec<String> {
    let mut segs = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        if in_single {
            if c == '\'' {
                in_single = false;
            }
            cur.push(c);
            continue;
        }
        if in_double {
            // 双引号内仍可能有命令替换 $(...) / 反引号，需切出内层命令校验
            match c {
                '"' => {
                    in_double = false;
                    cur.push(c);
                }
                '`' => segs.push(std::mem::take(&mut cur)),
                '$' if chars.peek() == Some(&'(') => {
                    chars.next();
                    segs.push(std::mem::take(&mut cur));
                }
                _ => cur.push(c),
            }
            continue;
        }
        match c {
            '\'' => {
                in_single = true;
                cur.push(c);
            }
            '"' => {
                in_double = true;
                cur.push(c);
            }
            // 命令替换：$( 开启一段内层命令
            '$' if chars.peek() == Some(&'(') => {
                chars.next();
                segs.push(std::mem::take(&mut cur));
            }
            // 反引号、右括号也作分隔（反引号开/闭、$(...) 的闭合）
            '`' | ')' => segs.push(std::mem::take(&mut cur)),
            // 换行符：shell 视为命令分隔
            '\n' | '\r' => segs.push(std::mem::take(&mut cur)),
            '&' | '|' => {
                // && / || / | / & 均为分隔符；连续两个一起吃掉
                segs.push(std::mem::take(&mut cur));
                if chars.peek() == Some(&c) {
                    chars.next();
                }
            }
            ';' => {
                segs.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    segs.push(cur);
    segs
}

/// 危险命令黑名单：即使在白名单内也一律拒绝执行。
/// 这是不可绕过的安全底线，防止破坏性操作。
fn is_dangerous_command(command: &str) -> Option<&'static str> {
    // 归一化：小写 + 把连续空白（含 tab/多空格）压成单个空格，
    // 防止 `rm  -rf`（多空格）、`rm\t-rf` 绕过；首尾各补一个空格，
    // 使带空格的模式能做"词边界"匹配（避免 `git add` 命中 `dd `）。
    let collapsed = command.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = format!(" {} ", collapsed.to_lowercase());
    // 破坏性删除 / 磁盘操作 / 关机等。短 token（rm/dd）用前后空格做词边界。
    const PATTERNS: &[(&str, &str)] = &[
        (" rm -rf", "递归强制删除"),
        (" rm -fr", "递归强制删除"),
        (" rm -r -f", "递归强制删除"),
        (" rm -f -r", "递归强制删除"),
        (" rmdir /s", "递归删除目录"),
        (" del /f", "强制删除"),
        (" del /s", "递归删除"),
        (" format ", "格式化磁盘"),
        ("mkfs", "格式化文件系统"),
        (" dd ", "磁盘块写入"),
        (":(){", "fork 炸弹"),
        ("shutdown", "关机/重启"),
        ("reboot", "重启"),
        ("> /dev/sda", "覆写磁盘设备"),
        (" chmod -r 777", "递归放开权限"),
        ("mkfs.", "格式化文件系统"),
    ];
    for (pat, reason) in PATTERNS {
        if lower.contains(pat) {
            return Some(reason);
        }
    }
    None
}

/// 将子进程输出的原始字节解码为字符串。
///
/// Windows 上输出编码不统一：内置命令（`dir`/`ver`/`systeminfo` 等）按当前
/// 控制台输出码页（简体中文系统为 GBK/CP936）输出，而 `git`/`cargo`/`node`
/// 以及 `type` 一个 UTF-8 文件时输出的是 UTF-8 字节。固定按某一种解码总会
/// 把另一类弄成乱码。这里先做严格 UTF-8 解码：成功即说明是 UTF-8，直接用；
/// 失败再按控制台码页解码。GBK 等多字节序列极少恰好构成合法 UTF-8，纯 ASCII
/// 在两种编码下又完全一致，因此该启发式在实践中可靠。
#[cfg(windows)]
pub(crate) fn decode_console_bytes(bytes: &[u8]) -> String {
    // 1. 优先 UTF-8（git/cargo/UTF-8 文件等）
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    // 2. 退回控制台输出码页（dir/ver 等内置命令）
    extern "system" {
        fn GetConsoleOutputCP() -> u32;
        fn GetOEMCP() -> u32;
    }
    // 安全：纯查询调用，无副作用。优先用控制台输出码页；
    // 若进程未关联控制台（返回 0），回退到系统 OEM 码页。
    let cp = unsafe {
        let c = GetConsoleOutputCP();
        if c == 0 {
            GetOEMCP()
        } else {
            c
        }
    };
    let encoding = match cp {
        65001 => encoding_rs::UTF_8,
        936 => encoding_rs::GBK,
        950 => encoding_rs::BIG5,
        932 => encoding_rs::SHIFT_JIS,
        949 => encoding_rs::EUC_KR,
        1252 => encoding_rs::WINDOWS_1252,
        _ => encoding_rs::UTF_8,
    };
    let (cow, _, _) = encoding.decode(bytes);
    cow.into_owned()
}

/// 将子进程输出的原始字节解码为字符串（非 Windows：按 UTF-8 处理）。
#[cfg(not(windows))]
pub(crate) fn decode_console_bytes(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }
    fn description(&self) -> &str {
        "执行 Shell 命令（有安全限制）"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            schema_type: "function".to_string(),
            function: FunctionSchema {
                name: "shell".to_string(),
                description:
                    "在本机默认 shell 中执行一条命令（Windows 走 cmd.exe，类 Unix 走 sh），并返回标准输出。\n\n何时使用：运行构建/测试、调用 git 之外的命令行工具等内置工具覆盖不到的操作。\n何时不要用：读文件、列目录、搜索内容时优先用 view / list_dir / search——它们跨平台一致，不受当前系统命令差异影响（例如 Windows 上没有 ls/pwd）。\n\n约束：仅允许白名单内的命令；破坏性命令（递归删除、磁盘格式化、关机等）一律拒绝；默认超时 30 秒。请按本机操作系统选择正确的命令。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "要执行的完整命令行。命令名（首个词）须在允许列表内，且需匹配本机系统（如 Windows 用 dir 而非 ls）。" },
                        "timeout": { "type": "integer", "description": "超时秒数（可选，默认 30）。" }
                    },
                    "required": ["command"]
                }),
            },
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let command = args["command"].as_str().ok_or("缺少 command 参数")?;
        let timeout = args["timeout"].as_u64().unwrap_or(self.timeout);

        // 危险命令黑名单：不可绕过的安全底线
        if let Some(reason) = is_dangerous_command(command) {
            return Err(format!(
                "已拒绝危险命令（{}）: '{}'\n此类破坏性操作被安全策略禁止，如确需执行请在终端手动运行。",
                reason, command
            ));
        }

        // 白名单检查：复合命令（&&、||、|、;、& 连接）按每段命令名分别校验，
        // 防止 `echo ok && rm -rf /` 这类用首个安全命令绕过白名单的情况。
        for seg in split_command_segments(command) {
            let cmd = seg.split_whitespace().next().unwrap_or("");
            if cmd.is_empty() {
                continue;
            }
            if !self.allowed.iter().any(|c| c == cmd) {
                return Err(format!(
                    "命令 '{}' 不在允许列表中。\n可在配置 'tools.shell.allowed' 中添加，当前允许: {}",
                    cmd,
                    self.allowed.join(", ")
                ));
            }
        }

        let output = tokio::time::timeout(std::time::Duration::from_secs(timeout), {
            // 跨平台 shell：Windows 用 cmd /C，其余用 sh -c
            #[cfg(windows)]
            let mut c = {
                let mut c = tokio::process::Command::new("cmd");
                c.arg("/C");
                c
            };
            #[cfg(not(windows))]
            let mut c = {
                let mut c = tokio::process::Command::new("sh");
                c.arg("-c");
                c
            };
            c.arg(command).output()
        })
        .await
        .map_err(|_| "命令超时")?
        .map_err(|e| format!("执行失败: {}", e))?;

        let stdout = decode_console_bytes(&output.stdout);
        let stderr = decode_console_bytes(&output.stderr);

        if !output.status.success() {
            // status 的 Display 在各平台已含 "exit code: N"，不再重复前缀
            return Err(format!("{}\n{}", output.status, stderr));
        }

        if !stderr.is_empty() {
            Ok(format!("{}\n[stderr]: {}", stdout, stderr))
        } else {
            Ok(stdout)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dangerous_command_detected() {
        assert!(is_dangerous_command("rm -rf /").is_some());
        assert!(is_dangerous_command("RM -RF ~/data").is_some()); // 大小写不敏感
        assert!(is_dangerous_command("sudo shutdown now").is_some());
        assert!(is_dangerous_command("mkfs.ext4 /dev/sdb").is_some());
        assert!(is_dangerous_command("dd if=/dev/zero of=/dev/sda").is_some());
    }

    #[test]
    fn test_safe_command_not_flagged() {
        assert!(is_dangerous_command("ls -la").is_none());
        assert!(is_dangerous_command("git status").is_none());
        assert!(is_dangerous_command("cat file.txt").is_none());
    }

    #[test]
    fn test_shell_with_config_empty_falls_back() {
        let tool = ShellTool::with_config(Vec::new(), 0);
        assert!(!tool.allowed.is_empty(), "空白名单应回退到默认集");
        assert_eq!(tool.timeout, 30, "0 超时应回退到 30");
    }

    #[test]
    fn test_shell_with_config_custom() {
        let tool = ShellTool::with_config(vec!["ls".to_string(), "echo".to_string()], 60);
        assert_eq!(tool.allowed.len(), 2);
        assert_eq!(tool.timeout, 60);
    }

    #[tokio::test]
    async fn test_shell_rejects_dangerous() {
        let tool = ShellTool::default();
        let res = tool
            .execute(serde_json::json!({"command": "rm -rf /tmp/x"}))
            .await;
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("危险命令"));
    }

    #[tokio::test]
    async fn test_shell_rejects_not_in_whitelist() {
        let tool = ShellTool::with_config(vec!["ls".to_string()], 30);
        let res = tool
            .execute(serde_json::json!({"command": "wget http://x"}))
            .await;
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("不在允许列表"));
    }

    #[test]
    fn test_split_command_segments() {
        assert_eq!(split_command_segments("ls"), vec!["ls"]);
        assert_eq!(
            split_command_segments("echo ok && rm -rf /"),
            vec!["echo ok ", " rm -rf /"]
        );
        assert_eq!(
            split_command_segments("a | b ; c & d || e"),
            vec!["a ", " b ", " c ", " d ", " e"]
        );
        // 引号内的操作符不切分
        assert_eq!(
            split_command_segments("echo \"a && b\""),
            vec!["echo \"a && b\""]
        );
    }

    #[tokio::test]
    async fn test_shell_rejects_compound_bypass() {
        // 首段安全（echo 在默认集），但第二段不在白名单，应整体拒绝
        let tool = ShellTool::with_config(vec!["echo".to_string()], 30);
        let res = tool
            .execute(serde_json::json!({"command": "echo ok && wget http://x"}))
            .await;
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("不在允许列表"));
    }
}
