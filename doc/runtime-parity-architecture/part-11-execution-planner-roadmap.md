# Part 11 - Execution Planner Roadmap

## 1. 当前问题

OpenHarness 已经补齐了很多 capability，但 runtime 仍有两个结构性问题：

1. CLI 和 HTTP 还没有完全共用执行 loop。
2. tool execution 仍以串行为主，缺少受控并发。

这两个问题会影响：

- CLI/HTTP 行为一致性；
- event shape；
- skill/task/MCP 执行语义；
- approval/question resume；
- background task lifecycle；
- provider step recovery；
- 性能。

下一阶段路线图围绕三个方向：

- SessionRunner；
- Task background lifecycle；
- ToolBatchPlanner。

## 2. SessionRunner

### 2.1 目标形态

```text
SessionRunner
  -> resolve profile
  -> prepare ToolContext
  -> bind system prompt
  -> inject available skills
  -> preload child skills
  -> assemble provider messages
  -> call provider
  -> stream provider events
  -> execute built-in/MCP/skill/task tools
  -> append messages/events/parts
  -> handle approval/question pause
  -> resume pending tool results
  -> finish completed/failed/cancelled
```

这个 runner 应该成为 CLI、HTTP、TUI/Desktop indirect execution 的共同 runtime。

### 2.2 为什么现在必须做

之前 CLI 和 HTTP 各自演进，能快速补功能，但现在重复成本变高：

- profile 解析曾经重复；
- ToolContext 构造曾经重复；
- question-answer JSON 解析曾经重复；
- tool call event 构造曾经重复；
- skill event、task event、tool result append 仍有重复；
- provider step finish/error/pause 仍不完全一致。

共享 schema 和 `SessionRunnerFacade` 已经证明，小步抽取可行。

### 2.3 已完成的第一批 facade

已抽入共享层：

- AgentProfile/SkillConfig/TaskConfig schema；
- ToolContext construction；
- question-answer JSON parsing；
- `item/toolCall/started` event construction；
- `item/toolCall/completed` / `failed` event construction。
- tool-result message/session projection payload construction；
- skill session-event payload construction。
- terminal turn event envelope construction。

近期验证覆盖：

```bash
cargo test -p openagent-tools session_runner_facade_builds_shared_tool_call_events -q
cargo test -p openagent-cli binary_approval_and_question_responses_resume_paused_runs --test cli_commands -q
cargo test -p openagent-http-runtime app_bridge_protocol_contract_and_client_live_subscription --test http_runtime -q
cargo check -p openagent-cli -p openagent-http-runtime
```

### 2.4 建议抽取顺序

不要一次搬完整 loop。推荐顺序：

1. Shared ToolContext。
2. Shared question-answer parsing。
3. Shared tool-call event construction。
4. Shared tool-result message/projection payload construction。
5. Shared skill event payload construction。
6. Shared tool-result append to session。
7. Shared system prompt/profile binding。
8. Shared provider message assembly。
9. Shared provider-step result model。
10. Shared pending approval/question resume。
11. CLI/HTTP loop 包到 SessionRunner。

每一步都要有 CLI + HTTP 验收，防止 surface drift。

## 3. Task background lifecycle

### 3.1 目标状态机

```text
queued
  -> running
  -> completed
  -> failed
  -> cancelled
```

### 3.2 必要操作

- list；
- inspect；
- wait；
- promote；
- cancel；
- resume；
- output；
- foreground/background switch。

### 3.3 设计要求

Task lifecycle 必须 event-backed：

```text
task.created
task.started
task.progress
task.completed
task.failed
task.cancelled
```

TUI/Desktop 只消费 task tree 和 task events，不自己维护另一套队列。

### 3.4 当前状态

- foreground Task/subagent 路径较完整；
- HTTP background queue 有基础；
- CLI background execution 仍不完整；
- wait/promote/cancel/resume 还未成为统一 runtime contract；
- TUI/Desktop subagent panes 未完成。

## 4. Unified event model

当前 event 家族可用，但 envelope 仍不完全统一。目标不是所有 payload 一样，而是外壳一致：

```text
event_id
session_id
run_id
turn_id
kind
status
step
call_id/task_id/request_id
attributes
created_at_ms
```

