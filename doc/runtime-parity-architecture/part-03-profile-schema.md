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

## 9. Schema 需求演化

Profile 的演化可以分成四个阶段。

### 9.1 参数阶段

最早的 profile 更像 CLI 参数集合：

- model；
- provider；
- prompt；
- tools；
- temperature/top_p。

这个阶段的重点是让 `run --agent` 能选择不同配置。

### 9.2 Agent definition 阶段

当 subagent 出现后，profile 不再只是主 agent 的参数，而是 agent definition：

- `mode: primary | subagent`；
- description 用于 auto routing；
- tools/permission 限定 agent 能力；
- model/provider 可独立覆盖；
- workspace isolation 决定执行边界。

这一步使 profile 成为 subagent 的运行时说明书。

### 9.3 Capability binding 阶段

Skill、Task、MCP 进入后，profile 需要描述能力绑定：

- 允许哪些 skill；
- skill roots 来自哪里；
- task/subagent 是否 allow/deny/ask；
- tool allowlist 和 MCP tool 是否可见；
- hidden/disabled 是否影响 discovery。

这一步把 profile 从“模型配置”升级成“capability routing 配置”。

### 9.4 Runner input 阶段

下一阶段 profile 会成为 SessionRunner 的标准输入。Runner 不应该再从 CLI flags 或 HTTP JSON 里东拼西凑，而应该拿到一个已 normalize 的 profile，然后完成：

- system prompt binding；
- available skills injection；
- tool materialization；
- permission gate；
- provider request lowering；
- session metadata 写入。

## 10. 字段分层

Profile 字段可以按归属分层。

| 层级 | 字段例子 | 消费者 |
| --- | --- | --- |
| Identity | id、name、description、mode | agent registry、Task tool、UI |
| Prompt | prompt/body、instructions | ContextPackBuilder、provider messages |
| Model | provider、model、temperature、top_p | provider resolver |
| Tooling | tools、MCP enablement | tool registry |
| Permission | permission、skill_permissions、task_permissions | permission engine |
| Skill | skills、skill_roots、frontmatter policy | skill registry/tool |
| Task | task config、workspace isolation、max depth | Task runtime |
| Presentation | color、hidden、disabled | CLI/TUI/Desktop |
| Escape hatch | model_options | provider-specific lowering after filtering |

这个分层的意义是避免字段被错误消费者读取。比如 `color` 不应影响 provider request，`skill_roots` 不应出现在 model options，`hidden` 不应改变已指定 profile 的执行权限。

## 11. 开发过程细化

这一链路实际开发时应按下面顺序推进。

### Step 1: 建共享类型

先在 shared crate 建立结构体和 serde/parser 单测，不改 CLI/HTTP 执行路径。验收重点是 JSON/Markdown 输入能解析成同一 internal shape。

### Step 2: 迁移权限解析

把 `permission.skill`、`skill_permissions`、`permission.task`、`task_permissions` 的兼容写法收进 helper。这个阶段要特别防止 allow/deny/ask 默认值漂移。

### Step 3: 剥离 runtime-only options

建立 known runtime keys 表，把 runtime 字段从 `model_options` 中移除。验收必须看 fake provider request，而不是只看 parser 输出。

### Step 4: surface adapter 变薄

CLI/HTTP 只负责 discovery 和 IO，不再自己解释字段。adapter 只做：

```text
source file/request
  -> raw value/frontmatter
  -> AgentProfileSchema
  -> surface runtime profile
```

### Step 5: public value 对齐

API/TUI/Desktop 需要看到 profile 的真实 runtime 配置，所以 public value 不能只返回 model/provider。SkillConfig、TaskConfig、permissions、hidden/disabled 都要投影出来，但 secrets 和本地敏感路径要按策略处理。

### Step 6: runner 接入

最后由 SessionRunner 消费 normalize 后的 profile。此时 CLI/HTTP 不应再重复构造 ToolContext、system prompt、available skills。

## 12. 设计取舍

### 为什么放在 `openagent-tools`

严格看，profile schema 也可以放到 `protocol`。当前放在 `openagent-tools` 的原因是它和 ToolContext、skill/task permission、tool registry 更近，能减少依赖反转。等 SessionRunner crate 成型后，可以重新评估是否拆出 `runtime-config`。

### 为什么 discovery 不完全共享

不同 surface 的 profile 来源不完全一样。CLI 有本地文件和 flags，HTTP 有 request override 和 workspace root，未来 plugin 也会贡献 agents。先共享解析，不强行共享 discovery，可以降低改动风险。

### 为什么兼容 OpenCode markdown

OpenCode agent markdown 已经是一种实用生态格式。OpenHarness 兼容它，是为了迁移成本和用户心智，而不是要复制所有字段。关键是 frontmatter 最终进入 shared schema，不让 markdown 特性穿透到执行 loop。

## 13. 缺口和验收追踪

后续 profile 工作可以按这个表追踪：

| 能力 | 当前判断 | 需要的验收 |
| --- | --- | --- |
| JSON/Markdown 等效解析 | 已落地 | shared parser tests |
| runtime-only provider filtering | 已落地 | fake provider payload |
| SkillConfig 一等化 | 已落地 | CLI/HTTP profile tests |
| TaskConfig 一等化 | 已落地 | Task tool/profile tests |
| built-in profile registry 共享 | 部分 | CLI/HTTP list/show 对齐 |
| plugin-provided agents | 未完成 | plugin manifest -> registry tests |
| SessionRunner profile binding | 部分 | runner-level integration |

Profile 链路的完成标准不是字段多，而是一个 profile 在 CLI、HTTP、TUI/Desktop 间具有同一个 runtime 含义。
