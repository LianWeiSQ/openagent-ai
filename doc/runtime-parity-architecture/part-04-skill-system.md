# Part 04 - Skill System

## 1. 需求背景

Skill 最初容易被理解成“放一份 markdown 指令，必要时读出来”。这个理解太窄。对一个长期运行的 coding agent 来说，skill 至少要解决五个问题：

- 怎么让模型知道有哪些专业能力可用；
- 怎么避免把所有 skill 全量塞进 system prompt；
- 怎么按 profile、agent、permission 控制可见和可加载范围；
- 怎么加载 skill body 以及相关资源文件；
- 怎么在 compaction、subagent、forked execution 后保持语义稳定。

因此 Skill 需要升级为 runtime capability，而不是 markdown 文件。

## 2. 对标参考

### 2.1 OpenCode skill tool

OpenCode 的 Skill V2 思路很清晰：

```text
available skills in system context
  -> model calls skill({ name })
  -> tool returns <skill_content>
  -> output includes base directory and sampled files
```

重点有三个：

- 模型先看到 name/description/location，不直接吃 full content；
- 加载通过普通 `skill` tool 进入 tool path；
- 输出告诉模型 base directory，skill 中的相对路径有明确基准。

OpenHarness 的 Skill tool V2 基本沿用这个主路径。

### 2.2 Claude Code SkillTool

Claude Code 的 SkillTool 更进一步：skill 可以记录 invoked state，可以 fork 到独立 agent，可以在 compact 后恢复已调用 skill。它把 skill 放进上下文生命周期，而不是只当工具输出。

对 OpenHarness 的启发：

- skill listing 和 skill body 是两个阶段；
- loaded skill 要写 session event；
- compact 不能丢失已加载 skill；
- forked skill 应该走 child context，而不是污染 parent；
- skill frontmatter 可以携带路径匹配、工具限制、参数等语义。

## 3. OpenHarness 当前架构

当前 Skill 路径如下：

```text
AgentProfile
  -> SkillConfig
  -> ToolContext
  -> SkillRegistry
  -> available skills system prompt
  -> skill tool V2 load by name
  -> session events
  -> compaction protection
  -> optional fork to Task/subagent
```

这个路径把配置、发现、权限、加载、事件、compaction、subagent 连接成一条链。

## 4. 分阶段增强过程

### Stage 1: SkillConfig 一等化

需求：

- CLI/HTTP profile 都能声明 `skills`、`skill_roots`、`skill_permissions`；
- 这些字段不再混在 `model_options`；
- provider payload 不泄漏 skill 配置。

落地：

- shared AgentProfile schema 增加 SkillConfig；
- CLI/HTTP profile adapter 使用同一解析规则；
- runtime-only known keys 从 provider options 剥离。

验收：

- JSON/Markdown profile 解析一致；
- public profile value 展示 skill 配置；
- fake provider request 不包含 `skill_roots`、`skill_permissions`。

### Stage 2: ToolContext 打通

需求：

- skill tool 执行时能知道 active agent、skill roots、preloaded skills、permission；
- CLI fake provider 和 HTTP fake provider 都能读取 profile 配置的 skill roots。

落地：

- ToolContext 增加 skill roots、active skills、agent id；
- CLI agent loop 和 HTTP runtime 创建 ToolContext 时注入这些字段；
- 后续抽入 `SessionRunnerFacade::tool_context`。

价值：

- skill tool 不需要猜 profile；
- subagent 和主 agent 可以有不同 skill scope；
- 未来 SessionRunner 可以直接复用。

### Stage 3: Built-in skill root

需求：

- `/skill/openagent` 作为 built-in root；
- 默认能列出内置 skill；
- workspace/user skills 可以覆盖同名 built-in skill；
- built-in root 可通过配置开关控制。

设计：

```text
workspace skills
  > explicit profile roots
  > user skills
  > built-in skills
```

这个优先级保证项目可以覆盖内置行为，同时保留默认可用能力。

### Stage 4: available skills system prompt

需求：

- 当 agent 允许 `skill` tool 时，在 system prompt 注入 `<available_skills>`；
- 只放 name、description、location；
- 禁用 skill tool 时不注入。

对标 OpenCode：

```text
available list is guidance
full skill body is loaded by tool
```

这样既减少 prompt 体积，也让 skill 加载成为可审计行为。

### Stage 5: Skill permission

需求：

- 支持 `permission.skill` 或 `skill_permissions`；
- 支持 `skill:<name>` allow/deny/ask；
- deny 后不展示在 available skills；
- direct load 返回权限错误。

设计原则：

```text
discovery visibility and execution authorization share one policy boundary
```

否则会出现模型看得到但加载不了，或者看不到但猜名字能加载的漏洞。

### Stage 6: Skill tool V2 输出

需求：

