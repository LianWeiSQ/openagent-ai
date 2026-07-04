# Part 02 - Runtime Architecture

## 1. 架构目标

OpenHarness 当前的目标是一个 local-first agent runtime。它既要能像 CLI 一样直接跑任务，也要能像服务一样被 TUI、Desktop、远端 client 接入；既要能跑内置工具，也要能接 MCP、skill、subagent、provider catalog 和 plugin。

因此架构上需要避免两个倾向：

- 把所有逻辑塞进 CLI agent loop；
- 为每个产品入口写一套独立 runtime。

当前设计把系统拆成六个平面：

```text
Product Plane
  CLI / HTTP / App Bridge / TUI / Desktop

Routing Plane
  AgentProfile / SkillConfig / TaskConfig / Permission / Route

Capability Plane
  Built-in Tools / MCP / Skills / Task / Plugins / Provider Catalog

Execution Plane
  Agent Loop / Provider Loop / Task Runtime / SessionRunner Facade

State Plane
  Session Store / Messages / Parts / Events / Checkpoints / Compaction

Evidence Plane
  Golden Tests / Smoke Tests / Trace / Eval / Diagnostics
```

这个分层的核心原则是：产品入口只投影 runtime state，不创造 runtime state。

## 2. Crate ownership

Rust workspace 的拆分让 ownership 比旧结构清楚：

| 位置 | 责任 |
| --- | --- |
| `src/protocol` | shared wire/event/session types |
| `src/tools` | tool registry、ToolContext、skill/task/profile shared helpers |
| `src/session` | session persistence、trace、compaction、checkpoint state |
| `src/mcp` | MCP bridge/runtime helpers |
| `runtime/http` | HTTP Runtime、App Bridge routes、turn runtime、MCP lifecycle |
| `runtime/tui` | TUI state、render、App Bridge attach/control |
| `cli` | command surface、CLI run、profile discovery、binary smokes |
| `desktop` | packaged UI over App Bridge |

这个边界不是为了“拆包好看”，而是为后续共享 runner 做准备。CLI 和 HTTP 可以保留各自入口，但 profile、tool context、event shape、session append、permission、skill/task config 应该尽量归到共享层。

## 3. Product Plane

Product Plane 包含用户能接触到的入口：

- CLI：脚本化、一次性 run、agent/plugin/mcp/providers/models/session 等命令；
- HTTP Runtime：sessions、turns、events、skills、agents、MCP、tasks、checkpoint API；
- App Bridge：TUI/Desktop/remote client 的本地协议层；
- TUI：终端里的 session、file、model、agent、approval、diff、checkpoint 控制面；
- Desktop：更完整的 workspace UI、approval dock、MCP panel、checkpoint restore workflow。

对标 OpenCode，产品入口要共用 session/event/app bridge contract。否则同一件事在 CLI 支持、HTTP 不支持，或者 Desktop 显示的状态不是 runner 的真实状态。

## 4. Routing Plane

Routing Plane 决定“哪个 agent、哪个 skill、哪个 task、哪些工具、哪些权限”参与执行。

当前对象包括：

- AgentProfile；
- AgentProfileSchema；
- SkillConfig；
- TaskConfig；
- TaskSubagentDescriptor；
- permission ruleset；
- `permission.skill` / `skill_permissions`；
- `permission.task` / `task_permissions`；
- workspace isolation；
- hidden/disabled/model options。

设计上，routing 不属于 provider payload。provider 只应该看到 messages、model、tools、允许透传的 model options。`skill_roots`、`task_permissions`、`permission.skill` 这类字段必须在 provider boundary 之前被消化。

这就是 shared AgentProfile schema 的意义：CLI 和 HTTP 不能各自解释 profile。

## 5. Capability Plane

Capability Plane 是模型或 runtime 可调用的能力集合：

- built-in workspace tools：read/write/edit/bash/grep/glob 等；
- MCP tools：外部 server 暴露的工具；
- Skill tool：按 name 加载 skill content；
- Task tool：启动 subagent/child task；
- plugin registry：未来提供 skills、MCP、commands、providers、UI panes；
- provider/model catalog：模型与 provider 能力；
- debug/db/eval：运维和证据能力。

统一抽象是：

```text
capability descriptor
  -> permission/policy
  -> execution
  -> normalized result
  -> session event
```

Skill V2、MCP execution、Task tool 都应该尽量复用这条路径，而不是各自定义特殊结果。

## 6. Execution Plane

Execution Plane 当前仍有历史债：

- CLI 有自己的 agent loop；
- HTTP 有自己的 provider loop；
- Task/subagent 有 child session 分支；
- approval/question resume 有特殊路径；
- tool events 曾经在 CLI/HTTP 各自构造。

近期已经开始抽 `SessionRunnerFacade`：

- 共享 ToolContext 构造；
- 共享 question-answer JSON 解析；
- 共享 `item/toolCall/started`、`completed`、`failed` event 构造。

这不是最终 SessionRunner，但方向正确：先抽低风险、可验证的小边界，再把 provider step、tool append、skill load、task handoff、pause/resume 逐步收进去。

## 7. State Plane

State Plane 是 runtime 的事实来源：

- session info；
- messages；
- message parts；
- run/turn records；
- events；
- approval/question queues；
- tool result metadata；
- checkpoint/restore history；
- task tree metadata；
- skill discovered/loaded records；
- compaction boundaries。

如果 TUI 或 Desktop 需要展示某个状态，但 session/event 层没有这个状态，说明 runtime contract 还不完整。

