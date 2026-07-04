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
