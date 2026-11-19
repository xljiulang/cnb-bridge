# Reasoning Stream Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修正 Responses 流式翻译中的 reasoning item 生命周期，使同一响应内的多轮工具调用分别产生独立 reasoning item，同时不丢失已确认的字段回退、空白过滤和原始格式。

**Architecture:** `ResponsesStream` 继续作为单个上游 Chat SSE 到 Responses SSE 的状态机。每个 reasoning item 有明确的 begin/finish 生命周期；reasoning 的活动边界由 Chat 工具调用完成以及可见文本结束触发。reasoning 字段提取继续共用同一个优先级函数，非流式聚合仍沿用现有 `chat_to_responses` 路径。

**Tech Stack:** Rust 2024、serde_json、bytes、Tokio/Axum、现有 Rust 单元测试与 gateway 集成测试。

**Spec:** 本计划及 2026-11-20 审查发现。

## Global Constraints

- 不修改 `/v1/responses`、`/v1/chat/completions`、公开错误 envelope、UI、模型目录或 Codex 配置。
- 不增加依赖；不实现 P0 的 `previous_response_id` 或跨请求历史缓存。
- 有效 reasoning 的字节内容保持原样；纯空白 reasoning 不得创建 item。
- 每个 reasoning item 必须完整发送 `output_item.added`、`reasoning_summary_part.added`、若干 `reasoning_summary_text.delta`、`reasoning_summary_text.done`、`reasoning_summary_part.done`、`output_item.done`。
- `response.output` 的顺序必须与 `output_item.done` 的顺序一致；所有 `output_index` 唯一且递增。
- 同一工具轮次内的 reasoning 字段切换继续合并；完成一个工具调用后才允许开启下一段 reasoning。
- 保留当前工作区中 `reasoning_fragment` 的字段回退补丁，不覆盖用户已有修改。
- 提交信息使用 `修复: 完善流式思考生命周期`，直接提交到 `main` 并推送。

---

### Task 1: 收口现有字段回退补丁并建立事件顺序测试助手

**Files:**
- Modify: `src/responses/tests.rs`
- Verify: `src/responses/stream.rs`

**Interfaces:**
- Consumes: `ResponsesStream::push(&Value) -> Vec<Bytes>`、`ResponsesStream::finish() -> Vec<Bytes>`、现有 `terminal_response` 与 `named_events`。
- Produces: 测试助手 `event_names(events: &[Bytes]) -> Vec<String>`，按实际 SSE 顺序返回所有事件名。

- [ ] **Step 1: 写事件顺序辅助函数**

在 `src/responses/tests.rs` 顶部现有 helper 后加入：

```rust
fn event_names(events: &[Bytes]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| {
            std::str::from_utf8(event)
                .ok()?
                .lines()
                .find_map(|line| line.strip_prefix("event: ").map(str::to_string))
        })
        .collect()
}
```

- [ ] **Step 2: 将现有回退测试改名**

将 `stream_keeps_reasoning_sources_separate_when_upstream_switches_fields` 改名为：

```rust
fn stream_merges_reasoning_source_switch_within_one_item()
```

保留测试内容，并补充断言：

```rust
let deltas = named_events(&events, "response.reasoning_summary_text.delta")
    .into_iter()
    .filter_map(|event| event["delta"].as_str().map(str::to_string))
    .collect::<String>();
let done = named_events(&events, "response.reasoning_summary_text.done");
assert_eq!(deltas, "primary fallback");
assert_eq!(done.len(), 1);
assert_eq!(done[0]["text"], "primary fallback");
```

- [ ] **Step 3: 运行定向测试确认基线**

Run:

```powershell
cargo test responses::tests::stream_merges_reasoning_source_switch_within_one_item --lib
```

Expected: PASS；该步骤只确认当前未提交回退补丁已被测试固定。

---

### Task 2: 用失败测试固定多轮 reasoning 的协议要求

**Files:**
- Modify: `src/responses/tests.rs`

