# Part 01 - Demand Evolution

## 1. 背景

OpenHarness 的需求不是从一开始就指向“完整 agent runtime”。早期目标更直接：提供一个可以在本地代码仓库中运行的 coding agent harness，能调用 provider，能执行 read/write/bash/search 等工具，能把结果返回给用户。

这个形态可以支撑 demo，也能完成一部分单轮工程任务。但当它开始承担真实开发工作时，需求很快发生变化：任务会跨多轮、会产生文件修改、会请求权限、会暂停等待问题回答、会需要恢复、会有多个 UI 入口、会接入 MCP、会委派 subagent、会加载 skill，还需要把这些过程留成可审计证据。

也就是说，需求从“模型会不会回答”变成了“runtime 能不能稳定组织工作”。

## 2. 总体演化路径

可以把 OpenHarness 的演化分成九个阶段：

| 阶段 | 原始诉求 | 架构升级点 |
| --- | --- | --- |
| 1. Tool-using core | 模型能调用本地工具 | tool registry、permission、ToolResult、trace |
| 2. Session ledger | 多轮任务能恢复和审计 | session store、messages、parts、events |
| 3. Context runtime | 长任务不丢上下文 | context budget、compaction、file context、instruction loading |
| 4. Rust workspace | 边界清晰、可维护 | protocol/tools/session/mcp/http/tui/cli crate 拆分 |
| 5. App Bridge | 多产品入口共享状态 | HTTP runtime、SSE、turn、approval/question、checkpoint |
| 6. MCP capability plane | 外部工具可配置、可诊断 | config/auth/discovery/lifecycle/execution/UI |
| 7. Subagent/Task | 复杂任务可委派 | Task tool、child session、lineage、isolation、background queue |
| 8. Skill runtime | 专业知识按需注入 | SkillConfig、registry、permission、available skills、Skill tool V2 |
| 9. Shared runner | CLI/HTTP 行为收敛 | shared schema、ToolContext facade、event facade、SessionRunner roadmap |

这条线说明 OpenHarness 的建设重点已经不是“补更多命令”，而是把 agent 所需的运行时事实变成显式对象：profile、session、event、skill、task、permission、provider、MCP、product surface。

## 3. 为什么不能停留在 CLI loop

单一 CLI loop 的典型结构是：

```text
prompt
  -> provider request
  -> tool call
  -> tool result
  -> final answer
```

这个结构的问题在于，很多关键事实没有归属：

- tool call 的状态只在当前内存里；
- approval/question 暂停以后，resume 逻辑容易散落；
- provider payload 和 runtime config 容易混在一起；
- HTTP/TUI/Desktop 会重复实现 session 投影；
- skill/subagent/MCP 只能变成“特殊分支”，无法统一进入权限和事件模型；
- 长任务 compact 后无法保证已加载知识和执行状态还在；
- 子任务中间工具调用会污染父上下文，或者完全不可观察。

因此 OpenHarness 的演进方向是把 loop 拆成可持久、可投影、可路由、可恢复的 runtime contract。

## 4. 对标 OpenCode 的需求启发

OpenCode 的参考价值在于 runtime 整体性。它的核心思想不是某个命令，而是多个 surface 共享同一套 session/runtime 层。

对 OpenHarness 的直接启发包括：

- CLI、TUI、App 不能各自解释 profile、permission、provider、session；
- SessionRunner 是承接 provider step、tool execution、event projection 的中心；
- provider/model catalog 是可操作资源，需要 list、refresh、auth、health、capability；
- skill 先以 available list 暴露，再由 `skill` tool 加载完整内容；
- MCP 需要 config/auth/discovery/lifecycle，而不只是 tool execution；
- plugin 应该作为 provider、skill、MCP、command、UI capability 的来源进入 registry；
- product UI 应该消费 session/event state，而不是反向创造 runtime state。

OpenHarness 已经把这些思想拆进多个阶段：CLI parity matrix、App Bridge、MCP lifecycle、skills API、shared AgentProfile schema、SessionRunner facade。

## 5. 对标 Claude Code 的需求启发

Claude Code 的关键启发是“上下文和代理不是字符串拼接，而是生命周期系统”。

几个设计思想直接影响了 OpenHarness：