优先统一这些事件：

- tool call；
- approval/question；
- skill discovered/loaded；
- MCP lifecycle；
- task lifecycle；
- checkpoint/restore；
- provider step。

统一 event 的收益：

- App Bridge replay 更稳定；
- TUI/Desktop projection 更简单；
- golden tests 更准确；
- eval/replay 更可靠。

## 5. ToolBatchPlanner

### 5.1 需求

当 provider 一次返回多个独立 tool calls，串行执行会浪费时间。但并发不能破坏：

- permission；
- approval pause；
- file safety；
- event order；
- session projection；
- deterministic tests。

因此并发必须由 runner 控制，而不是工具自己决定。

### 5.2 分阶段策略

| 阶段 | 行为 |
| --- | --- |
| Trace-only | 只记录哪些 tool call 可并行，实际仍串行 |
| Read-only concurrency | read/glob/grep/ls/code_search 并发 |
| Keyed concurrency | tools 声明 resource keys，冲突写串行 |
| Permission-aware | ask/approval 能暂停 batch |
| Session-aware | 结果按确定顺序投影到 session |

### 5.3 Runner 侧模型

```text
provider tool calls
  -> ToolBatchPlanner
  -> permission gate
  -> scheduler
  -> tool execution
  -> deterministic session projection
```

先 trace-only，再读并发，最后写并发。不要直接开启全量并发。

## 6. 与 ContextPackBuilder 的关系

SessionRunner 最终也应该接管 provider message assembly。

目标：

```text
session state
  -> ContextPackBuilder
  -> provider-specific request lowering
```

这能解决：

- context budget；
- compact boundary；
- loaded skills；
- MCP instructions；
- task/agent listing；
- provider-specific tool schema；
- model switch 后 provider-native metadata replay。

这是更大阶段，应该在 runner 基础事件和 tool append 收口后推进。

## 7. 开发节奏

每个阶段遵守：

1. 只抽一个边界。
2. CLI 和 HTTP 都改到共享 helper。
3. 加 tools/CLI/HTTP 至少一组测试。
4. 跑 fmt/check/focused tests。
5. 更新 parity matrix。
6. 提交并推远端。

这能避免 runner refactor 一次改太多，导致回归定位困难。

## 8. 完成标准

### SessionRunner phase

完成标准：

- CLI/HTTP 共用 tool-result session append；
- skill event recording 不重复；
- provider message assembly 共享；
- approval/question resume 共享；
- CLI/HTTP tests 通过；
- provider payload 不含 runtime config；
- App Bridge event shape 稳定。

### Task lifecycle phase

完成标准：

- queued/running/completed/failed/cancelled 全覆盖；
- wait/promote/cancel/resume 全测试；
- task tree API 能暴露状态迁移；
- CLI/HTTP/TUI/Desktop 消费同一 contract。

### ToolBatchPlanner phase

完成标准：

- trace-only 可观测；
- read-only 并发可测；
- keyed concurrency 阻止冲突写；
- permission pause 行为正确；
- session event/result ordering deterministic。

## 9. 下一步建议

下一步继续沿着 SessionRunnerFacade 收口：

1. 抽 shared tool-result append。
2. 抽 shared skill event recording。
3. 抽 provider-step finish/paused/failed result model。
4. 抽 Task handoff event 和 child session metadata。
5. 再启动 background lifecycle 状态机。

这条线比继续补 surface 更关键，因为它决定 OpenHarness 是否真正成为一个统一 runtime。

## 10. Roadmap 的实施原则

这个 roadmap 的重点不是“重写 runner”，而是用可验证的小阶段把重复执行逻辑逐步收进共享层。

实施原则：

1. 每次只抽一个边界。
2. 抽取对象必须是 CLI/HTTP 都在用的逻辑。
3. 先加 shared tests，再改 surface。
4. 不在同一阶段同时改 provider、task、UI 和 compaction。
5. 每个阶段更新 parity matrix 或本组文档。
6. 一个阶段能独立提交、回滚、推送。

这样做的原因很现实：runner 是全系统热路径，大爆炸重构很容易把已有 CLI/HTTP/Desktop 验收打散。

## 11. SessionRunner 分阶段路线

### Phase A: Facade helpers

目标：先抽纯构造逻辑。

