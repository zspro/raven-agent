# CLAUDE.md

本文件为 AI 编码助手（以及新加入的开发者）提供在本仓库工作时的指导。

> **Raven 🐦‍⬛** — A sharp, cross-platform AI agent in Rust. 轻量、快速、跨平台的 Rust AI Agent。Think like a raven. Code like the wind. 设计哲学是"少即是多"——刻意避免臃肿。Release 二进制 ~12MB。

---

## 1. 环境与构建（重要，先读这一节）

本机的 Rust 工具链**不在标准位置**，而是装在 `D:\Environment` 下。直接敲 `cargo` 在 Git Bash / PowerShell 里**找不到命令**，必须先设好环境变量并使用完整路径。

### 在 Git Bash 中构建

```bash
export CARGO_HOME=/d/Environment/cargo
export RUSTUP_HOME=/d/Environment/rustup
/d/Environment/cargo/bin/cargo.exe build --release -p cli
```

- cargo 版本：1.96.0
- 默认工具链：`stable-x86_64-pc-windows-gnu`
- 编译产物：`target/release/raven.exe`（约 12MB）

> Windows 原生 `cmd.exe` 的交互式会话能直接用 `cargo`（用户已配置好交互式 PATH），但通过工具/脚本非交互调用时拿不到该 PATH，所以脚本里一律用上面的完整路径写法。

### 常用命令（与 `.github/workflows/ci.yml` 保持一致）

```bash
# 设好上面的两个环境变量后：
CARGO=/d/Environment/cargo/bin/cargo.exe

$CARGO check --all-targets                 # 快速类型检查
$CARGO build --release -p cli              # 构建 CLI 二进制
$CARGO test --workspace                    # 跑全部测试
$CARGO fmt --all -- --check                # 格式检查（CI 用）
$CARGO clippy --all-targets -- -D warnings # 静态分析（CI 视警告为错误）
```

构建较慢（release 全量约 2-3 分钟），改单个 crate 时优先用 `-p <crate>` 缩小范围。

---

## 2. 架构与代码地图

Cargo workspace，9 个 crate 位于 `crates/`，依赖方向自底向上（上层依赖下层）。改动时按下表定位：

| crate | 职责 | 典型改动场景 |
|-------|------|------|
| `agent-types` | 共享类型（`Message`/`Config`/`Error` 等） | 增删字段、改数据结构（牵一发动全身，慎改） |
| `config-system` | TOML 配置加载、热重载、提示词模板、平台检测 | 配置项、环境变量、`~/.raven` 路径、提示词模板 |
| `model-router` | OpenAI 兼容路由、提供商验证 | 接入新模型提供商、改请求/鉴权 |
| `tool-system` | 10 个内置工具 + MCP + FileEdit/View/Diff + Git-first + RepoMap | 增删工具、改工具行为、MCP 协议 |
| `context-engine` | 上下文管理、缓存、会话持久化、Checkpoint | 上下文压缩、会话存取、崩溃恢复 |
| `agent-core` | Agent 主循环、权限、Git-first 集成、诊断 | 对话编排、权限模式、`doctor` 检查 |
| `http-api` | axum HTTP API + SSE | REST 端点、流式响应 |
| `tui` | ratatui 终端全屏界面 | TUI 交互、快捷键、渲染 |
| `cli` | 命令行入口 + 交互设置（产出二进制 `raven`） | CLI 参数、子命令、输入处理、`/settings` 向导 |

前端入口（非 workspace crate）：`web/index.html`（Web UI）、`editors/vscode/`（VS Code 插件）、`desktop/`（Tauri 桌面端）。

**CLI 入口关键逻辑**：`crates/cli/src/main.rs` 的 `main()` 在无子命令时，合并「命令行消息 + 管道 stdin」后分场景（见 `read_piped_stdin` / `merge_prompt_with_stdin`），语义对齐 claw-code/Claude Code——**交互是默认，单次需显式意图**：
- `-p/--print` → 强制单次（`cmd_single`），无 prompt 报错退出码 2；
- 非 TTY（管道/CI）→ 一律单次，无输入报错退出码 2；
- TTY 且带文本 → 进交互（`cmd_chat_with_opening`），把文本作开场白先发一轮；
- TTY 无输入 → 进交互（`cmd_chat`）。
修改输入行为改这里；注意 `raven "问题"` 在终端下**不是**单次而是交互开场白。

