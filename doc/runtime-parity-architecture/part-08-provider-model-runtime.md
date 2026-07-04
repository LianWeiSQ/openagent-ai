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

## 9. Provider runtime 的边界

Provider runtime 要解决两个方向的问题：

1. 向上给 agent loop 一个稳定接口；
2. 向下适配不同 provider 的认证、模型、工具 schema、streaming 和错误格式。

如果没有这个边界，agent loop 会充满 provider-specific 判断；如果边界太薄，provider catalog、auth、model capability 又无法进入产品面。

目标形态：

```text
AgentProfile
  -> provider/model resolver
  -> capability/catalog lookup
  -> request lowering
  -> native provider call
  -> normalized stream/events
  -> runner result
```

## 10. Model catalog 的设计含义

Model catalog 不是 `models list` 的缓存文件。它应该表达：

- 哪个 provider 提供模型；
- provider API id 和 display name；
- context/token/tool/image/streaming capability；
- default/small/fast/large 选择；
- enabled/disabled；
- auth/account 状态；
- cost/limit/health；
- cache TTL 和 offline snapshot。

OpenCode 的 provider/model catalog 强调“模型是 runtime resource”。OpenHarness 当前已有部分 catalog/list/cache/fallback 能力，但还没有完全 service 化。

## 11. Provider payload 降级链路

Profile 进入 provider 前需要经过降级：

```text
profile/runtime config
  -> remove runtime-only keys
  -> resolve model/provider
  -> select native wire format
  -> lower tool schemas
  -> attach provider options
  -> send request
```

这里最容易犯的错是把 runtime config 当作 provider option 透传。SkillConfig 一等化后，provider payload 不泄漏 `skill_roots` 等字段，就是这个边界的验证。

## 12. 开发过程细化

### Step 1: OpenAI-compatible baseline

先跑通最常见路径，保证 chat/tool call/streaming 可用。

### Step 2: Streaming normalization

不同 provider 的 stream delta 不同，runner 需要统一看到：

- text delta；
- tool call start/update/finish；
- finish reason；
- usage；
- error。

### Step 3: Auth commands

CLI 的 auth/provider 命令先承担本地运营入口，支持 provider-specific env、auth-file、redaction。

### Step 4: Model listing and cache

补 catalog、provider filter、refresh、offline mode、snapshot fallback。这个阶段要确保 listing fallback 不覆盖 execution explicit model。

### Step 5: Native provider routing

Anthropic 等 provider 不能长期套 OpenAI-compatible 假设。native route 是 provider runtime 成熟的标志。

### Step 6: HTTP diagnostics

把 provider/model health 暴露给 HTTP/App Bridge，让 TUI/Desktop 不需要自己探测 provider。

### Step 7: Profile boundary

通过 shared AgentProfile schema 保证 runtime-only fields 被剥离，provider-facing options 明确。

### Step 8: Catalog service 化

后续要把 provider catalog、auth status、policy、plugin contribution 合成一套 service，而不是 CLI/HTTP 各自拼。

## 13. Error taxonomy

Provider 错误需要分类，便于用户和 UI 判断：

- missing auth；
- invalid auth；
- model not found；
- provider unavailable；
- rate limit；
- context length exceeded；
- tool schema rejected；
- unsupported capability；
- streaming interrupted；
- provider payload validation failed。

目前错误处理能支持基本诊断，但还没有完整 taxonomy。对齐 OpenCode 后，provider health 和 model picker 应该能直接展示这些状态。

## 14. 对标差距

| 能力 | OpenCode 参考 | OpenHarness 状态 |
| --- | --- | --- |
| Provider auth/list/logout | CLI provider ops | 部分已落地 |
| Model catalog/cache | provider/model resources | 部分已落地 |
| Native provider routing | provider-specific lowering | 部分已落地 |
| Well-known login | account flow | 待补 |
| Plugin providers | plugin catalog transform | 待补 |
| Provider policy | config/policy layer | 待补 |
| Capability-aware UI | model picker/status | 部分 |
| Error taxonomy | operator diagnostics | 待补 |

Provider 这条链的后续重点是 service 化和 catalog/policy/plugin 合流。

## 15. 验收口径

Provider 改动至少要覆盖：

- CLI auth/provider/model 命令；
- HTTP provider/model diagnostics；
- explicit model 不被 catalog fallback 覆盖；
- runtime-only profile 字段不进 payload；
- native provider route 不走错误 endpoint；
- secrets redacted；
- offline/cache 行为可重复；
- provider errors 有可读分类。

Provider 是外部依赖最多的一层，测试上要同时覆盖 fake provider、snapshot/catalog 和至少一个 native path。