- list/search 保留为诊断；
- 主路径按 name 加载；
- 输出 `<skill_content>`；
- 带 base directory；
- 带 sampled skill_files；
- 不越权读取 workspace 外不可读文件。

输出形态：

```text
<skill_content name="...">
  # Skill: ...
  ...
  Base directory for this skill: ...
  <skill_files>
    <file>...</file>
  </skill_files>
</skill_content>
```

这解决了 skill body 和资源文件路径的相对基准问题。

### Stage 7: Subagent 预加载 skills

需求：

- profile 支持 `skills: ["x"]`；
- 启动 subagent 时自动加载这些 skill；
- skill body 进入 child session system messages；
- parent context 只看到 summary/metadata。

这是 Claude Code subagent 独立 context 思路在 OpenHarness 中的落地。

### Stage 8: Claude frontmatter 子集

先支持必要子集：

- `when_to_use`；
- `paths`；
- `allowed-tools`；
- `disallowed-tools`；
- `user-invocable`；
- `disable-model-invocation`；
- `arguments`。

这些字段的作用：

- `when_to_use`：帮助 available skills 说明适用场景；
- `paths`：路径匹配和条件激活；
- tool allow/deny：给 skill 增加工具边界；
- `user-invocable`：控制是否直接展示给用户/模型；
- `disable-model-invocation`：隐藏不应由模型主动调用的 skill；
- `arguments`：支持参数替换。

### Stage 9: Fork skill -> Task/subagent

需求：

- skill 可以声明 `context: fork + agent`；
- 加载这类 skill 时走 Task/subagent；
- 支持 foreground/background；
- 主会话只收到摘要，不接收中间工具调用。

设计：

```text
skill metadata
  -> route to Task tool
  -> create child session
  -> run specialized agent
  -> return summary and metadata
```

这一步把 skill 和 subagent 真正接上了。

### Stage 10: Observability and API

需求：

- CLI `skills list/show/doctor`；
- HTTP `/api/skills`；
- session event 记录 `skill.discovered` / `skill.loaded`；
- compaction 保护 loaded skill output；
- golden/API/session tests 覆盖。

意义：

- skill 不再是黑箱；
- Desktop/TUI/API 可以显示真实 skill 状态；
- compact 后不会丢失已加载 skill 的指导语义。

## 5. 当前能力总结

OpenHarness Skill 当前已经具备：

- profile-level SkillConfig；
- ToolContext skill scope；
- built-in root；
- workspace override；
- available skills prompt；
- permission-filtered discovery；
- name-based Skill tool V2；
- base dir + sampled files；
- subagent skill preload；
- Claude frontmatter 子集；
- fork skill to Task；
- CLI/HTTP API；
- session observability；
- compaction protection。

这条链路已经从“文件加载”升级为“runtime capability”。

## 6. 验收证据

代表性命令：

```bash
cargo test -p openagent-tools -q
cargo test -p openagent-cli --test cli_commands -q
cargo test -p openagent-http-runtime --test http_runtime -q
cargo test -p openagent-session --test session_trace -q
```

覆盖点：

- profile skill config 解析；
- provider payload 不泄漏 skill config；
- CLI/HTTP skill root 注入；
- built-in skill discovery；
- workspace 同名覆盖 built-in；
- denied skill 隐藏和拒绝加载；
- Skill tool V2 输出；
- frontmatter path/tool/argument 行为；
- subagent preload；
- fork skill child session；
- `skill.discovered` / `skill.loaded`；
- compaction 后 loaded skill 不丢。

## 7. 后续边界

Skill 已经有完整骨架，后续重点是深度：

1. 更强的路径触发和动态 discovery。
2. Skill-aware TUI/Desktop 面板。
3. Skill install/update 工作流。
4. Fork skill 与 background task lifecycle 深度融合。
5. 更详细的“为什么选择这个 skill”可观测性。
6. plugin-provided skills 进入同一 registry。

## 8. Skill 设计主线

Skill 的核心设计不是“让模型多读几段提示词”，而是把专业能力做成可发现、可加载、可授权、可审计、可恢复的 runtime resource。

这条主线可以概括为：

```text
lightweight listing
  -> permission-filtered visibility
  -> explicit load by name
  -> scoped resource access
  -> session event
  -> compaction preservation
  -> optional forked execution
```

OpenCode 给出的关键参考是两段式加载：先在 system prompt 中暴露 available skills，再通过 `skill` tool 加载完整内容。Claude Code 给出的关键参考是 lifecycle：skill 被调用后要能进入上下文历史、compact 后恢复，必要时还能 fork 到独立 agent。

OpenHarness 的实现吸收了这两个方向：主路径对齐 OpenCode 的 Skill tool V2，生命周期对齐 Claude Code 的 invoked skill/compaction/fork 思路。