**Interfaces:**
- Consumes: `ResponsesStream::new`、`event_names`、`terminal_response`。
- Produces: 三个 RED 回归用例，分别覆盖工具轮次、reasoning 与工具同帧、无 finish 收尾。

**测试数据约定：** 每个工具调用必须有非空名称、稳定 `call_id` 和合法参数；第一个工具调用使用 `finish_reason: "tool_calls"`，第二个使用 `finish_reason: "stop"`，以明确两个上游回合。

- [ ] **Step 1: 增加跨工具轮次的 reasoning 拆分测试**

```rust
#[test]
fn stream_separates_reasoning_after_a_completed_tool_call() {
    let mut stream = ResponsesStream::new("resp_two_turns".to_string(), "model".to_string());
    let mut events = stream.push(&json!({
        "choices": [{
            "delta": { "reasoning_content": "first thought" },
            "finish_reason": null
        }]
    }));
    events.extend(stream.push(&json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "function": { "name": "read_file", "arguments": "{}" }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    })));
    events.extend(stream.push(&json!({
        "choices": [{
            "delta": { "reasoning_content": "second thought" },
            "finish_reason": null
        }]
    })));
    events.extend(stream.push(&json!({
        "choices": [{
            "delta": { "content": "done" },
            "finish_reason": "stop"
        }]
    })));
    events.extend(stream.finish());

    let output = terminal_response(&events, "response.completed")["response"]["output"]
        .as_array()
        .unwrap()
        .clone();
    let kinds = output.iter().map(|item| item["type"].as_str().unwrap()).collect::<Vec<_>>();
    assert_eq!(kinds, ["reasoning", "function_call", "reasoning", "message"]);
    assert_eq!(output[0]["summary"][0]["text"], "first thought");
    assert_eq!(output[2]["summary"][0]["text"], "second thought");

    let names = event_names(&events);
    let first_done = names
        .iter()
        .position(|name| name == "response.output_item.done")
        .unwrap();
    let added_positions = names
        .iter()
        .enumerate()
        .filter(|(_, name)| name.as_str() == "response.output_item.added")
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    assert_eq!(added_positions.len(), 4);
    assert!(added_positions[1] < first_done);
    assert!(first_done < added_positions[2]);
}
```

- [ ] **Step 2: 增加 reasoning 与工具同帧的顺序测试**

```rust
#[test]
fn stream_closes_same_delta_reasoning_before_tool_item_done() {
    let mut stream = ResponsesStream::new("resp_same_delta".to_string(), "model".to_string());
    let mut events = stream.push(&json!({
        "choices": [{
            "delta": {
                "reasoning_content": "use a tool",
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "function": { "name": "read_file", "arguments": "{}" }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    }));
    events.extend(stream.finish());

    let names = event_names(&events);
    let first_reasoning_done = names
        .iter()
        .position(|name| name == "response.reasoning_summary_text.done")
        .unwrap();
    let first_tool_done = names
        .iter()
        .position(|name| name == "response.output_item.done")
        .unwrap();
    assert!(first_reasoning_done < first_tool_done);
    assert!(named_events(&events, "response.reasoning_summary_text.delta")[0]["delta"]
        .as_str()
        .unwrap()
        .contains("use a tool"));
}
```

- [ ] **Step 3: 增加无 finish_reason 时的多 item 收尾测试**

```rust
#[test]
fn stream_finishes_open_reasoning_before_incomplete_terminal() {
    let mut stream = ResponsesStream::new("resp_missing_finish".to_string(), "model".to_string());
    let mut events = stream.push(&json!({
        "choices": [{
            "delta": { "reasoning_content": "partial thought" },
            "finish_reason": null
        }]
    }));
    events.extend(stream.finish());

    let incomplete = terminal_response(&events, "response.incomplete");
    assert_eq!(incomplete["response"]["output"][0]["type"], "reasoning");
    assert_eq!(
        incomplete["response"]["output"][0]["summary"][0]["text"],
        "partial thought"
    );
}
```

- [ ] **Step 4: 运行三个测试确认 RED**

Run:

