# OpenHarness Runtime Parity Architecture

这组文档记录的是 OpenHarness 在 harness 层面的需求演化和架构收敛过程，不是某一次 session 的改动清单。

写法上按需求域拆分，每个 part 单独成文。每篇都回答几个问题：

- 这个需求为什么从“功能点”升级成了 harness 运行时能力；
- OpenCode 和 Claude Code 在同类问题上的架构思路是什么；
- OpenHarness 采用了哪些设计，哪些地方没有照搬；
- 功能是按什么阶段落地的；
- 当前验收证据在哪里，后续工程边界是什么。

## 文档定位

OpenHarness 早期更像一个 tool-using coding agent：接收 prompt，调用 provider，执行 workspace tool，然后返回结果。随着 CLI、HTTP Runtime、App Bridge、TUI、Desktop、MCP、Skill、Subagent、Provider Catalog、Session Store 逐步补齐，系统已经从“一个命令行 agent”变成了“本地 agent runtime”。

因此这套文档的主线不是按单个提交或单次会话罗列文件，而是：

```text
coding-agent CLI
  -> session-aware runtime
  -> capability-oriented harness
  -> product-surface shared runtime
  -> skill/task/subagent first-class routing
  -> observable, resumable, extensible local agent OS
```

## 参考架构

### OpenCode

OpenCode 的价值不只在命令多，而在它把 CLI、TUI、App、Session、Provider、MCP、Skill、Plugin、Permission、Runner 放进同一个 runtime contract 里。几个关键参考点：

- SessionRunner 负责把 session id 路由到 location-scoped runner；
- provider/model catalog 是一等运行时资源，不只是环境变量；
- skill 通过 available list 暴露，再由 `skill` tool 按 name 加载完整内容；
- permission 按 agent、tool、resource 计算，不是散落在命令入口；
- TUI/App 读取 session/event 状态，而不是自己维护另一套运行时事实；
- plugin/provider/skill/catalog 通过可重放 transform 或 registry 进入 runtime。

### Claude Code

Claude Code 的关键参考点是上下文生命周期和 subagent/skill 的一等对象化：

- AgentTool 是普通工具，但会创建独立 subagent runtime；
- subagent 有独立 context window、system prompt、tool access、permission、model、skills；
- 主上下文只接收 child result/summary，不吞掉所有 child tool calls；
- SkillTool 不只是加载 markdown，还支持 forked skill agent、invoked skill 记录和 compact 后恢复；
- context 不是拼 messages，而是 system prompt、attachments、MCP delta、skill listing、agent listing、compaction、resume 的生命周期系统。

OpenHarness 不是复制两者的产品形态，而是吸收这些 runtime 思想，并按本地 harness 的约束落地：文件证据、可重复测试、显式 session state、App Bridge API、Rust crate 边界、local-first 操作。

## 阅读顺序

| Part | 文档 | 主题 |
| --- | --- | --- |
| 01 | [Demand Evolution](part-01-demand-evolution.md) | harness 层面的需求演化总览 |
| 02 | [Runtime Architecture](part-02-runtime-architecture.md) | 当前分层架构和 crate ownership |
| 03 | [Profile Schema](part-03-profile-schema.md) | AgentProfile、SkillConfig、TaskConfig 一等化 |
| 04 | [Skill System](part-04-skill-system.md) | Skill 从 markdown 文件升级为 runtime capability |
| 05 | [Subagent And Task Runtime](part-05-subagent-task-runtime.md) | Task tool、subagent session、隔离和生命周期 |
| 06 | [Session, Context, And Events](part-06-session-context-events.md) | session ledger、context、event、compaction、checkpoint |
| 07 | [MCP Capability Plane](part-07-mcp-capability-plane.md) | MCP config/auth/discovery/lifecycle/execution/UI |
| 08 | [Provider And Model Runtime](part-08-provider-model-runtime.md) | provider auth、catalog、native routing、payload boundary |
| 09 | [Product Surfaces](part-09-product-surfaces.md) | CLI、HTTP、App Bridge、TUI、Desktop |
| 10 | [Extension And Operations](part-10-extension-operations.md) | plugin、GitHub、debug/db、eval、生命周期和运维 |
| 11 | [Execution Planner Roadmap](part-11-execution-planner-roadmap.md) | SessionRunner、task lifecycle、event model、ToolBatchPlanner |

## 写作约定

- 文档以 OpenHarness 当前 Rust runtime 为主，不再描述旧 Python 树作为目标架构。
- “对标”指架构能力和运行时边界对齐，不代表 API 或 UI 完全复制。
- 已完成能力会写明验收面；未完成能力会写在“后续边界”，避免混在已完成叙述里。
- 需求演化按 harness 层面分类，不按单次 session 或单个 commit 分类。