对标 OpenCode V2，session 不只是 transcript，而是 admission、promotion、runner、event projection、context epoch、compaction 的组合。OpenHarness 当前还没有完整 context epoch，但已经沿着 session ledger 和 App Bridge event projection 方向推进。

## 8. Evidence Plane

OpenHarness 的稳定性依赖测试证据，而不是手工确认。原因很简单：runtime shape 比单点功能更容易悄悄漂移。

证据层包括：

- CLI binary smoke；
- CLI golden JSON；
- HTTP runtime endpoint tests；
- App Bridge protocol contract tests；
- TUI render/state tests；
- Desktop smoke workflows；
- session trace tests；
- tool runtime tests；
- diff-check / fmt / cargo check。

每个对标阶段都应该有验收面，而不是只记录“已实现”。

## 9. 架构约束

后续开发应保持这些约束：

1. Provider payload 不接收 runtime-only config。
2. Product surface 消费 session/App Bridge state，不自己发明执行状态。
3. Skill、Task、MCP 都经过 ToolContext 和 permission。
4. Child/subagent context 与 parent context 隔离，只通过 summary/metadata 回流。
5. Loaded skill content 必须能跨 compaction 保留。
6. Session events 要能被 CLI、HTTP、TUI、Desktop 共同理解。
7. Shared runner 每次只抽一个可测试边界，避免大爆炸式重写。

## 10. 当前架构风险

当前最大风险不是某个命令缺失，而是执行 loop 仍在多个入口复制：

- provider step assemble 仍不完全共享；
- tool result append 仍需继续收口；
- task background lifecycle 还未成为统一状态机；
- MCP/provider/plugin 的 product visibility 还不够完整；
- TUI/Desktop 的 subagent/task projection 仍不足。

下一阶段的结构性投资应继续围绕 SessionRunner，而不是在每个 surface 继续补重复逻辑。

## 11. 架构演化过程

OpenHarness 的分层不是一开始设计出来的，而是被需求逐步逼出来的。

### 11.1 从 CLI loop 到 runtime

最初 CLI loop 足够支撑：

```text
prompt -> provider -> tool -> answer
```

但一旦加入 session、approval、MCP、skill、subagent、Desktop，就会发现 loop 内部有太多隐含状态。于是第一步是把事实从 loop 里抽出来：messages 进 session，tool call 进 event，profile 进 schema，MCP 进 registry。

### 11.2 从 surface ownership 到 shared ownership

早期每个 surface 为了快速可用，会自己解释一部分状态：

- CLI 解析 profile；
- HTTP 也解析 profile；
- TUI 维护显示状态；
- Desktop 维护局部 workflow state。

这种方式推进快，但会产生 runtime drift。后续架构调整的原则变成：surface 可以拥有交互，但不能拥有事实。事实必须下沉到 shared crate、HTTP Runtime、session store 或 registry。

### 11.3 从 helper 到 runner

直接重写完整 SessionRunner 风险太高，所以目前采用“先 helper，后 runner”的路线：

1. 先把纯函数和低风险结构抽进 `openagent-tools`；
2. 让 CLI/HTTP 同时调用；
3. 补工具层单测和 surface integration；
4. 等边界稳定后，再上移到真正 runner。

`SessionRunnerFacade` 属于这个过渡层。它不是最终架构，但能在不大爆炸重构的情况下，逐步减少重复 loop。

## 12. Ownership 规则

后续开发可以按下面规则判断代码应该放哪里。

| 问题 | 应放位置 | 不应放位置 |
| --- | --- | --- |
| Wire/event/session 类型 | `src/protocol` | CLI/HTTP 私有 struct |
| ToolContext、profile helper、skill/task policy | `src/tools` | surface command parser |
| Session 持久状态、trace、compaction | `src/session` | TUI/Desktop local state |
| MCP server config/auth/lifecycle helper | `src/mcp` + HTTP runtime | Desktop component |
| App Bridge route 和 SSE 投影 | `runtime/http` | CLI output formatter |
| TUI render/control | `runtime/tui` | core runtime crate |
| Desktop workflow UI | `desktop` | session store |

这张表不是固定不变，但能防止常见错误：为了实现一个 UI 功能，把 runtime state 写在 UI 层。

## 13. 典型反模式

后续对齐 OpenCode/Claude Code 时，应避免几类反模式。

### 13.1 Provider payload 背 runtime config

如果 `skills`、`skill_roots`、`permission`、`task_permissions`、`workspace_isolation` 出现在 provider request 里，说明 profile boundary 失守。

### 13.2 UI 自己判断执行状态

如果 Desktop 通过按钮点击结果推断 task completed，而不是读 session event/task tree，reload 后就会漂。

### 13.3 Subagent 只是 prompt prefix

如果 subagent 没有 child session、独立 ToolContext、permission、metadata，就不是一等 subagent，只是一次 prompt 拼接。

### 13.4 Skill 只是文件读取

如果 skill 没有 registry、available listing、permission、load event、compaction protection，就无法支撑长期上下文。

### 13.5 MCP 只是工具调用

如果 MCP 没有 config/auth/discovery/lifecycle/doctor，用户无法运营外部 capability。

## 14. 阶段验收

架构分层是否有效，可以用这些问题验收：

- CLI 和 HTTP 是否复用同一 profile schema；
- skill/task/MCP 是否都经过 ToolContext；
- session event 是否能被 App Bridge replay；
- TUI/Desktop reload 后是否能恢复同一状态；
- provider request 是否只包含 provider-facing 字段；
- child session 是否能独立追踪；
- docs/parity matrix 是否能指出已完成和未完成边界。

只要这些问题有一个需要“某个 surface 特殊处理”才能回答，就说明 shared runtime 还需要继续收口。
