# Part 09 - Product Surfaces

## 1. 需求背景

OpenHarness 现在不是一个 CLI-only 工具。它需要同时支持：

- 命令行自动化；
- 本地 HTTP API；
- TUI 交互；
- Desktop 工作台；
- future IDE/client；
- packaged app smoke。

产品入口越多，越容易出现 runtime drift：同一件事 CLI 能做，HTTP 不能做；TUI 显示一个状态，Desktop reload 后消失；approval 在 UI 解决了，但 session 没有记录。

因此 Product Surface 的架构目标是：

```text
all surfaces are projections over shared runtime state
```

## 2. 对标参考

### OpenCode

OpenCode 的 TUI/App/CLI 共享 session、provider、MCP、permission、agent、plugin 等 runtime contract。TUI 不是独立玩具，而是运行时状态的操作界面。

对 OpenHarness 的启发：

- TUI/Desktop 不应该自己维护 MCP/approval/task 状态；
- App Bridge 需要稳定 API/SSE；
- CLI golden 和 HTTP tests 都是产品 contract；
- product smoke 能暴露纯 runtime test 看不到的问题。

### Claude Code

Claude Code 在 UI 上强调 task/subagent、permission、tool progress、context、compact/resume 等运行时反馈。对 OpenHarness 来说，重点是让这些状态先进入 session/event，再投影到 TUI/Desktop。

## 3. CLI

CLI 是最早、最快的操作面，也是回归测试最稳定的入口。

当前覆盖：

- `run`；
- `session`；
- `models`；
- `agent`；
- `plugin`；
- `mcp`；
- `auth/providers`；
- `debug/db`；
- `skills`；
- `attach`；
- `approval/question`；
- import/export/share/checkpoint/restore；
- OpenCode-style flags。

CLI 的价值：

- 适合 golden JSON；
- 适合 binary smoke；
- 适合验证 profile/provider/tool/task 入口；
- 适合开发阶段快速确认。

限制：

- 长任务 UI 不如 TUI/Desktop；
- background task lifecycle 还未完整；
- plugin runtime 仍偏 scaffold。

## 4. HTTP Runtime and App Bridge

HTTP Runtime 是多产品入口的 runtime API。

当前能力：

- sessions；
- turns；
- SSE events；
- global events replay；
- approvals/questions；
- models/providers；
- agents；
- MCP config/lifecycle；
- skills；
- task tree；
- checkpoint/diff/restore；
- provider health；
- TUI control routes。

App Bridge 的设计不是“附带 web server”，而是把 runtime state 暴露给外部 surface 的协议层。

## 5. TUI

TUI 已从简单显示器演进为终端操作面：

- session picker；
- file picker；
- model picker；
- agent picker；
- variant/thinking picker；
- theme/color；
- approval/question docks；
- diff/checkpoint rendering；
- App Bridge attach/control；
- remote transcript。

未完成：

- subagent panes；
- task tree navigation；
- plugin panes；
- full command palette；
- configurable keymap；
- 更完整 terminal automation。

## 6. Desktop

Desktop 是目前最能暴露产品化缺口的 surface。

当前已经覆盖：

- workspace shell；
- MCP panel；
- approval dock；
- question reply/dismiss；
- approval/question history；
- checkpoint restore workflow；
- timeline/detail cards；
- packaged app smoke；
- reload persistence。

几个已完成的用户可见循环：

### Desktop MCP panel

能力：

- Add/Edit/Delete/Test；
- Start/Stop/Restart；
- Enable/Disable；
- tool trace；
- reload state；
- PID reuse；
- packaged smoke。

这证明 MCP lifecycle 是 App Bridge runtime state，而不是 Desktop 局部状态。

### Approval dock

能力：

- pending approval card；
- risk/permission chips；
- Allow/Deny；
- allow always；
- question reply/dismiss；
- persisted resolved history；
- packaged smoke pending screenshot。

这证明 approval/question 是 session pause/resume state，而不是 UI prompt。

### Checkpoint restore

能力：

- checkpoint restore；
- restore metadata；
- restore history；
- Desktop timeline/detail；
- reload 后恢复；
- packaged checkpoint smoke。

这证明 checkpoint/restore 必须进入 session metadata。

## 7. 开发过程

产品 surface 不是一次性搭完，而是按 runtime contract 推进：

1. 恢复 CLI command surface 和 golden tests。
2. 建 HTTP Runtime health/session/turn routes。
3. 增加 App Bridge SSE 和 turn lifecycle。
4. TUI attach 到 App Bridge。
5. MCP lifecycle 进入 HTTP/TUI/Desktop。
6. Approval/question 进入 session pause/resume 和 UI dock。
7. Diff/checkpoint/restore 进入 TUI/Desktop。
8. Desktop smoke 覆盖真实用户 workflow。
9. Skills API 和 CLI diagnostics 进入 surface。
10. Task tree API 暴露给 attach/TUI 后续消费。

每个产品阶段都倒逼 runtime 状态更明确。

## 8. 验收证据

代表性命令：

```bash
cargo test -p openagent-cli --test cli_commands -q
cargo test -p openagent-http-runtime --test http_runtime -q
cargo test -p openagent-tui -q
npm --prefix desktop run smoke:local-mcp-ui
npm --prefix desktop run smoke:approval-dock
npm --prefix desktop run smoke:checkpoint-restore-ui
```

Desktop packaged smoke 还覆盖真实 Tauri app 的关键路径。

## 9. 后续边界

1. TUI/Desktop subagent panes 和 task tree navigation。
2. Plugin panes 和 plugin runtime controls。
3. Attachment/media/resource 更完整。
4. Product-level provider/model/agent consistency。
5. Terminal automation 覆盖长运行场景。
6. Desktop session restore 和 task background 操作继续产品化。
