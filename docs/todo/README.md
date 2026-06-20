# Raven 待办任务清单

本目录用 Markdown 跟踪项目的待办事项与改进计划。每个较大的任务可单独建一个 `NN-标题.md` 文件,本 README 作为总索引。

## 进行中 / 待办

| 编号 | 任务 | 状态 | 说明 |
|------|------|------|------|
| 01 | TUI 表格/富文本兼容性优化 | 🔲 待办 | Markdown 表格、宽字符(中文/emoji)对齐、边框渲染兼容性不足 |
| 02 | Shell 工具交互式确认 | 🔲 待办 | 危险命令执行前的 y/n 实时确认,需贯穿 TUI/CLI 输入层 |
| 03 | 流式工具调用 | 🔲 待办 | 见 [../TODO-streaming-toolcalls.md](../TODO-streaming-toolcalls.md) |

## 已完成

| 任务 | 完成日期 | 说明 |
|------|----------|------|
| web_search 改用 Bing | 2026-06-20 | 从 DuckDuckGo 切换到 Bing 网页抓取,并修复正则双引号解析 bug |
| Shell 白名单可配置 | 2026-06-20 | 白名单改为读 `config.tools.shell.allowed`,新增不可绕过的危险命令黑名单 |
| 陈旧单元测试修复 | 2026-06-20 | raven-types / config-system 测试对齐当前类型,恢复 CI 全绿 |
| 配置目录迁移 | 2026-06-20 | `~/.agent` → `~/.raven` 配置迁移 |

## 备注

- 安全底线:`tools.shell.allowed` 可自定义放行的命令,但 `rm -rf`、`dd`、`format`、`mkfs`、`shutdown` 等破坏性命令由 `is_dangerous_command` 一律拦截,不受白名单影响。
- 改动后务必跑 `cargo check --all-targets` 与 `cargo test --workspace`。
