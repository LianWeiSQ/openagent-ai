# Part 05 - Subagent And Task Runtime

## 1. 需求背景

复杂工程任务不适合一直压在一个主上下文里。典型场景包括：

- 先只读搜索和代码理解；
- 单独规划方案；
- 并行实现不同模块；
- 用 reviewer agent 检查变更；
- 用专门 skill/agent 处理文档、MCP、测试、迁移；
- 长任务后台运行，主会话继续处理其他问题。

如果没有 subagent，主上下文会不断膨胀，工具权限难以分层，执行记录也混在一起。需求自然升级为：

- 独立 context；
- 独立 system prompt；
- 独立 tool access；
- 独立 permission；
- 可选 model/provider；
- 可选 skills；
- parent/child session lineage；
- foreground/background；
- resume/cancel/wait；
- workspace/worktree isolation；
- summary/result 回流。

## 2. 对标参考

### 2.1 Claude Code AgentTool

Claude Code 的 AgentTool 是最重要的参考。它不是内部 router，而是模型可调用的普通工具：

```text
main session model
  -> calls Agent tool
  -> harness creates subagent runtime
  -> subagent runs in its own context
  -> parent receives result/summary
```

AgentTool 支持 subagent type、model override、background、worktree/remote isolation 等概念。内置 agent 包括 general-purpose、explore、plan 等。

对 OpenHarness 的启发：

- subagent 必须是一等运行时对象；
- 启动 subagent 应该走工具路径；
- child tool calls 不应该直接污染 parent context；
- child session 需要 metadata 和 lineage；
- background task 是 runtime lifecycle，不是 UI 小功能。

### 2.2 OpenCode Task/SessionRunner

OpenCode 的参考重点在 Task 工具概念和 runner/session contract：

- task/subagent 通过 tool registry 暴露给模型；
- session runner 负责 provider step、tool execution、event projection；
- run coordinator 保证同一 session drain chain 串行，不同 session 可并行；
- background job 和 session event 是 UI 可观察的运行时状态。

OpenHarness 采用 Task tool 作为对外抽象，避免把 subagent 做成 CLI-only 参数。

## 3. OpenHarness 当前设计

当前链路：

```text
Task tool call
  -> resolve subagent descriptor/profile
  -> validate task permission and nesting guard
  -> create child session
  -> bind prompt/tools/model/permission/skills
  -> optionally isolate workspace
  -> execute foreground or queue background
  -> write child metadata/task tree
  -> return summary/metadata to parent
```

这里的关键是 child session。subagent 不是一次 provider call，也不是 prompt prefix，而是有自己的 session state。

## 4. Subagent profile

profile 可以描述：

- `id` / `name` / `description`；
- `mode: primary | subagent`；
- system prompt/body；
- model/provider；
- tools；
- permission；
- task permissions；
- skill config；
- workspace isolation；
- hidden/disabled；
- max steps；
- color/metadata。

OpenCode markdown agent 也能通过 frontmatter 加载，方便复用 `.opencode/agent/*.md` 的定义方式。

## 5. Task tool

Task tool 是模型启动 subagent 的主入口。典型参数：

- `description`：短任务描述；
- `prompt`：给 child agent 的完整任务；
- `subagent_type`：选择哪个 profile；
- `background`：是否后台运行；
- task/resume 相关 id；
- isolation 相关选项。

Task tool 必须走普通 tool path：

```text
tool descriptor
  -> permission
  -> ToolContext
  -> execution
  -> ToolResult
  -> session event
```

这样它才能和 skill/MCP/built-in tools 使用同一套审计和权限模型。

## 6. 已完成阶段

### Stage 1: profile loading

先把 built-in subagents、project-defined agents、OpenCode markdown agents 纳入统一 registry。

验收：

- `agent list/show/run` 能看到 built-ins；
- markdown agent frontmatter 被解析；
- mode/model/tools/permission 生效。

### Stage 2: Task tool registration

当存在可用 subagent descriptor 时注册 Task tool，并把可用 subagent 描述暴露给模型。

验收：

- fake provider 能调用 Task tool；
- Task tool 输入输出进入 session。

### Stage 3: explicit Task execution

实现 foreground child session 执行：

- 创建 child session；
- 写 parent/child metadata；
- 运行 child prompt；
- 父会话收到 result/summary。

### Stage 4: description-based auto routing

当用户请求明显匹配某个 subagent description 时，主 agent 可以自动委派。

设计上 auto routing 是便利路径，不替代 Task tool。长期方向仍是模型显式调用 Task。

### Stage 5: nesting guard

加入：

- task depth；
- lineage；
- parent/root session id；
- self-call 检测；
- recursion guard。