- subagent 是一等对象，有独立 context、system prompt、tools、permission、model、skills；
- 启动 subagent 是 AgentTool/TaskTool 这样的模型可见工具，而不是隐藏 router；
- child session 中间 tool calls 不应该直接进入 parent context；
- skill 是可发现、可加载、可权限控制、可 compact restore 的运行时对象；
- skill 可以 fork 到独立 agent 执行，而不是必须污染主上下文；
- context 需要 typed source、delta、attachment、compaction boundary、resume restore。

OpenHarness 当前还没有完全达到 Claude Code 的 context OS 水平，但已经吸收了几个关键边界：SkillConfig 一等化、subagent skill preload、fork skill 到 Task、loaded skill compaction protection、child session metadata、task tree API。

## 6. 需求分类

### 6.1 执行可靠性

早期重点是工具能跑。后续要求变成：

- tool call started/completed/failed 事件稳定；
- tool result 能进入 session ledger；
- approval/question 能暂停和恢复；
- pending tool resume 有明确状态；
- CLI 和 HTTP 不再各写一套事件构造逻辑。

近期 `SessionRunnerFacade` 把 ToolContext 构造、question-answer JSON 解析、`item/toolCall/*` 事件构造收进共享层，就是这一类需求的延续。

### 6.2 状态持久化

真实工程任务要求 session 能回答：

- 用户说了什么；
- 模型调用了什么工具；
- 哪些文件被改了；
- 哪些 approval 被允许或拒绝；
- 哪些 skill 被发现和加载；
- 哪些 child task 被创建；
- compact 边界前后保留了什么。

这推动了 session store、events、parts、checkpoint、restore、task metadata、skill event 的建设。

### 6.3 能力路由

能力不再只有内置 tool。现在有：

- built-in workspace tools；
- MCP tools；
- Skill tool；
- Task/subagent tool；
- plugin-provided capabilities；
- provider/model catalog；
- debug/db/eval operations。

它们都需要通过 profile、permission、registry、ToolContext、session event 进入同一条 runtime 路径。

### 6.4 产品入口一致性

OpenHarness 已经不是 CLI-only：

- CLI 用于脚本化和快速验证；
- HTTP Runtime/App Bridge 用于 API、SSE、Desktop/TUI 接入；
- TUI 用于终端内操作；
- Desktop 用于可视化 workflow；
- future IDE/client 也应该接同一套 App Bridge。

这要求 product surface 是 projection，而不是 runtime owner。

### 6.5 可扩展性和运维

当 harness 支持 MCP、provider、plugin、GitHub、eval、debug/db 后，需求转向可运营：

- config/auth/secret redaction；
- doctor/debug；
- lifecycle start/stop/restart；
- provider health；
- smoke/golden test；
- packaging lifecycle；
- failure classification 和 replay evidence。

## 7. 当前阶段判断

OpenHarness 当前已经完成了多个关键收口：

- Rust workspace 边界基本确立；
- CLI command surface 已恢复到可对标 OpenCode 的基础状态；
- HTTP Runtime/App Bridge 已能承接 sessions、turns、events、approval/question、MCP、skills、tasks；
- Skill 这条链路已从配置、工具、权限、事件、API、compaction、subagent preload 形成闭环；
- AgentProfile/SkillConfig/TaskConfig 已从 CLI/HTTP duplicate parser 收到共享 schema；
- SessionRunnerFacade 已开始抽取 CLI/HTTP 共享 loop 能力。

还没有完成的关键点：

- CLI/HTTP provider-step/tool-call/task loop 仍未完全统一；
- Task background lifecycle 没有达到完整 wait/promote/cancel/resume；
- MCP OAuth/dynamic registration 仍是 P0 gap；
- provider catalog/login 还没有完全 OpenCode 化；
- TUI/Desktop 的 subagent pane、task tree navigation、plugin pane 仍不足。

## 8. 后续北极星

下一阶段不是继续堆 surface，而是继续把运行时中心抽出来：

```text
SessionRunner
  -> resolve profile
  -> bind system prompt and available skills
  -> build provider request
  -> stream provider response
  -> execute tools / MCP / skill / task
  -> persist session messages and events
  -> manage pause / resume / interrupt
  -> return completed / failed / cancelled
```

当这个边界稳定后，CLI、HTTP、TUI、Desktop 的差异会变成产品交互差异，而不是 runtime 行为差异。
