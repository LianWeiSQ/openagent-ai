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