这防止 subagent 无限创建同类 subagent。

### Stage 6: workspace isolation

实现 isolated workspace/worktree metadata 和行为，让实现型 subagent 可以在隔离空间里工作。

后续还需要更完整的 merge-back、conflict review、approval policy。

### Stage 7: task tree API

HTTP Runtime 暴露 task tree payload，TUI/CLI attach 能查看 `/tasks`、`/task`。

这让 subagent 不只是 tool result，而是可观察的 session tree。

### Stage 8: skill preload

subagent profile 支持 `skills: [...]`。启动 child session 时加载 skill body 到 child system context，并写 metadata。

父上下文只看到 summary/metadata，不吃 child skill content。

### Stage 9: fork skill to Task

支持 skill metadata 指定 fork agent。加载这类 skill 时创建 child task/subagent。

这把 Skill 的专业知识和 Task 的独立执行连接起来。

### Stage 10: foreground/background foundations

HTTP 侧已有 background queue 基础，foreground 路径更完整。CLI background lifecycle 尚未完全完成。

## 7. 当前限制

主要缺口是 background lifecycle 还不完整。

目标状态机：

```text
queued
  -> running
  -> completed
  -> failed
  -> cancelled
```

还需要稳定操作：

- `wait`；
- `promote`；
- `cancel`；
- `resume`；
- `inspect`；
- list task tree；
- foreground/background 切换语义。

目前 HTTP queue foundation 有了，CLI foreground 更完整，但 CLI background 还不是 OpenCode/Claude Code 水平。

## 8. SessionRunner 方向

Subagent 最终不应由 CLI 或 HTTP 自己跑，而应进入 SessionRunner：

```text
SessionRunner::run_task
  -> validate parent session
  -> resolve child profile
  -> prepare child ToolContext
  -> bind preloaded skills
  -> claim task run lock
  -> execute provider/tool loop
  -> update task lifecycle event
  -> summarize child result
```

这样 TUI/Desktop 可以消费相同 task events，而不是定制轮询。

## 9. 验收证据

代表性命令：

```bash
cargo test -p openagent-cli binary_run_executes_task_subagent_tool --test cli_commands -q
cargo test -p openagent-cli binary_run_auto_routes_prompt_to_matching_subagent_description --test cli_commands -q
cargo test -p openagent-cli binary_run_executes_subagent_in_isolated_workspace --test cli_commands -q
cargo test -p openagent-http-runtime --test http_runtime -q
```

覆盖点：

- explicit Task tool；
- auto routing；
- nested guard；
- workspace isolation；
- child session metadata；
- OpenCode markdown agent loading；
- subagent preloaded skills；
- HTTP task tree。

## 10. 后续边界

1. 完成 background lifecycle 状态机。
2. CLI/HTTP 都支持 wait/promote/cancel/resume。
3. Task events 统一成可重放 event family。
4. TUI/Desktop 增加 subagent pane 和 task tree navigation。
5. Worktree isolation 增强 merge-back 和冲突审查。
6. SessionRunner 接管 task execution。

## 11. Subagent 的一等对象边界

判断 subagent 是否真正一等，不能只看有没有 `--agent xxx` 或 Task tool。需要看它是否拥有独立 runtime 边界。

| 边界 | 要求 |
| --- | --- |
| Context | child 有自己的 system prompt、messages、loaded skills、context budget |
| Tooling | child 有自己的 tool access 和 MCP/skill/task 可见性 |
| Permission | child 的 permission 可以不同于 parent |
| Model | child 可以覆盖 model/provider |
| Session | child 有独立 session id、events、metadata |
| Lineage | parent/root/child 关系可追踪 |
| Result | parent 只接收 summary/result/metadata |
| Lifecycle | foreground/background/wait/cancel/resume 可表达 |
| Isolation | workspace/worktree 可选隔离 |

如果缺少 session 和 lineage，subagent 就只是一次函数调用；如果 child tool calls 全部塞回 parent context，主上下文会被污染；如果没有 lifecycle，background task 就只是隐藏进程。

## 12. Task tool 为什么是主入口

对标 Claude Code 的 AgentTool 和 OpenCode 的 Task 工具，OpenHarness 选择 Task tool 作为 subagent 主入口，有几个原因：

1. 模型能显式决定何时委派，而不是完全依赖 harness 内部 router。
2. Task tool 可以走普通 tool registry、permission、ToolContext、event。
3. subagent 创建行为可以被 session 记录和测试。
4. 后续 foreground/background、wait/cancel/promote 可以复用同一 task id。
5. fork skill、auto routing、用户显式 `agent run` 都能收敛到同一个 runtime。

