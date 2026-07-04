# Part 07 - MCP Capability Plane

## 1. 需求背景

MCP 不是“多一种工具调用”。它引入的是外部 capability server，因此完整需求包含：

```text
config
  -> auth
  -> discovery
  -> lifecycle
  -> tool execution
  -> diagnostics
  -> product visibility
```

如果只实现 provider loop 调 MCP tool，用户仍然无法知道：

- server 怎么配置；
- token 是否有效；
- 当前是否 running；
- 有哪些 tools；
- 失败是启动失败、认证失败、schema 失败还是工具执行失败；
- Desktop/TUI 是否看到同一状态。

因此 MCP 必须作为 capability plane，而不是普通 tool branch。

## 2. 对标参考

### 2.1 OpenCode MCP

OpenCode 对 MCP 的参考点包括：

- CLI `mcp` config/auth/debug；
- remote OAuth；
- dynamic client registration；
- TUI/App 中的 MCP 状态；
- MCP tool 进入普通 provider/tool execution path。

OpenHarness 已经对齐 local/remote config、auth token、doctor/debug、App Bridge lifecycle、provider-loop execution。缺口主要在 OAuth 和 dynamic registration。

### 2.2 Claude Code MCP instructions delta

Claude Code 还有一个重要 context 思路：MCP server 可能 late connect、disconnect、reconnect。如果每次把 MCP instructions 重新拼进 system prompt，会破坏 prompt cache。它通过 MCP instructions delta 作为 attachment 处理变化。

OpenHarness 当前还没有完整 delta attachment，但这个方向对后续 ContextPackBuilder 很重要。

## 3. 当前 OpenHarness 架构

```text
CLI / TUI / Desktop
  -> MCP config/auth commands or App Bridge routes
  -> HTTP Runtime MCP lifecycle registry
  -> discovery
  -> tool registry materialization
  -> provider/tool loop execution
  -> session events and UI projection
```

设计重点：lifecycle state 由 App Bridge/HTTP Runtime 统一管理，产品入口不各自维护 MCP 进程状态。

## 4. 分阶段增强过程

### Stage 1: Config

实现本地和远端 MCP 配置：

- server record；
- transport；
- command/args 或 URL；
- headers；
- timeout；
- enabled/disabled；
- secret redaction。

验收重点：

- CLI 能 list/show/add；
- JSON 输出不泄漏 secrets；
- HTTP 能读取相同 config。

### Stage 2: Auth

实现：

- token storage；
- status；
- logout；
- redacted display；
- config-aware doctor。

目前缺口：

- browser OAuth；
- dynamic client registration；
- provider-like account flow。

### Stage 3: Discovery

Discovery 把 server tools 转成 runtime descriptors。

要求：

- discovery failure 可诊断；
- tool schema 可注册；
- disabled server 不暴露 tools；
- UI 能显示 tool count。

### Stage 4: Lifecycle

App Bridge 管理生命周期：

- start；
- stop；
- restart；
- enable；
- disable；
- test。

Desktop MCP panel 和 smoke test 覆盖 Add/Edit/Delete/Test/Start/Stop/Restart/Enable/Disable，证明 UI 操作的是同一 runtime state。

### Stage 5: Tool execution

MCP tools 进入 provider/tool loop：

```text
provider tool call
  -> tool registry lookup
  -> MCP bridge execution
  -> ToolResult
  -> session event
  -> continuation
```

它应该和 built-in tools 一样接受 permission、trace、event、result normalization。

### Stage 6: Product visibility

MCP 状态进入：

- CLI `mcp`；
- HTTP/App Bridge；
- TUI controls；
- Desktop MCP panel；
- smoke tests。

Desktop 的 MCP panel 暴露了很多 CLI 不会暴露的问题：reload 后状态是否恢复、PID 是否复用、错误 tool trace 是否能显示、按钮是否在真实 packaged app 可用。

## 5. 当前能力

已支持：

- local/remote config；
- auth token storage/status/logout；
- secret redaction；
- debug/doctor；
- App Bridge lifecycle；
- provider-loop MCP tool execution；
- Desktop MCP panel；
- TUI remote MCP controls；
- MCP UI smoke 和 packaged smoke。

仍未完全对齐：

- browser OAuth；
- dynamic client registration；
- 跨重启 lifecycle recovery；
- MCP instructions delta；
- plugin-provided MCP registration。

## 6. 架构原则

1. MCP config/auth/lifecycle 是 runtime concern，不是 UI concern。
2. MCP tools 一旦 materialize，就走普通 tool path。
3. MCP secrets 任何输出都要 redacted。
4. Discovery 和 execution 失败要分层记录，便于 doctor。
5. UI 只消费 App Bridge MCP state。
6. MCP instructions 未来应进入 ContextPackBuilder/delta，而不是每轮粗暴重写 system prompt。

## 7. 验收证据

代表性命令：

```bash
cargo test -p openagent-cli --test cli_commands -q
cargo test -p openagent-http-runtime --test http_runtime -q
npm --prefix desktop run smoke:local-mcp-ui
```