已包含或应包含：

- ToolContext construction；
- question-answer parsing；
- tool-call started/completed/failed event；
- tool-result projection；
- skill event payload；
- terminal turn event envelope。

验收：`openagent-tools` 单测 + CLI/HTTP focused tests。

### Phase B: Session append helpers

目标：把 tool result、assistant final、warnings、metadata append 收进共享 helper。

风险：session store side effect 变多，需要防止 CLI/HTTP 顺序变化。

验收：

- session trace；
- CLI golden；
- HTTP event replay；
- approval/question resume。

### Phase C: System prompt and context binding

目标：profile prompt、available skills、loaded skills、MCP instructions、file context 进入同一 ContextPackBuilder。

风险：provider prompt cache、compact、skill restore 都会受影响。

验收：

- fake provider request；
- skill available/load tests；
- compact restore tests；
- MCP instruction tests。

### Phase D: Provider step model

目标：runner 接收 provider normalized stream，产出统一 step result：

```text
Continue(tool calls)
Pause(approval/question)
Complete(answer)
Fail(error)
Interrupt(reason)
```

验收：CLI/HTTP turn completed/failed/interrupted events 一致。

### Phase E: Tool execution settlement

目标：built-in/MCP/skill/task tool 通过同一 settlement path：

- permission；
- execution；
- result normalization；
- event；
- session append；
- provider continuation。

验收：tool、MCP、skill、Task 各一组 focused tests。

### Phase F: Runner ownership

目标：CLI/HTTP 只负责入口参数、IO、transport，真正执行由 SessionRunner 完成。

验收：CLI/HTTP 行为保持，重复 loop 删除或变成薄 adapter。

## 12. Task lifecycle 分阶段路线

### Phase A: Event family

先定义 task.created/started/completed/failed/cancelled，不急于实现所有操作。

### Phase B: Queue and locks

HTTP background queue 和 CLI background 共享 task run lock，防止同一个 child session 被多 runner 同时推进。

### Phase C: Wait/inspect/output

先做只读操作，让用户能观察 background task。

### Phase D: Cancel/resume/promote

再做改变生命周期的操作。这里必须处理 provider cancellation、tool cancellation、session terminal state。

### Phase E: UI projection

TUI/Desktop subagent pane 消费 task tree 和 task events。

## 13. ToolBatchPlanner 分阶段路线

并发执行要保守推进。

### Phase A: Trace-only

标记哪些 tool calls 理论上可并发，但仍串行执行。用 trace 验证分类是否合理。

### Phase B: Read-only concurrency

只允许 read/glob/grep/ls 这类无副作用工具并发。

### Phase C: Resource-key concurrency

工具声明 resource keys，比如 file path、workspace、MCP server id。冲突的写操作串行。

### Phase D: Permission-aware batch

ask/approval 可以暂停整个 batch 或拆分 batch，不能让部分写操作绕过审批。

### Phase E: Deterministic settlement

即便并发执行，session event/result 投影也要按确定顺序输出，保证 golden/replay 稳定。

## 14. 风险控制

| 风险 | 控制方式 |
| --- | --- |
| CLI/HTTP event shape 漂移 | shared event builders + golden |
| Provider payload 被污染 | fake provider payload tests |
| Tool result 顺序变化 | deterministic settlement |
| Approval resume 回归 | focused resume tests |
| Skill compact 丢失 | compaction tests |
| Background task 卡死 | lifecycle timeout/cancel tests |
| UI 状态重复 | App Bridge projection tests |

每个 runner 阶段都应先列风险，再决定验收命令。

## 15. 完成后的目标形态

最终状态不是一个巨大的 `run()` 函数，而是一组清晰对象：

```text
SessionRunner
  owns turn lifecycle

ContextPackBuilder
  owns provider message assembly

ToolExecutor
  owns built-in/MCP/skill/task dispatch

TaskRuntime
  owns child sessions and lifecycle

EventProjector
  owns session/App Bridge projection

ProviderRuntime
  owns provider request lowering and stream normalization
```

CLI、HTTP、TUI、Desktop 都围绕这些对象工作。到这一步，OpenHarness 才真正从“多个入口都能跑 agent”变成“多个入口共享同一个 agent runtime”。