```powershell
cargo test responses::tests::stream_separates_reasoning_after_a_completed_tool_call responses::tests::stream_closes_same_delta_reasoning_before_tool_item_done responses::tests::stream_finishes_open_reasoning_before_incomplete_terminal --lib
```

Expected: 第一个测试 FAIL，实际只有 1 个 reasoning；第二个测试可能 PASS；第三个测试用于固定当前 `finish()` 收尾语义。失败原因必须来自 item 生命周期，而不是测试编译错误。

---

### Task 3: 抽出单个 reasoning item 的 begin 与 finish

**Files:**
- Modify: `src/responses/stream.rs`
- Test: `src/responses/tests.rs`

**Interfaces:**
- Consumes: `TextTrack { id, index, text }`、`next_index`、`output`。
- Produces: `fn reasoning_start(&mut self) -> Vec<Bytes>`。
- Produces: `fn reasoning_done(&mut self) -> Vec<Bytes>`。
- Produces: `fn reasoning_text_fragment(&mut self, delta: &str) -> Vec<Bytes>`，调用 begin 后追加 delta。

- [ ] **Step 1: 抽出 reasoning_start**

把当前 `reasoning_delta` 中 `self.reasoning.is_none()` 分支移入：

```rust
fn reasoning_start(&mut self) -> Vec<Bytes> {
    if self.reasoning.is_some() {
        return Vec::new();
    }
    let index = self.next_index;
    self.next_index = self.next_index.saturating_add(1);
    let id = gen_id("rs");
    self.reasoning = Some(TextTrack {
        id: id.clone(),
        index,
        text: String::new(),
    });
    vec![
        sse_event(
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": index,
                "item": { "id": id, "type": "reasoning", "summary": [] }
            }),
        ),
        sse_event(
            "response.reasoning_summary_part.added",
            json!({
                "type": "response.reasoning_summary_part.added",
                "item_id": id,
                "output_index": index,
                "summary_index": 0,
                "part": { "type": "summary_text", "text": "" }
            }),
        ),
    ]
}
```

- [ ] **Step 2: 抽出 reasoning_done**

把 `finish()` 中当前 `if let Some(track) = self.reasoning.take()` 分支移入：

```rust
fn reasoning_done(&mut self) -> Vec<Bytes> {
    let Some(track) = self.reasoning.take() else {
        return Vec::new();
    };
    self.reasoning_pending.clear();
    vec![
        sse_event(
            "response.reasoning_summary_text.done",
            json!({
                "type": "response.reasoning_summary_text.done",
                "item_id": track.id,
                "output_index": track.index,
                "summary_index": 0,
                "text": track.text
            }),
        ),
        sse_event(
            "response.reasoning_summary_part.done",
            json!({
                "type": "response.reasoning_summary_part.done",
                "item_id": track.id,
                "output_index": track.index,
                "summary_index": 0,
                "part": { "type": "summary_text", "text": track.text }
            }),
        ),
        sse_event("response.output_item.done", {
            let item = json!({
                "id": track.id,
                "type": "reasoning",
                "summary": [{ "type": "summary_text", "text": track.text }]
            });
            self.output.push(item.clone());
            json!({
                "type": "response.output_item.done",
                "output_index": track.index,
                "item": item
            })
        }),
    ]
}
```

- [ ] **Step 3: 让 reasoning_delta 只负责 begin 和 delta**

替换为：

```rust
fn reasoning_delta(&mut self, delta: &str) -> Vec<Bytes> {
    let mut events = self.reasoning_start();
    let track = self
        .reasoning
        .as_mut()
        .expect("reasoning_start must create a track");
    track.text.push_str(delta);
    events.push(sse_event(
        "response.reasoning_summary_text.delta",
        json!({
            "type": "response.reasoning_summary_text.delta",
            "item_id": track.id,
            "output_index": track.index,
            "summary_index": 0,
            "delta": delta
        }),
    ));
    events
}
```

- [ ] **Step 4: 让 finish 使用 reasoning_done**

在 `finish()` 中把整段旧 reasoning 收尾替换为：

