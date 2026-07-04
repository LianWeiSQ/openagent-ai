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
