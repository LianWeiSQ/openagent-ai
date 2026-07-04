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

## 10. Product surface 的架构原则

Product surface 的核心原则是：入口可以不同，事实必须相同。

```text
CLI
HTTP/App Bridge
TUI
Desktop
future IDE/client
  -> shared runtime state
  -> session/event/registry projection
```

如果某个能力只能在一个入口使用，要判断它是产品交互差异，还是 runtime 没有下沉。前者可以接受，后者需要回到 runtime 层补对象。

## 11. Surface 合同

### CLI contract

CLI 的合同是：

- 可脚本化；
- JSON 输出稳定；
- 适合 golden tests；
- 能覆盖 profile/provider/MCP/skill/task 的主路径；
- 不依赖 UI state。

CLI 是最适合做回归门禁的 surface。

### HTTP/App Bridge contract

HTTP/App Bridge 的合同是：

- session/turn/event API 稳定；
- SSE 可 replay；
- approval/question/task/checkpoint/MCP/skill 都有结构化 route；
- 多 UI 入口消费同一状态；
- failure payload 可诊断。

它是 TUI/Desktop/future client 的协议层。

### TUI contract

TUI 的合同是：

- 终端内操作复杂 session；
- attach 到 App Bridge；
- 显示 transcript、files、model、agent、approval、diff；
- 不自己拥有 runtime 事实。

后续 subagent pane、task tree、plugin pane 都应从 App Bridge 消费数据。

### Desktop contract

Desktop 的合同是：

- 暴露完整工作台体验；
- 覆盖 reload/persistence；
- 用 packaged smoke 验证真实 app；
- 把复杂 workflow 逼回 runtime contract。

Desktop 往往最早暴露“runtime 状态没有持久化”的问题。

## 12. 开发过程细化

### Step 1: CLI 恢复和 golden

先确保核心命令可跑、输出稳定。CLI 是最早的验收门。

### Step 2: HTTP Runtime

把 session、turn、events、provider health、MCP、skills、tasks 暴露成 API，而不是让 TUI/Desktop 直接调 CLI。

### Step 3: App Bridge SSE

建立实时事件流。所有长任务、approval、tool call、MCP lifecycle 都应能通过事件推送。

### Step 4: TUI attach

TUI 先 attach 到 App Bridge，验证协议能支撑终端交互。

### Step 5: Desktop workflows

Desktop 实现 MCP panel、approval dock、checkpoint restore 等真实 workflow。每做一个 workflow，都检查 runtime state 是否足够。

### Step 6: Packaged smoke

开发环境通过不代表 packaged app 可用。Tauri packaged smoke 能验证路径、权限、资源、端口、reload、bridge 都没有隐含开发机假设。

### Step 7: Shared runner 收口

当 surface 足够多后，重复 loop 成本会上升。此时继续补 UI 不如先把 SessionRunner 抽出来，减少 runtime drift。

## 13. 产品面暴露出的 runtime 问题

| 产品需求 | 暴露的问题 | runtime 补齐 |
| --- | --- | --- |
| Desktop MCP panel reload 后保持状态 | MCP lifecycle 不应在 UI 内 | App Bridge MCP state |
| Approval dock 历史可见 | approval 不是弹窗 | session pause/resume state |
| Checkpoint restore timeline | restore 不是一次 git 操作 | session checkpoint metadata |
| TUI attach remote session | transcript 不能只在 CLI stdout | session/event API |
| Model picker | provider/model 不是 env 字符串 | provider catalog/health |
| Task tree | subagent 不是 tool text | child session/task metadata |
| Skill list/show | skill 不是文件路径 | skill registry/API |

产品面不是只做 UI，它在持续检验 runtime 合同是否完整。

## 14. 对标差距

| 能力 | OpenCode/Claude Code 参考 | OpenHarness 状态 |
| --- | --- | --- |
| CLI parity | OpenCode CLI | 大部分基础已恢复 |
| TUI runtime operation | OpenCode TUI | 部分 |
| App Bridge/client API | OpenCode App/session API | 部分 |
| Desktop workbench | Claude/OpenCode product surface | 部分 |
| Subagent panes | Claude subagent visibility | 待补 |
| Plugin panes | OpenCode plugin UI | 待补 |
| Task background controls | OpenCode task UI | 待补 |
| Context/compact visibility | Claude context UX | 待补 |

产品面后续重点不是做更多静态页面，而是把 subagent、task、plugin、context 这些 runtime 对象可视化。

## 15. 验收口径

Product surface 改动至少要按层验证：

- CLI：binary smoke/golden；
- HTTP：endpoint + SSE contract；
- TUI：render/state/control tests；
- Desktop：dev smoke + packaged smoke；
- reload：状态不丢；
- cross-surface：同一 session 在不同入口看到一致事实。

如果一个 workflow 只在开发 UI 中手点过，没有 API/session evidence，就不能算完成。
