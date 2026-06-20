# 02 — Shell 工具真正的交互式确认

## 目标

执行 shell 命令(尤其敏感/写操作命令)前,弹出交互式确认,由用户实时选择是否放行,参考 claw-code / Claude Code 的权限确认体验。

## 当前状态

已落地两层非交互防线:

- `tools.shell.allowed` 可配置白名单
- `is_dangerous_command` 危险命令黑名单(`rm -rf`/`dd`/`format`/`shutdown` 等)不可绕过

缺的是「执行前实时问 y/n」的交互确认。

## 架构难点

`Agent::execute_tools`(`crates/raven-core/src/lib.rs`)是无交互的纯异步执行,
无法直接向用户提问。要实现交互确认需把「确认回调」从 UI 层(CLI/TUI)注入到 core。

## 设计方案(待定)

1. 在 `raven-core` 定义一个确认 trait / 回调:
   ```rust
   #[async_trait]
   pub trait Confirmer: Send + Sync {
       async fn confirm(&self, tool: &str, detail: &str) -> Decision; // Allow / Deny / AllowAlways
   }
   ```
2. `Agent` 持有 `Option<Arc<dyn Confirmer>>`,在 `execute_tools` 执行前调用
3. CLI 实现:终端 stdin 读 y/n
4. TUI 实现:弹出模态确认框(ratatui 弹层),方向键/y/n 选择
5. `AllowAlways` 写入 session 级白名单,避免反复打扰
6. 权限模式联动:`auto`/`yes` 跳过确认,`ask` 触发确认,`readonly` 直接拒绝

## 参考

- claw-code / Claude Code 的工具权限确认 UX(允许一次 / 始终允许 / 拒绝)

## 相关文件

- `crates/raven-core/src/lib.rs` — `execute_tools`、`PermissionChecker`
- `crates/cli/src/main.rs` — CLI 输入循环
- `crates/tui/src/lib.rs` — TUI 渲染与输入
