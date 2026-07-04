# Part 03 - AgentProfile, SkillConfig, And TaskConfig Schema

## 1. 需求背景

Agent profile 最开始只是 CLI `run --agent` 的配置输入。随着功能增加，profile 开始承载越来越多 runtime 语义：

- agent id/name/description；
- primary/subagent mode；
- model/provider；
- tool allowlist；
- permission；
- task permissions；
- skill roots、preloaded skills、skill permissions；
- workspace isolation；
- max steps；
- markdown frontmatter；
- model options。

问题是 CLI 和 HTTP Runtime 都需要解释这些字段。如果两个入口各写一套 parser，就会产生几类风险：

- 同一个 profile 在 CLI 和 HTTP 下行为不同；
- runtime-only 字段进入 provider payload；
- skill/task permission 合并规则不一致；
- public API 返回值和实际执行值不一致；
- 后续 SessionRunner 很难建立统一入口。

因此 AgentProfile 需要从“命令参数解析结果”升级为 shared runtime schema。

## 2. 对标参考

### OpenCode

OpenCode 的 agent/config/provider/skill 是 runtime 层对象，不是 CLI 层字符串。它的测试覆盖 agent config、skill config、provider options、permission，并通过 core 层统一解释。

对 OpenHarness 的启发是：profile schema 应该在 CLI/HTTP 之下，成为工具、权限、provider、session runner 的共同输入。

### Claude Code

Claude Code 的 subagent definition 包含 system prompt、tools、permissions、model、skills 等字段。重点不是 frontmatter 格式，而是 agent definition 能决定一个独立 runtime 的行为。

对 OpenHarness 的启发是：agent profile 必须有能力描述 subagent，而不是只描述主会话模型。

## 3. 当前 schema

共享 schema 放在 `openagent-tools`，因为 CLI 和 HTTP 都依赖它，并且 tools crate 已经拥有 ToolContext、task/skill permission 类型。

核心结构：

```text
AgentProfileSchema
  - id
  - name
  - description
  - mode
  - model
  - provider
  - tools
  - prompt
  - permission
  - task
  - skill
  - max_steps
  - temperature
  - top_p
  - color
  - hidden
  - disabled
  - workspace_isolation
  - model_options

SkillConfig
  - skills
  - roots
  - permissions

TaskConfig
  - permissions
```

这个 schema 不是完整 runner，也不是完整 config system。它是进入 runner 之前的 profile normalization 边界。

## 4. 关键设计

### 4.1 Discovery 保留在 surface

CLI 和 HTTP 仍然可以各自决定从哪里找 profile：

- `.openagent/agents`；
- `.opencode/agents`；
- `.opencode/agent`；
- built-in profile；
- request/profile override。

共享 schema 只负责“把找到的 JSON/Markdown frontmatter 解释成统一结构”。这样改动范围小，风险可控。

### 4.2 Runtime-only 字段不进入 provider

profile 中有两类字段：

```text
model-facing:
  temperature / top_p / provider-specific body / headers

runtime-facing:
  skills / skill_roots / skill_permissions
  task_permissions
  permission
  tools
  workspace_isolation
  hidden / disabled
```

parser 会把 runtime-facing known keys 从 `model_options` 中剥离，避免传给 provider。

这是一个重要边界。否则 provider payload 会带上 `skill_roots` 或 `permission.skill` 这类本地 runtime 字段，既污染 API，也可能泄露本地目录结构或内部策略。

### 4.3 权限来源合并

为了兼容不同写法，skill/task permission 可以来自：

```yaml
permission:
  skill:
    review: allow
  task:
    planner: ask

skill_permissions:
  deploy: deny

task_permissions:
  reviewer: allow
```

schema 负责合并这些来源，CLI/HTTP 不再各自写规则。

### 4.4 Markdown agent 兼容

OpenCode 风格 markdown agent 仍然可用。调用方提取 frontmatter 和 body，交给 shared schema 解析。body 作为 prompt/system prompt 进入 profile。

这让 OpenHarness 能保留 `.opencode/agent/*.md` 生态，同时不让 markdown parser 逻辑散落在多个 runtime。

## 5. 开发过程

这一阶段按低风险顺序落地：

1. 在 `openagent-tools` 定义 `AgentProfileSchema`、`SkillConfig`、`TaskConfig`。
2. 把 task/skill permission rule 解析移入共享 helper。
3. 写工具层 parser 单测，覆盖 JSON/Markdown 等效解析。
4. 验证 `skills`、`skill_roots`、`skill_permissions` 能成为 profile 一等字段。
5. 验证 `task_permissions` 和 `permission.task` 合并。
6. 验证 runtime-only known keys 不留在 `model_options`。
7. CLI 的 `agent_profile_from_value` 改成薄 adapter。
8. HTTP 的 `runtime_agent_profile_from_value` 改成薄 adapter。
9. 删除重复 parser helper。
10. 跑 CLI/HTTP integration，确认已有 profile 行为不变。

这个顺序避免了“大重写”。先把纯解析和测试做稳，再换调用点。

## 6. 验收口径

验收不是“能解析字段”这么简单，而是三个层面：

| 验收项 | 目的 |
| --- | --- |
| profile JSON/Markdown 解析一致 | 同一 profile 在 CLI/HTTP 下等价 |
| public value 含 skill/task config | API/TUI/Desktop 能展示真实 runtime 配置 |
| provider payload 不泄漏 runtime config | provider boundary 干净 |

代表性命令：

```bash
cargo test -p openagent-tools -q
cargo test -p openagent-cli --test cli_commands -q
cargo test -p openagent-http-runtime --test http_runtime -q
cargo fmt --all -- --check
git diff --check
```

## 7. 当前收益

这个阶段带来的收益很实际：

- SkillConfig 不再藏在 `model_options`；
- TaskConfig 和 SkillConfig 可以进入 ToolContext；
- CLI/HTTP 解析差异减少；
- provider payload 边界更安全；
- subagent profile 可以携带 preloaded skills；
- SessionRunner 后续可以基于统一 profile 输入工作。

## 8. 后续边界

还需要继续推进的点：

1. built-in profile registry 进一步共享，减少 CLI/HTTP built-in 差异。
2. profile public value 与 internal schema 的转换 helper 固化。
3. provider catalog 与 profile model/provider 选择更紧密结合。
4. plugin-provided agents 进入同一 schema。
5. SessionRunner 接管 profile/system prompt/tool materialization。