内部 auto routing 仍然有价值，但应该是调用 Task 的便利层，而不是另一套 subagent runtime。

## 13. 开发过程细化

### Step 1: Agent registry

先把 built-in agents、project agents、OpenCode markdown agents 放进统一 registry。这个阶段只解决“有哪些 agent 可选”，不急着跑 child session。

验收重点：

- list/show 能看到 agent；
- hidden/disabled 生效；
- markdown frontmatter 能解析；
- mode 区分 primary/subagent。

### Step 2: Task descriptor 暴露

当 registry 中存在可用 subagent，tool registry 注册 Task tool，并把 subagent description 暴露给模型。

这个阶段要避免把全部 profile 内容塞进 prompt，只暴露足够路由的信息。

### Step 3: Foreground child session

先实现 foreground，因为它最容易验证：

```text
parent tool call
  -> create child session
  -> run child agent loop
  -> summarize
  -> parent tool result
```

这个阶段必须确认 child messages/events 不直接进入 parent transcript。

### Step 4: Lineage 和 recursion guard

有了 child session 后马上补 lineage：

- parent session id；
- root session id；
- depth；
- subagent type；
- origin tool call id；
- self-call guard。

这一步是安全边界，不能等 background 以后再补。

### Step 5: Skill preload

profile 中的 `skills` 应在 child context 创建时预加载。加载结果写 child session metadata/system context，parent 只看到 metadata。

这一步验证 Skill 和 Subagent 两条链是否真正解耦。

### Step 6: Workspace/worktree isolation

实现型 subagent 需要隔离时，Task runtime 负责准备 workspace。初期可以先记录 isolation metadata 和基础路径，后续再完善 merge-back、冲突处理和审批。

### Step 7: Task tree API

child session 一旦存在，HTTP/App Bridge 就需要能投影 task tree。TUI/Desktop 后续不应从 parent transcript 猜 task 状态。

### Step 8: Background lifecycle

最后进入 queued/running/completed/failed/cancelled 状态机。原因是 background 需要更强的锁、队列、取消、恢复和 UI 投影，不能在 child session 未稳时贸然实现。

## 14. Parent/Child 上下文规则

Subagent 的上下文隔离规则应写清楚：

1. Parent prompt 可以产生 Task tool call。
2. Child prompt 接收任务描述、profile prompt、预加载 skill、必要 workspace metadata。
3. Child 中间 tool calls 只进入 child session。
4. Child final answer 被压成 summary/result 返回 parent。
5. Parent session 记录 Task tool result、child session id、status、metadata。
6. Parent compact 不应吞 child transcript；child session 自己负责 compact。

这套规则是对标 Claude Code subagent 的核心：child 不是 parent context 的展开，而是独立 context 的工作结果。

## 15. Foreground 与 Background 语义

Foreground task：

- parent 等 child 完成；
- parent turn 结束前拿到 summary；
- 适合短任务、review、单点查询。

Background task：

- parent 立即拿到 task id；
- child 在队列中运行；
- 用户可以 wait/inspect/cancel/promote；
- 适合长时间测试、批量扫描、并行实现。

这两个模式不应是两套实现。它们应该共享 child session 创建、profile binding、permission、event，只在 scheduling 和 parent continuation 上不同。

## 16. 对标差距

| 能力 | Claude Code/OpenCode 参考 | OpenHarness 状态 |
| --- | --- | --- |
| Agent/Task tool | Claude AgentTool / OpenCode Task | 已有主路径 |
| Independent context | Claude subagent context | 已有 child session 基础 |
| Optional model/tools/skills | Claude subagent fields | 已有 profile/schema 基础 |
| Nested subagent | Claude nested capability | 有 guard，深度语义待增强 |
| Background/resume | Claude foreground/background/resume | 部分 |
| Worktree isolation | Claude/OpenCode isolation | 部分 |
| Task tree UI | OpenCode/TUI task view | HTTP 基础，TUI/Desktop 待补 |
| Auto routing by description | Claude description routing | 已有基础 |

当前最关键的差距不是能否创建 child，而是 lifecycle 是否完整、UI 是否能观察、runner 是否统一。

## 17. 验收口径

Subagent/Task 改动至少要覆盖：

- profile/agent discovery；
- Task tool descriptor；
- foreground child session；
- parent/child metadata；
- nested guard；
- skill preload；
- workspace isolation；
- task tree API；
- background queue 或明确不在本阶段；
- parent context 不接收 child 中间工具调用。

只有这些都能被测试或 session evidence 证明，才算对标到一等 subagent，而不是实现了一个“调用另一个 prompt”的快捷方式。