## 9. Runtime 对象

Skill 链路涉及的 runtime 对象如下。

| 对象 | 责任 |
| --- | --- |
| `SkillConfig` | profile 级配置，声明 roots、preloaded skills、permission |
| Skill root | skill 文件搜索根，包括 workspace、user、built-in、profile roots |
| Skill descriptor | name、description、location、frontmatter、visibility |
| Skill registry | discovery、override、path matching、permission filtering |
| Skill tool | list/search 诊断，load-by-name 主路径 |
| ToolContext | 当前 agent、workspace、skill roots、active skills、permission |
| Session event | `skill.discovered`、`skill.loaded` |
| Compaction guard | compact 后保留 loaded skill output |
| Task bridge | fork skill 转 Task/subagent |

这套对象让 skill 进入正常 runtime，而不是作为 prompt 文件旁路存在。

## 10. 开发过程细化

### Step 1: Profile 字段一等化

先让 CLI/HTTP profile 能稳定表达 skill 配置。这个阶段的重点不是读取 skill，而是确认配置边界：

- public profile 能看到 skill 配置；
- provider payload 不带 skill 配置；
- JSON/Markdown profile 行为一致。

### Step 2: ToolContext 注入

Skill tool 不能自己去猜 workspace 或 profile。它必须从 ToolContext 拿当前 agent 的 skill scope。这个阶段把 CLI loop 和 HTTP runtime 都改到同一个 context 输入。

### Step 3: Root 优先级

Skill root 的覆盖顺序很关键：

```text
workspace/project
  > explicit profile roots
  > user roots
  > built-in roots
```

这样项目能覆盖内置 skill，用户也能保留个人 skill。built-in root 是兜底，不是最高优先级。

### Step 4: available skills 注入

只有当 agent 允许 `skill` tool 时才注入 `<available_skills>`。注入内容只包括 name、description、location，避免 system prompt 被所有 skill body 撑爆。

### Step 5: 权限前置

deny 的 skill 不仅不能 load，也不应该出现在 available list 中。这样模型不会被诱导调用不可用能力，安全边界也更清楚。

### Step 6: Tool V2 输出

主路径改成 `skill({ name })`。输出必须包含：

- `<skill_content>`；
- base directory；
- sampled skill files；
- 安全过滤后的资源列表。

这一步解决了 skill 内相对路径和资源文件的解释问题。

### Step 7: Frontmatter 语义

Claude frontmatter 子集不是为了格式兼容，而是为了把 skill 的适用条件和工具边界结构化：

- `paths` 做路径匹配；
- `allowed-tools` / `disallowed-tools` 做 tool scope；
- `user-invocable` / `disable-model-invocation` 做可见性；
- `arguments` 做参数替换；
- `when_to_use` 做 discovery 说明。

### Step 8: Subagent preload 和 fork

预加载 skill 是 child session 的 system context，不进入 parent context。fork skill 更进一步：它不是把 skill body 返回给主模型，而是启动 child task 去执行专业流程。

这一步让 Skill 和 Subagent 形成组合能力。

### Step 9: 可观测和 compact

Skill 加载必须写 event，compact 必须保护 loaded output。否则 session 恢复后模型无法解释自己为什么采用某个工作方式。

## 11. 对标差距

| 能力 | OpenCode/Claude Code 参考 | OpenHarness 状态 |
| --- | --- | --- |
| available skills listing | OpenCode 两段式加载 | 已落地 |
| load by name | OpenCode Skill V2 | 已落地 |
| base dir/resource files | OpenCode V2 输出 | 已落地 |
| invoked skill restore | Claude Code compact restore | 部分落地，继续增强 |
| forked skill agent | Claude Code fork/context | 已有基础，依赖 Task lifecycle 深化 |
| skill install/update | 产品化 skill marketplace | 部分，仍需 workflow |
| UI skill panel | OpenCode/TUI capability view | 待补 |
| plugin-provided skills | OpenCode plugin registry | 待补 |

Skill 主链已经收口，后续差距主要在产品化、plugin 集成和 fork/background lifecycle。

## 12. 验收口径

Skill 相关改动必须同时过三类验收。

### 配置验收

- profile JSON/Markdown 能解析；
- public value 暴露 SkillConfig；
- provider payload 不泄漏 skill 字段。

### 执行验收

- available list 受 permission 过滤；
- load-by-name 返回完整 `<skill_content>`；
- base directory 和 sampled files 正确；
- deny 后 direct load 返回权限错误；
- path/frontmatter/argument 单测通过。

### 生命周期验收

- `skill.discovered` / `skill.loaded` event 存在；
- subagent preload 只进入 child context；
- fork skill 创建 child session；
- compact 后 loaded skill 不丢。

只满足其中一类，都不能算 Skill runtime 完整。