```rust
events.extend(self.reasoning_done());
```

保留随后 text 的 `response.output_text.done`、`response.content_part.done` 和 `output_item.done` 收尾逻辑。

- [ ] **Step 5: 运行 reasoning 定向测试**

Run:

```powershell
cargo test responses::tests::stream_ --lib
```

Expected: 现有 reasoning、空白过滤、字段回退、`apply_patch` 测试全部 PASS；Task 2 的多轮边界测试仍允许 FAIL，因为边界尚未接入。

---

### Task 4: 在工具轮次完成时关闭 reasoning，并保持事件顺序

**Files:**
- Modify: `src/responses/stream.rs`
- Test: `src/responses/tests.rs`

**Interfaces:**
- Consumes: `reasoning_done()`、`flush_ready_calls()`、`add_call_events()`、`tool_delta()`。
- Produces: `fn close_reasoning_before_tool_turn(&mut self) -> Vec<Bytes>`；在 `finish_reason` 为 `tool_calls` 或 `function_call` 时调用一次。
- Produces: 同一工具循环中后续 reasoning 使用新的 `TextTrack`。

**边界规则：**

1. 同一 `push` 内先处理 reasoning，再处理 content，再处理工具调用，保持当前顺序。
2. 工具调用轮次的 finish reason 到达时，先发出已有工具调用 `output_item.added`/参数 delta，再关闭 reasoning，再在 `finish()` 中输出工具 `output_item.done`。
3. 后续 `push` 中收到 reasoning 时，如果当前没有活动 reasoning track，就创建新 item；活动 track 必须等该 `push` 内所有工具调用和 finish reason 处理完成后再关闭，以免同一帧被拆成多个 reasoning。
4. 同一工具轮次内部出现的 reasoning 字段切换仍留在当前 track；不按 `reasoning_content`、`reasoning_details` 来源拆分。

- [ ] **Step 1: 添加轮次关闭函数**

```rust
fn close_reasoning_before_tool_turn(&mut self) -> Vec<Bytes> {
    self.reasoning_done()
}
```

- [ ] **Step 2: 只在当前 choice 声明工具轮次结束时调用**

在 `push()` 的 choice 循环中，先记录当前 choice 的 `finish_reason`，然后仅当该值属于 `tool_calls` 或 `function_call` 时调用：

```rust
let closes_tool_turn = matches!(
    choice
        .get("finish_reason")
        .and_then(Value::as_str),
    Some("tool_calls" | "function_call")
);
if closes_tool_turn {
    events.extend(self.flush_ready_calls());
    events.extend(self.close_reasoning_before_tool_turn());
}
```

不得在普通 `stop`、`length`、空 `finish_reason` 或没有工具调用的分片上关闭 reasoning；这些分片仍属于同一 assistant 输出。若同一 choice 内存在名称或 `call_id` 尚未到达的工具调用，不得因为此步骤发送伪造的 `output_item.added`；它们仍在 `finish()` 中按既有规则处理。若同一 choice 同时包含第二段 reasoning 和结束第一轮的工具调用，按现有 push 顺序先追加到旧 track，再在帧末关闭，不能把该帧拆成两个 item。

- [ ] **Step 3: 保持工具调用最终输出与 reasoning 的稳定顺序**

在 `finish()` 中先调用 `reasoning_done()`，再处理可见 text 收尾，随后保持现有工具添加、校验和 `output_item.done` 顺序。若工具轮次中 reasoning 已在 Task 4 Step 2 关闭，则 `reasoning_done()` 必须返回空事件，不重复发送 done。

- [ ] **Step 4: 运行 Task 2 的三个 RED 测试**

Run:

```powershell
cargo test responses::tests::stream_separates_reasoning_after_a_completed_tool_call responses::tests::stream_closes_same_delta_reasoning_before_tool_item_done responses::tests::stream_finishes_open_reasoning_before_incomplete_terminal --lib
```

