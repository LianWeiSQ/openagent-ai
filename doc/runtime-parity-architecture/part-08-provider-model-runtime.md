# Part 08 - Provider And Model Runtime

## 1. 需求背景

Provider 支持不能停留在一个 OpenAI-compatible `base_url + api_key + model`。真实 harness 要支持：

- 多 provider；
- provider-aware auth；
- provider-specific env defaults；
- native provider routing；
- model catalog；
- model capability；
- health diagnostics；
- redaction；
- profile/model option boundary；
- HTTP/CLI/TUI/Desktop 一致选择。

核心问题是边界控制：runtime config 不能泄漏进 provider payload，provider wire format 也不能污染 agent loop。

## 2. 对标参考

### 2.1 OpenCode provider/model catalog

OpenCode provider catalog 把 provider 和 model 做成 typed runtime resource：

- provider 有 id/name/enabled/env/endpoint/options；
- model 有 providerID、apiID、capabilities、variants、limits、cost、status；
- catalog 支持 provider/model get/all/available/default/small；
- plugin/config/auth 可以通过 transform 影响 catalog。

对 OpenHarness 的启发：

- provider/model 是可操作资源，不只是环境变量；
- auth、catalog、policy、plugin 都会影响可用模型；
- model selection 和 provider endpoint resolution 应该被封装。

### 2.2 Claude Code provider boundary

Claude Code 关注不同 model/tool/context 能力对 request assembly 的影响。对 OpenHarness 来说，当前最关键的是不要把 runtime-only fields 放进 provider request。

## 3. 当前 OpenHarness 能力

### Provider-aware auth

CLI 支持：

- login；
- list；
- methods；
- logout；
- auth-file；
- provider-specific env metadata；
- redaction。

### Model listing

支持：

- provider filter；
- refresh；
- offline/catalog mode；
- verbose capability metadata；
- TTL/cache；
- snapshot fallback。

### Native provider routing

Anthropic 等 native provider path 不再被迫套 OpenAI-compatible `/models` 假设。

### HTTP provider health

HTTP Runtime 暴露 provider/model health 和 diagnostics，供 App Bridge/TUI/Desktop 使用。

### Explicit model preservation

近期修复了 HTTP provider catalog fallback 与 explicit provider model 的关系：model list 可以做 catalog filtering，但执行时 profile/session 显式指定的 model 不能被 fallback 覆盖。

## 4. Provider payload boundary

Provider request 应该包含：

- messages；
- model；
- tools；
- provider options；
- model-facing 参数，如 temperature/top_p；
- provider-specific headers/body 中被允许的字段。

不应该包含：

- `skills`；
- `skill_roots`；
- `skill_permissions`；
- `task_permissions`；
- `permission`；
- `workspace_isolation`；
- `hidden` / `disabled`；
- internal task/skill metadata；
- local filesystem runtime state。

Shared AgentProfile schema 的一个核心验收点就是 provider payload 不泄漏 runtime fields。

## 5. 分阶段增强过程

### Stage 1: OpenAI-compatible path

早期先保证 basic provider call、streaming、tool call 能跑。

### Stage 2: Streaming normalization

把 provider streaming 事件归一到 runtime 可消费的 delta/tool/finish 形态。

### Stage 3: CLI auth/provider commands

补：

- `auth login/list/logout`；
- `providers list/methods`；
- auth-file routing；
- secret redaction。

### Stage 4: Model catalog

补：

- model list；
- provider filter；
- refresh/offline；
- verbose capability；
- cache/snapshot。

### Stage 5: Native provider routing

为非 OpenAI-compatible provider 加 native route，避免所有 provider 都强行走同一个 wire assumption。

### Stage 6: HTTP diagnostics

HTTP Runtime 暴露 provider health/model payload，给 product surfaces 使用。

### Stage 7: Shared profile boundary

把 SkillConfig/TaskConfig 等 runtime-only fields 从 model_options 剥离，防止 provider payload 污染。

### Stage 8: Execution model preservation

修复 catalog fallback 和 explicit execution model 的边界，保证 provider call 使用用户/profile/session 明确指定的 model。

## 6. 当前差距

对标 OpenCode，仍不足：

- well-known provider URL login；
- 更完整 provider catalog；
- account-based provider enablement；
- plugin-provided providers；
- provider policy；
- model capability 在 UI 中更深使用；
- provider-specific failure classification。

这些属于 provider runtime 深度，不是 CLI flag 问题。

## 7. 验收证据

代表性命令：

```bash
cargo test -p openagent-cli --test cli_commands -q
cargo test -p openagent-http-runtime --test http_runtime -q
cargo check -p openagent-cli -p openagent-http-runtime
```

覆盖点：

- provider-specific env/model；
- auth-file provider routing；
- model catalog/cache/fallback；
- native provider diagnostics；
- profile runtime config 不泄漏；
- HTTP explicit model preservation。

## 8. 后续边界

1. Provider catalog service 化。
2. Account/provider login 设计对齐 OpenCode。
3. Plugin-provided provider/model 进入 catalog。
4. Provider policy 与 agent profile 合流。
5. TUI/Desktop model/provider 选择与 session state 更紧密绑定。
6. Provider error taxonomy 和 operator diagnostics 增强。
