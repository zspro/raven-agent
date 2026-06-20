# Raven 🐦‍⬛

> **Think like a raven. Code like the wind.**
> 轻量、快速、跨平台的 Rust AI Agent。借渡鸦之智——善于观察、使用工具、解决复杂问题。

A sharp, cross-platform AI agent in Rust. Named after the raven — nature's most intelligent bird, known for tool use and problem-solving. Release binary ~12MB, zero-dependency deployment.

> 设计哲学：吸取 OpenClaw（配置复杂 80+ 模块）、Claude Code（臃肿 512K 行）的教训，做到**少即是多**。

---

## 核心特性 / Key Features

| 特性 Feature | 说明 Description |
|-------------|-----------------|
| **轻量高效 Lightweight** | Rust 实现，Release 二进制 ~12MB，内存 ~12MB |
| **多模型 Multi-model** | OpenAI / Anthropic / DeepSeek / 任意 OpenAI 兼容端点 |
| **提供商验证 Provider verification** | 连通性 + 功能测试 + SHA256 指纹，自动检测异常端点 |
| **10 个内置工具 Built-in tools** | file_read/write/edit/view/shell/search/list_dir/git/web_search/fetch_url |
| **Claude Code 工具** | FileEdit（diff 编辑）、View（代码查看）、成本追踪 |
| **MCP 协议** | 连接外部 MCP Server，无限扩展工具 |
| **Git-first** | 每次编辑自动 git commit，`/settings` 可开关 |
| **崩溃恢复 Crash recovery** | Checkpoint 系统，断线/崩溃后从断点继续 |
| **会话持久化 Session persistence** | 自动保存对话历史，启动时可恢复 |
| **7 个提示词模板 Prompt templates** | coder/reviewer/architect/debugger/rust_expert/writer/default |
| **跨平台 Cross-platform** | Linux / macOS / Windows / Android (Termux) |
| **前端入口** | CLI + TUI + Web + VS Code 插件（Tauri 桌面端开发中，暂不可用） |

## 快速开始 / Quick Start

```bash
# 1. 获取二进制 / Build binary
cargo build --release

# 2. 设置 API Key / Set API key
export RAVEN_API_KEY="sk-your-key"

# 3. 运行 / Run
raven "解释 Rust 的所有权系统"      # 终端里：进交互，并把这句作为开场白先发出
raven -p "解释 Rust 的所有权系统"   # 强制单次：答完即退（脚本/一次性任务用）
raven chat                          # 交互式对话 / Interactive chat
raven tui                           # TUI 全屏界面 / Full-screen TUI
raven serve                         # HTTP API + Web UI
raven doctor                        # 诊断检查 / Diagnostic check

# 管道输入 / Pipe input
cat error.log | raven "帮我分析这段日志"
git diff | raven "审查这些改动"
echo "1+1=?" | raven
```

> 交互 vs 单次（对齐 claude/claw）：
> - `raven`（裸，终端）→ 交互模式；`raven "问题"`（终端）→ 进交互并先发这句开场白。
> - `raven -p "问题"` → 强制单次，答完即退。
> - 管道/重定向/CI（非 TTY）→ 一律单次；非 TTY 且无任何输入 → 直接报错退出码 2。

## 安装 / Installation

### 从源码编译 / Build from source

```bash
git clone <repo>
cd raven
cargo build --release
# 二进制: ./target/release/raven (~12MB)
```

> 本机 Rust 工具链装在非标准位置时，参见 `CLAUDE.md` 的"环境与构建"一节。

### Docker

```bash
docker build -t raven .
docker run -it -e RAVEN_API_KEY=sk-... raven
```

### Android (Termux)

```bash
pkg install rust git
git clone <repo> && cd raven
cargo build --release
```

### VS Code 插件 / Extension

```bash
cd editors/vscode
npm install
npm run compile
# F5 调试，或 vsce package 打包
```

### Tauri 桌面端 / Desktop

> ⚠️ **暂不可用 / Currently unavailable**：桌面端为实验性脚手架，尚未达到可用状态，请使用 CLI / TUI / Web 入口。
> The desktop app is an experimental scaffold and is not yet usable. Please use the CLI / TUI / Web entry points instead.

```bash
cd desktop
cargo tauri build
# 或 cargo tauri dev 开发模式
```

## CLI 命令 / Commands

```
raven [消息]           # 终端：进交互并把消息作开场白；管道：单次
raven -p [消息]        # 强制单次提问，答完即退
raven chat             # 交互式对话
raven tui              # TUI 全屏界面
raven serve            # HTTP API 服务器（默认 0.0.0.0:8080）
raven doctor           # 诊断检查
raven models           # 列出可用模型
raven verify           # 验证模型提供商
raven init             # 初始化配置文件
raven --help           # 显示帮助
```

### 交互式命令 / Interactive commands

| 命令 | 说明 |
|------|------|
| `/quit` | 退出 |
| `/clear` | 清空会话 |
| `/compact` | 压缩上下文（省 Token） |
| `/stats` | Token 使用统计 |
| `/settings` | 设置界面（API Key、模型、权限、Git-first） |
| `/prompt` | 切换提示词模板（coder/reviewer/architect 等） |
| `/cost` | 查看本次会话成本 |
| `/help` | 显示帮助 |

