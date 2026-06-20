# TODO: 流式 Tool Call 完整实现

## 问题

OpenAI 兼容 API 的 streaming 模式下，tool call 参数是**分块增量发送**的。当前实现每个 chunk 都发一个 `StreamEvent::tool_call`，导致：
- TUI 收到空/不完整的 arguments → "参数解析错误: EOF while parsing"
- agent-core 的 tool 执行失败

---

## 各平台 Streaming Tool Call 协议对比

### 1. OpenAI（及所有 OpenAI 兼容：DeepSeek、硅基流动、NewAPI、OneAPI）

**流式 chunk 结构：**
```json
// chunk 1: 第一个 tool call 的首包（带 id + name）
{
  "choices": [{
    "delta": {
      "tool_calls": [{
        "index": 0,
        "id": "call_abc123",
        "type": "function",
        "function": { "name": "get_weather", "arguments": "" }
      }]
    },
    "finish_reason": null
  }]
}

// chunk 2: arguments 增量
{
  "choices": [{
    "delta": {
      "tool_calls": [{
        "index": 0,
        "function": { "arguments": "{\"location\":" }
      }]
    }
  }]
}

// chunk 3: arguments 继续
{
  "choices": [{
    "delta": {
      "tool_calls": [{
        "index": 0,
        "function": { "arguments": " \"Paris\"}" }
      }]
    }
  }]
}

// 结束
{
  "choices": [{
    "delta": {},
    "finish_reason": "tool_calls"
  }]
}
```

**关键规则：**
- `index` 字段标识第几个 tool call（支持并行）
- 首包带 `id` 和 `function.name`，后续包只有 `function.arguments` 增量
- `arguments` 是**字符串增量**，需拼接后才是完整 JSON
- 多个 tool call 通过不同 `index` 区分，可交替到达
- `finish_reason: "tool_calls"` 表示本轮全是工具调用

### 2. Anthropic Claude

**流式事件序列：**
```
event: message_start
data: {"type":"message_start","message":{"id":"msg_xxx","role":"assistant","content":[]}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_xxx","name":"get_weather","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"loc"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"ation\": \"Paris\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":15}}

event: message_stop
data: {"type":"message_stop"}
```

**关键规则：**
- 事件类型：`content_block_start`（带 id + name）→ `content_block_delta`（`input_json_delta` 增量）→ `content_block_stop`
- `index` 标识第几个 content block
- `partial_json` 是字符串增量，需拼接
- `stop_reason: "tool_use"` 表示工具调用
- 与文本 content block 混合流式发送

### 3. Google Gemini

**流式 chunk 结构：**
```json
{
  "candidates": [{
    "content": {
      "parts": [
        { "text": "Let me check the weather..." }
      ]
    }
  }]
}

{
  "candidates": [{
    "content": {
      "parts": [
        {
          "functionCall": {
            "name": "getWeather",
            "args": { "location": "Paris" }
          }
        }
      ]
    }
  }]
}
```

**关键规则：**
- Gemini 的 function call 在流式中是**完整对象**，不是增量
- `functionCall` 部分的 `args` 是完整 JSON 对象（不是字符串）
- 通常一个 chunk 包含完整的 function call
- 无需拼接，直接解析

### 4. NewAPI / OneAPI

- 本质是 OpenAI 兼容代理，转发底层提供商的 stream
- 流式格式与 OpenAI 完全一致（`delta.tool_calls[].function.arguments` 增量）
- 无需特殊处理，统一走 OpenAI 逻辑

---

## 当前实现状态

| 组件 | 状态 | 问题 |
|------|------|------|
| `openai_compat.rs` streaming | ⚠️ 修复中 | 每个 chunk 立即发送，未积累 |
| `agent-types/ToolCall` | ✅ 已加 `index` 字段 | - |
| `tool-system/Registry::execute` | ✅ 空参数 fallback | - |
| `tui/execute_turn` | ✅ 正确接收事件 | - |
| Anthropic streaming | ❌ 未实现 | 无 `openai_compat.rs` 对应模块 |
| Gemini streaming | ❌ 未实现 | 无对应模块 |

---

## 实现计划

### Phase 1: 修复 OpenAI streaming tool call 积累（当前）

**文件**: `crates/model-router/src/openai_compat.rs`

改动点：
1. ✅ `StreamDelta.tool_calls` 用 `ToolCall`（已有 `index` 字段）
2. ✅ streaming spawn 中用 `HashMap<usize, (id, name, arguments)>` 积累
3. ✅ `finish_reason` 时发送完整 tool call
4. ⚠️ 需要测试：deepseek / 硅基流动的实际 streaming 行为

### Phase 2: Anthropic provider

**文件**: `crates/model-router/src/anthropic.rs`（新建）

需要实现：
- Anthropic Messages API 的 streaming 解析
- `content_block_start` / `content_block_delta` / `content_block_stop` 事件处理
- `input_json_delta` 增量拼接
- 转换为统一的 `StreamEvent` 格式

### Phase 3: Gemini provider

**文件**: `crates/model-router/src/gemini.rs`（新建）

需要实现：
- Gemini generateContent streaming 解析
- `functionCall` parts 提取
- 转换为统一的 `StreamEvent` 格式
- 无需增量拼接（完整对象）

### Phase 4: 统一 ToolCall 解析

**文件**: `crates/agent-core/src/lib.rs`

当前 `run_stream` 中 tool call 解析：
```rust
// 需要确认：从 stream event 解析的 ToolCall 是否需要过滤 index
```

---

## 测试矩阵

| 提供商 | 模型 | 单工具调用 | 并行工具调用 | 纯文本 |
|--------|------|-----------|-------------|--------|
| DeepSeek | deepseek-v4-flash | | | ✅ |
| 硅基流动 | Qwen/LLaMA | | | |
| OpenAI | gpt-4o | | | |
| Anthropic | claude-3-5-sonnet | | | |
| Gemini | gemini-2.0-flash | | | |

---

## 参考文档

- OpenAI Streaming: https://platform.openai.com/docs/api-reference/chat/streaming
- Anthropic Streaming: https://docs.anthropic.com/en/api/messages-streaming
- Anthropic Tool Use: https://docs.anthropic.com/en/docs/build-with-claude/tool-use
- Gemini Function Calling: https://ai.google.dev/docs/function_calling
- NewAPI (GitHub): https://github.com/songquanpeng/one-api