### 核心运行机制（改 `agent-core` / `context-engine` 前先读）

- **Agent 循环**：添加用户消息 → 检查 Token 预算 → 超阈值则压缩上下文 → 取工具 Schema（非 readonly）→ 调 LLM → 有工具调用则执行并把结果回灌、继续循环，否则返回文本。
- **上下文压缩**：超过 `compact_threshold` 时保留最近 `keep_rounds` 轮完整对话，更早的生成摘要替换（摘要而非丢弃，避免上下文泄露）。
- **响应缓存**：缓存键 `SHA256(model + messages)` 前 16 位；只缓存纯文本响应（不缓存带工具调用的），TTL 1 小时，LRU 容量 100，命中时 Token 记为 cached。
- **权限控制**：`readonly` 拒绝全部 → `yes`/`auto` 放行全部（除 `denied_tools`）→ `ask` 仅放行 `allowed_tools`。改权限语义在 `agent-core`。
- **关键设计决策**：选 workspace 多 crate 而非单 crate（职责清晰、可独立编译测试）；HTTP 用 axum（与 tokio 集成好、SSE 原生、编译快）；`StreamEvent` 用 struct 而非 enum（SSE 序列化格式统一可预测：`{"type":"text","content":"..."}`）；SSE 用 mpsc 通道（异步标准模式，可后台逐步推送事件）。

---

## 3. 开发约定

- **改动后必须编译验证**：任何代码改动后，至少跑 `$CARGO check --all-targets`；改了 `cli` 等单 crate 可用 `-p` 加速。提交前确保 `fmt --check` 和 `clippy -D warnings` 通过（CI 会卡这两项）。
- **新功能/改 bug 要配测试**：workspace 已有单元测试（如 `config-system/src/tests.rs`），新增行为时同步加测试，跑 `$CARGO test --workspace`。
- **尊重"少即是多"**：本项目刻意保持精简。不要为单个需求引入大型依赖、过度抽象或防御性代码。新增 crate 依赖前先确认 workspace 里是否已有同类。
- **跨 crate 改类型**：动 `agent-types` 会波及所有上层 crate，改完务必全量 `check`/`test`。
- **配置与路径**：用户配置默认在 `~/.raven/`（`config.toml`、`sessions/`、`checkpoints/`）。环境变量前缀为 `RAVEN_`（`RAVEN_API_KEY`/`RAVEN_BASE_URL`/`RAVEN_MODEL`/`RAVEN_LOG_LEVEL`）。
- **平台兼容**：目标平台含 Linux/macOS/Windows/Android(Termux)。涉及路径、终端、文件系统的改动要考虑跨平台（参考 `config-system/src/platform.rs`）。
- **CLI 行为防呆**：涉及 stdin/终端的逻辑，记得区分 TTY 与非 TTY（管道/CI），避免在非交互场景下挂起或空耗 API 调用。
- **版本控制**：当前工作目录可能不是 git 仓库（视检出方式而定）。除非用户明确要求，不要自行 `git init`、提交或改动 git 配置。

## 4. 测试运行（无需真实 API 的冒烟检查）

```bash
BIN=./target/release/raven.exe
echo "" | "$BIN"; echo "退出码 $?"        # 非 TTY 无输入 → 应报错、退出码 2
"$BIN" -p < /dev/null; echo "退出码 $?"   # -p 但无 prompt → 应报错、退出码 2
echo "1+1=?" | "$BIN"                       # 管道输入 → stdin 当 prompt，单次（需 API Key）
echo "1+1=?" | "$BIN" -p                    # 强制单次，答完即退（需 API Key）
echo "代码片段" | "$BIN" "解释这段代码"     # 命令行 prompt 与管道内容合并
```

> 注意：TTY 下的 `raven "问题"`（进交互、发开场白）和裸 `raven`（进交互）无法在管道/CI 中冒烟测试，需在真实终端手验。