Expected: PASS。断言必须确认：
- 2 个 reasoning item 各自的 summary 文本正确；
- 所有 `output_item.added` 与 `output_item.done` 配对；
- 工具 `output_item.done` 不会复用旧 reasoning 的 `output_index`；
- 最终 `response.completed.output` 的 4 个 item 顺序稳定。

- [ ] **Step 5: 运行全部 Responses 测试**

Run:

```powershell
cargo test responses --lib
```

Expected: 所有既有测试和新增测试 PASS。

---

### Task 5: 补齐 source、空白和历史边界回归

**Files:**
- Modify: `src/responses/tests.rs`
- No production behavior change unless a new test exposes a real defect.

**Interfaces:**
- Consumes: `reasoning_text(&Value) -> Option<String>`、`reasoning_item_text(&Value) -> Option<String>`、`responses_to_chat`。
- Produces: 对 mixed reasoning_details、空白首选字段回退、历史 reasoning 归属边界的显式回归覆盖。

- [ ] **Step 1: 增加混合 reasoning_details 测试**

```rust
#[test]
fn reasoning_details_ignores_blank_entries_and_keeps_valid_order() {
    assert_eq!(
        reasoning_text(&json!({
            "reasoning_details": [
                { "text": " " },
                { "text": "first" },
                { "text": "\n" },
                { "text": "second" }
            ]
        }))
        .as_deref(),
        Some("first\n\nsecond")
    );
}
```

- [ ] **Step 2: 增加流式空白首选字段与有效后备字段的逐事件断言**

```rust
#[test]
fn stream_reasoning_blank_source_switch_emits_only_non_blank_values() {
    let mut stream = ResponsesStream::new("resp_blank_switch".to_string(), "model".to_string());
    let mut events = stream.push(&json!({
        "choices": [{
            "delta": { "reasoning_content": " " },
            "finish_reason": null
        }]
    }));
    events.extend(stream.push(&json!({
        "choices": [{
            "delta": { "reasoning_details": [{ "text": "valid" }] },
            "finish_reason": "stop"
        }]
    })));
    events.extend(stream.finish());

    let deltas = named_events(&events, "response.reasoning_summary_text.delta")
        .into_iter()
        .filter_map(|event| event["delta"].as_str().map(str::to_string))
        .collect::<String>();
    assert_eq!(deltas, "valid");
}
```

- [ ] **Step 3: 增加历史 reasoning 归属测试**

```rust
#[test]
fn history_keeps_reasoning_with_the_assistant_turn_that_follows_it() {
    let request = json!({
        "model": "model",
        "input": [
            { "type": "reasoning", "summary": [{ "type": "summary_text", "text": "before call" }] },
            {
                "type": "function_call",
                "call_id": "call_1",
                "name": "read_file",
                "arguments": "{}"
            },
            {
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "ok"
            },
            { "type": "reasoning", "summary": [{ "type": "summary_text", "text": "after result" }] },
            {
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "done" }]
            }
        ]
    });
    let chat = responses_to_chat(&request, false).unwrap();
    let messages = chat["messages"].as_array().unwrap();
    assert_eq!(roles(messages), ["assistant", "tool", "assistant"]);
    assert_eq!(messages[0]["reasoning_content"], "before call");
    assert_eq!(messages[2]["reasoning_content"], "after result");
    assert_eq!(messages[2]["content"], "done");
}
```

- [ ] **Step 4: 运行新增测试**

Run:

```powershell
cargo test responses::tests::reasoning_details_ignores_blank_entries_and_keeps_valid_order responses::tests::stream_reasoning_blank_source_switch_emits_only_non_blank_values responses::tests::history_keeps_reasoning_with_the_assistant_turn_that_follows_it --lib
```

Expected: 全部 PASS；若历史测试失败，只修正历史归属逻辑，不改变 Task 4 的流式生命周期。

---

### Task 6: 端到端网关回归与兼容说明

**Files:**
- Modify: `src/gateway/tests.rs`
- Modify: `README.md`

**Interfaces:**
- Consumes: `gateway_with_static_responses_stream`、现有 SSE mock 与 `/v1/responses` 路由。
- Produces: 一个真实 HTTP SSE 回归，断言纯空白 reasoning 不创建 item、正文正常、terminal 为 `response.completed`。