### TUI 快捷键 / TUI shortcuts

| 键 | 功能 |
|-----|------|
| `i` | 进入输入模式 |
| `Enter` | 发送消息 |
| `Shift+Enter` | 换行 |
| `Esc` | 返回 Normal 模式 |
| `n` | 新对话 |
| `Ctrl+d` | 切换 diff 模式 |
| `↑/↓` 或 `PgUp/PgDn` | 滚动历史 |
| `q` / `Ctrl+c` | 退出 |

## 系统提示词模板 / Prompt templates

```
> /prompt

可用提示词模板:
  1. default      - 通用助手
  2. coder        - 编程专家（Claude Code 风格）
  3. reviewer     - 代码审查员
  4. architect    - 架构师
  5. debugger     - 调试专家
  6. rust_expert  - Rust 专家
  7. writer       - 技术写作
```

## 配置 / Configuration

### 环境变量 / Environment variables

```bash
export RAVEN_API_KEY="sk-..."           # API 密钥
export RAVEN_BASE_URL="https://..."     # API base_url（OpenAI 兼容端点）
export RAVEN_MODEL="gpt-4o"             # 模型
export RAVEN_LOG_LEVEL="info"           # 日志级别
```

### 配置文件 / Config file（~/.raven/config.toml）

```toml
model = "gpt-4o"

[permission]
mode = "ask"
allowed_tools = ["file_read", "file_write", "file_edit", "view", "search", "list_dir", "git", "web_search", "fetch_url"]

[context]
max_tokens = 128000
compact_threshold = 100000
keep_rounds = 6

# Git-first：每次编辑后自动 git commit
[git_first]
enabled = true
auto_commit = true
commit_prefix = "raven"

# MCP Server（可选）
# [[mcp_servers]]
# name = "fs"
# command = "npx"
# args = ["@anthropics/mcp-server-filesystem", "/home/user"]
```

### 配置加载顺序 / Config loading order

4 层覆盖（后者覆盖前者）：内置默认值 → 全局配置 `~/.raven/config.toml` → 项目配置 `./.raven/config.toml` → 环境变量（`RAVEN_` 前缀）。

### 权限模式 / Permission modes

| 模式 Mode | 行为 Behavior | 适用场景 Use case |
|-----------|--------------|-------------------|
| `ask` | 只允许 `allowed_tools` 列表中的工具 | 日常开发，安全优先 |
| `auto` | 允许所有工具（除 `denied_tools`） | 自动化脚本 |
| `yes` | 允许所有工具（除 `denied_tools`），比 auto 更宽松 | 可信环境 |
| `readonly` | 禁止所有工具，纯对话 | 仅需咨询建议 |

## HTTP API

启动服务器：`raven serve`（默认端口 8080）

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | `/api/v1/chat` | 同步对话 |
| POST | `/api/v1/chat/stream` | SSE 流式对话 |
| GET | `/api/v1/models` | 列出模型 |
| POST | `/api/v1/models/verify` | 验证提供商 |
| GET | `/api/v1/tools` | 列出工具 |
| GET | `/api/v1/doctor` | 诊断检查 |
| GET | `/api/v1/tokens` | Token 统计 |
| GET | `/api/v1/sessions` | 列出所有会话 |
| POST | `/api/v1/sessions` | 创建新会话 |
| GET | `/api/v1/sessions/:id` | 加载会话 |
| GET | `/api/v1/sessions/:id/messages` | 获取会话历史 |
| DELETE | `/api/v1/sessions/:id` | 删除会话 |
| GET | `/health` | 健康检查 |

## 借鉴的开源项目 / Inspiration

| 项目 Project | 借鉴的功能 Inspired by |
|-------------|------------------------|
| **Claude Code** | FileEditTool、ViewTool、成本追踪、推测执行、TUI 交互 |
| **Aider** | Git-first 设计、Repo Map、diff 编辑 |
| **DeepSeek-TUI** | 崩溃恢复 checkpoint、容量控制 |
| **OpenClaw** | Gateway 架构、Brain/Muscle 模型分离 |

## 项目结构 / Project structure

```
crates/
├── agent-types        (~700 lines) 共享类型 + 单元测试
├── config-system      (~850 lines) 配置 + 热重载 + 提示词模板 + 平台检测
├── model-router       (~620 lines) OpenAI 路由 + 提供商验证
├── tool-system        (~2550 lines) 10 工具 + MCP + FileEdit + View + Diff + Git-first
├── context-engine     (~980 lines) 上下文 + 缓存 + 会话持久化 + Checkpoint
├── agent-core         (~580 lines) Agent 循环 + 权限 + Git-first 集成 + 诊断
├── http-api           (~310 lines) axum + SSE + 会话 API
├── tui                (~1520 lines) Raven TUI + Markdown 渲染 + 化学/数学预处理
└── cli                (~900 lines) 命令行入口 + 交互设置

editors/vscode/         VS Code 插件
desktop/                Tauri 桌面端
web/index.html          Web UI
```

## License

MIT
