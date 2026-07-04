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
4. Shared tool-result append to session。
5. Shared skill event recording。
6. Shared system prompt/profile binding。
7. Shared provider message assembly。
8. Shared provider-step result model。
9. Shared pending approval/question resume。
10. CLI/HTTP loop 包到 SessionRunner。

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