- [ ] **Step 1: 增加网关级空白 reasoning 测试**

用静态 SSE 返回三个 chunk：`reasoning_content: ""`、`reasoning_content: " "`、`content: "ok"`，最后为 `finish_reason: "stop"` 和 `[DONE]`。断言：

```rust
assert!(!body.contains("response.reasoning_summary_part.added"));
assert!(!body.contains("response.reasoning_summary_text.delta"));
assert!(body.contains("response.output_text.delta"));
assert!(body.contains("event: response.completed"));
```

- [ ] **Step 2: 增加多轮 reasoning 的网关级回归**

静态 SSE 第一回合返回 `reasoning_content: "first"` 和工具调用，`finish_reason: "tool_calls"`；第二回合返回 `reasoning_content: "second"` 与 `content: "done"`，`finish_reason: "stop"`。断言 SSE 中出现两个不同的 reasoning item id，并且两个 `response.output_item.done` 的 reasoning summary 分别为 `first` 与 `second`。

- [ ] **Step 3: 更新 README 的 reasoning 说明**

在现有 Responses 回译段落末尾补充：

```markdown
纯空白 reasoning 不会生成空的“思考过程”项。上游在同一个工具回合内切换 `reasoning_content`、`reasoning` 或 `reasoning_details` 时会合并到当前 reasoning item；完成一轮工具调用后出现的新 reasoning 会创建新的独立 item。
```

- [ ] **Step 4: 运行网关定向测试**

Run:

```powershell
cargo test gateway::tests::responses_stream_ --lib
```

Expected: 新老 SSE gateway 测试全部 PASS。

---

### Task 7: 全量验证、真实烟测与提交

**Files:**
- Verify only; no source edits expected.

**Interfaces:**
- Consumes: 已完成的流式生命周期修改。
- Produces: 可提交的 `main` 与最新 Debug bridge 可执行文件。

- [ ] **Step 1: 格式检查**

Run:

```powershell
cargo fmt -- --check
```

Expected: exit code `0`。

- [ ] **Step 2: Responses 与全量测试**

Run:

```powershell
cargo test responses --lib
cargo test
```

Expected: 两组命令均通过，无 ignored 或 flaky 失败。

- [ ] **Step 3: Clippy**

Run:

```powershell
cargo clippy --all-targets -- -D warnings
```

Expected: exit code `0`。

- [ ] **Step 4: Debug 编译**

Run:

```powershell
pwsh -File .\scripts\build-debug.ps1
```

Expected: `target\debug\cnb-bridge.exe` 成功重建。

- [ ] **Step 5: 真实 Codex 烟测**

启动最新 `target\debug\cnb-bridge.exe`，用 Codex 完成一个至少两次工具调用的任务。检查最新 `%AppData%\cnb-bridge\logs\http-trace`：
- 第一轮工具调用前存在有效 reasoning 时，产生第一个 reasoning item；
- 第二轮出现有效 reasoning 时，产生第二个独立 reasoning item；
- 两轮的 `output_item.added/done` 顺序严格配对；
- Chat 上游无 400，Codex UI 显示两个独立思考块，正文和工具调用正常；
- `apply_patch` 输入的换行仍保持原始字节。

- [ ] **Step 6: 提交并推送**

```powershell
git add src/responses/stream.rs src/responses/tests.rs src/gateway/tests.rs README.md
git commit -m "修复: 完善流式思考生命周期"
git push origin main
```

Expected: `git rev-parse HEAD` 与 `git rev-parse origin/main` 相同，工作区干净。

## Self-Review

- 多轮 reasoning、工具边界、空白过滤、字段回退、历史归属和真实网关路径均有对应任务与测试。
- 保留现有未提交 fallback 修改；Task 1 先收口，不覆盖用户工作。
- 生产接口、SSE 事件族、错误 envelope、UI、模型目录和 P0 范围保持不变。
- 没有新增依赖或 P0 缓存实现。
