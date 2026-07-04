# Part 06 - Session, Context, And Events

## 1. 需求背景

Agent 工作不是一次函数调用。一个工程任务中会出现：

- 多轮用户输入；
- provider streaming；
- tool calls；
- approval/question pause；
- skill discovery/load；
- subagent child session；
- MCP lifecycle；
- 文件 diff 和 checkpoint；
- compact；
- interrupt/resume；
- UI reload。

如果只保存最终 answer，harness 无法解释、恢复、审计，也无法让 TUI/Desktop 显示真实进度。因此 session/context/event 是 OpenHarness 的核心状态层。

## 2. 对标参考

### 2.1 OpenCode Session V2

OpenCode V2 的 Session API 强调几件事：

- prompt admission 和 execution 分离；
- session input 先进入 durable inbox，再由 runner promote；
- SessionExecution 从 session id 路由到 location runner；
- SessionRunner 负责 provider turn、tool settlement、history reload；
- Context Epoch 记录 privileged system context；
- compaction 产生 durable checkpoint，而不是覆盖 transcript。

对 OpenHarness 的启发是：session 不只是消息列表，而是执行生命周期的账本。

### 2.2 Claude Code context lifecycle

Claude Code 的 context 体系更像 Context OS：

- system prompt 静态/动态分段；
- attachments 注入文件、MCP、skills、agents、todo、diagnostics；
- skill listing 和 invoked skill restore；
- MCP instructions delta；
- microcompact、session memory compact、full compact；
- compact 后恢复 tool/agent/MCP/skill delta；
- JSONL transcript 支持 resume。

OpenHarness 当前没有完整复制，但已经在 ContextPackBuilder、compaction、skill preservation、session events、checkpoint restore 上对齐方向。

## 3. Session store 的职责

Session store 当前承担：

- session latest state；
- messages；
- message parts；
- run/turn records；
- tool results；
- events；
- warnings；
- metadata；
- status；
- checkpoint references；
- restore history；
- task metadata；
- compaction boundaries；
- skill loaded output protection。

设计原则：

```text
runtime action
  -> session event/message/part
  -> App Bridge projection
  -> CLI/TUI/Desktop view
```

如果 UI 要展示某个状态，应该先进入 session/event，而不是 UI 自己推导。

## 4. Context runtime

OpenHarness 的 context 已经不再是简单 prompt string：

- instruction loading；
- file context；
- context budget；
- ContextPackBuilder；
- structured compaction；
- compact boundary messages；
- skill output preservation；
- checkpoint/restore evidence。

目标是能回答：

- 模型为什么看到了这段内容；
- 哪些上下文被纳入；
- 哪些内容被 compact；
- compact 后哪些语义锚点保留；
- resume 后如何重建。

## 5. Event model

当前事件家族包括：

- turn/run started/completed/failed/interrupted；
- step started/finished；
- `item/toolCall/started`；
- `item/toolCall/completed`；
- `item/toolCall/failed`；
- approval requested/resolved；
- question asked/answered/dismissed；
- skill.discovered；
- skill.loaded；
- MCP lifecycle/discovery/tool execution；
- checkpoint/restore；
- patch/diff；
- task/subagent lifecycle。

近期已抽出的共享能力：

- `SessionRunnerFacade::tool_call_started_event`；
- `SessionRunnerFacade::tool_call_finished_event`；
- CLI/HTTP 共享 tool call event 构造；
- pending resume path 也使用共享 event builder。

这一步很小，但意义明确：event shape 不能由每个 surface 自己拼。

## 6. Approval 和 Question

approval/question 是 session runtime 的暂停点，不是 UI 弹窗。

需要保存：

- request id；
- turn id；
- tool call id；
- question/options；
- approval risk/tool input；
- resolved action；
- denial/note；
- resume 后的 tool result。

TUI 和 Desktop approval dock 的能力来自这套 state：

- allow once；
- allow always；
- deny with note；
- question reply；
- dismiss；
- persisted history；
- reload 后仍能看到 resolved flow。

## 7. Skill 和 compaction

Skill 给 compaction 提出了硬需求：已加载 skill 的内容不能因为 compact 消失。

如果模型先加载一个 skill，随后历史被压缩，而 skill 内容没有被保留，后续行为就失去依据。OpenHarness 因此把 loaded skill output 作为 compaction 保护对象。

这条原则可以推广：

```text
compaction is semantic preservation, not blind truncation
```

未来 task state、MCP instructions、agent listing、plan/todo 也应按语义锚点处理。

## 8. Checkpoint 和 restore

工程 agent 必须能回答“改了什么”和“能不能回退”。

当前 checkpoint/restore 相关状态包括：

- diff/patch parts；
- checkpoint ids；
- restore event；
- restore history；
- affected files；
- Desktop timeline/detail cards；
- packaged smoke 验证 reload 后仍能看到恢复状态。

这类状态必须存在 session metadata 中，否则 Desktop reload 会丢。

## 9. 开发过程

session/context/event 是被产品需求逐步逼出来的：

1. Tool result 需要被 run 后检查。
2. 多轮 session 需要 latest state。
3. JSON/golden 输出需要稳定事件。
4. App Bridge 需要 SSE turn events。
5. TUI 需要远端 transcript 和 control state。
6. Approval/question 需要 pause/resume。
7. Checkpoint/restore 需要 durable metadata。
8. MCP 需要 lifecycle event。
9. Skill 需要 discovered/loaded event 和 compaction protection。
10. Subagent 需要 parent/child/task tree metadata。
11. SessionRunnerFacade 开始统一 event construction。

每次产品 surface 暴露缺口，正确做法都是把事实补进 session/event 层，再投影出去。

## 10. 验收证据

代表性命令：

```bash
cargo test -p openagent-session --test session_trace -q
cargo test -p openagent-http-runtime --test http_runtime -q
cargo test -p openagent-cli --test cli_commands -q
cargo test -p openagent-tools -q
```

近期 runner event 收口还覆盖：

```bash
cargo test -p openagent-tools session_runner_facade_builds_shared_tool_call_events -q
cargo test -p openagent-http-runtime app_bridge_protocol_contract_and_client_live_subscription --test http_runtime -q
```

## 11. 后续边界

1. 统一 permission/question/approval/diff/checkpoint/task event envelope。
2. Task lifecycle event 一等化。
3. ContextPackBuilder 成为唯一 provider-message assembly path。
4. compact 后恢复 skill/task/MCP/agent deltas。
5. crash recovery 和 session resume 更完整。
6. SessionRunner 写出 CLI/HTTP 一致 event sequence。