覆盖点：

- CLI MCP config/auth/debug；
- HTTP MCP lifecycle；
- provider-loop MCP tool execution；
- Desktop panel Add/Edit/Delete/Test/Start/Stop/Restart/Enable/Disable；
- reload 后 MCP state；
- success/failure MCP tool trace；
- packaged app MCP smoke。

## 8. 后续边界

1. 实现 browser OAuth。
2. 实现 dynamic client registration。
3. MCP lifecycle state 跨 runtime 重启恢复。
4. MCP instructions delta 进入 context pipeline。
5. plugin-provided MCP servers 注册。
6. TUI/Desktop MCP picker 和错误恢复继续产品化。

## 9. MCP 为什么是 capability plane

MCP 和 built-in tool 的差异在于，built-in tool 的生命周期由 harness 自己掌握，而 MCP server 是外部 capability provider。它会引入配置、认证、进程、网络、schema、版本、失败恢复和 UI 可见性。

因此 MCP 的完整链路是：

```text
configuration
  -> secret/auth
  -> lifecycle
  -> discovery
  -> tool materialization
  -> permission
  -> execution
  -> diagnostics
  -> product projection
```

只做最后两步，会导致用户无法运营 MCP；只做 config/list，不接 provider loop，则模型无法使用 MCP。

## 10. Runtime 对象

| 对象 | 责任 |
| --- | --- |
| MCP server config | transport、command/url、headers、enabled、timeout |
| Auth record | token、method、redacted status |
| Lifecycle state | stopped/starting/running/failed/disabled |
| Discovery result | tools、schemas、capabilities、error |
| Materialized tool | provider-visible tool descriptor |
| Execution bridge | call MCP server，normalize result |
| Diagnostic report | doctor/debug/test 输出 |
| Product projection | CLI/HTTP/TUI/Desktop 状态 |

这些对象应该由 runtime 管理，UI 只调用 API。

## 11. 开发过程细化

### Step 1: Config store

先定义 server record 和 redaction 策略。验收重点是 CLI/HTTP 读到同一配置，secret 不泄漏。

### Step 2: Auth status

增加 token storage、status、logout。这里要把 secret display 和实际 execution credential 分开。

### Step 3: Discovery

连接 server，读取 tools/schema。Discovery 的失败要分类：

- command not found；
- process start failed；
- auth failed；
- network timeout；
- invalid schema；
- disabled server。

### Step 4: Lifecycle API

HTTP/App Bridge 提供 start/stop/restart/enable/disable/test。TUI/Desktop 不直接管理进程，只消费 lifecycle state。

### Step 5: Provider loop execution

把 discovery 出来的 MCP tool 注册到 tool registry，执行时走 ToolContext、permission、ToolResult、session event。

### Step 6: Product smoke

MCP 最容易出现“单测过、产品不可用”的问题，所以 Desktop smoke 很重要。Add/Edit/Delete/Test/Start/Stop/Restart/Enable/Disable 都是 lifecycle contract 的验证。

### Step 7: OAuth 和 dynamic registration

远端 MCP 完整对齐 OpenCode 时，需要 browser OAuth、dynamic client registration 和 account/session 级 auth flow。这是后续 P0 gap。

### Step 8: Context delta

MCP server instructions 变化不应每轮粗暴重写 system prompt。后续应进入 ContextPackBuilder 的 delta/attachment 模型。

## 12. 权限和安全边界

MCP 安全边界至少包括：

- server enabled/disabled；
- tool-level permission；
- secret redaction；
- command/args display；
- remote URL allow policy；
- timeout；
- tool schema validation；
- result size/budget；
- failure classification。

MCP 工具一旦 materialize，就不能绕过普通 tool permission。否则 MCP 会成为 capability escape hatch。

## 13. 对标差距

| 能力 | OpenCode 参考 | OpenHarness 状态 |
| --- | --- | --- |
| Local MCP config | CLI/App config | 已有 |
| Remote MCP config | URL/header/token | 已有基础 |
| Auth status/logout | provider-like auth | 已有基础 |
| Lifecycle controls | start/stop/restart/test | 已有 |
| Provider-loop execution | MCP tool path | 已有 |
| Desktop panel | App product visibility | 已有 |
| Browser OAuth | remote auth flow | 未完成 |
| Dynamic registration | OAuth client registration | 未完成 |
| Instructions delta | context lifecycle | 未完成 |
| Plugin-provided MCP | plugin contribution | 未完成 |

MCP 当前主路径可用，差距集中在远端认证、动态注册、跨重启恢复和 context delta。

## 14. 验收口径

MCP 每个阶段至少要覆盖：

- config list/show/add/update/delete；
- secret redaction；
- auth status/logout；
- discovery success/failure；
- lifecycle start/stop/restart；
- disabled server 不暴露 tools；
- provider loop 能调用 MCP tool；
- session event/trace 能看到结果；
- TUI/Desktop reload 后状态一致；
- packaged smoke 不依赖开发环境假设。

这套验收能防止 MCP 退化成“开发机上能跑的一次性工具调用”。
