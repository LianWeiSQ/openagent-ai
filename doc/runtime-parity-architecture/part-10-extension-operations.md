# Part 10 - Extension And Operations

## 1. 需求背景

当 harness 从 CLI 工具变成 runtime，外围能力也必须系统化：

- plugin；
- GitHub/PR workflow；
- debug/db；
- eval/replay；
- lifecycle；
- packaging；
- diagnostics；
- runbooks。

这些能力不应该塞进核心 agent loop。它们属于 extension and operations plane：围绕 runtime 提供扩展、配置、检查、回放、交付和运维。

## 2. 对标参考

### OpenCode plugin/config lifecycle

OpenCode 的 plugin/provider/catalog 设计强调：

- plugin 可以贡献 provider、command、skill 等能力；
- transform/registry 需要可重放；
- disable plugin 后影响要能撤销；
- config/auth/catalog/policy 变化要触发 runtime reload；
- TUI/App 需要看到变化后的状态。

对 OpenHarness 的启发：

- plugin 不应只是 CLI install 记录；
- plugin 最终应成为 skills、MCP servers、providers、commands、UI panes 的来源；
- plugin 影响 runtime state 时必须有生命周期和 cleanup。

### Claude Code operations

Claude Code 的 task、swarm、worktree、remote、permissions、MCP、skills 都有较强的运行时操作面。对 OpenHarness 来说，operations 的关键是把复杂执行留下可解释证据。

## 3. Plugin layer

当前 plugin 支持：

- local/module/remote registry install；
- list/show；
- enable/disable；
- remove；
- manifest-backed dry-run dispatch。

明确限制：

- `plugin run` 目前仍是 registry/config scaffolding；
- 没有真实 npm/plugin runtime execution；
- plugin-provided MCP/skills/providers 还未进入统一 registry。

未来设计：

```text
plugin manifest
  -> declared contributions
  -> registry scope
  -> runtime materialization
  -> cleanup on disable/remove
```

可能贡献：

- skills；
- MCP servers；
- commands；
- providers/models；
- TUI/Desktop panes；
- workflow templates。

## 4. GitHub and PR helpers

当前 GitHub/PR 能力偏本地 workflow scaffolding：

- status；
- issue；
- PR list/view/checkout/template/review；
- workflow helpers；
- `gh` integration。

对标 OpenCode 仍缺：

- GitHub agent install/run；
- share/import 更深集成；
- remote hosted workflow；
- PR review agent 的 runtime 化。

设计原则：

```text
GitHub should be an extension capability, not hardwired agent loop behavior
```

它应通过 tools、commands、skills、task templates 或 plugin 进入 runtime。

## 5. Debug and DB

Debug/DB 是运维入口：

- paths；
- env；
- sessions；
- files；
- rg/search；
- bundle；
- DB path/summary/rebuild/query/schema/export。

当前能力能做本地检查，但 OpenCode snapshot-level debug 还更强。

后续需要：

- snapshot-grade debug bundle；
- event/session/provider/MCP/task 一体化快照；
- failure reproduction artifact；
- redaction policy；
- support bundle。

## 6. Eval and replay

OpenHarness 有 Terminal-Bench、Harbor、Langfuse、JSONL-friendly observability 等方向。

原则：

- eval/replay 消费 session/trace；
- 不再创造独立状态模型；
- run receipt 和 score export 要能回链到 session evidence。

这对长期迭代非常关键。没有 evidence，agent runtime 的质量会变成主观体验。

## 7. Lifecycle and packaging

当前 `upgrade`/`uninstall` 是 dry-run plan，而不是 destructive action。

这是有意设计：

```text
source-tree harness
  -> no destructive lifecycle mutation

packaged distribution
  -> can own upgrade/uninstall behavior
```

在本地源码工作区，runtime 不应该随意删除或替换自己。packaged app 具备明确安装位置和权限后，才能接管 lifecycle。

## 8. 开发过程

Extension/operations 的落地顺序应该保守：

1. 先定义命令边界。
2. 增加安全 scaffold。
3. 输出 machine-readable JSON。
4. 增加 golden/integration tests。
5. 接入 session/trace evidence。
6. 再考虑真实 runtime execution。

这也是为什么 plugin runtime 和 destructive lifecycle 还没有直接打开。先有状态、权限、回滚和测试，再执行。

## 9. 当前能力分类

| 需求域 | 当前状态 | 主要缺口 |
| --- | --- | --- |
| Plugin registry | Partial | runtime execution、贡献 skills/MCP/providers |
| GitHub/PR | Partial | hosted agent/install/run parity |
| Debug/DB | Partial | snapshot-grade bundle |
| Eval/replay | Partial | 更完整 dataset/runbook/dashboard |
| Lifecycle | Deferred | packaged distribution 之后启用 |
| Operations docs | Partial | provider/MCP/task failure runbooks |

## 10. 后续边界

1. 真实 plugin runtime execution。
2. plugin-provided skills/MCP/providers。
3. Plugin lifecycle cleanup 和 reload。
4. GitHub agent workflow runtime 化。
5. Snapshot debug bundle。
6. Packaging-owned upgrade/uninstall。
7. MCP/provider/task failure runbook。
8. Eval/replay 与 session event 更深绑定。

## 11. Extension plane 的边界

Extension plane 负责把外部能力接入 harness，但不能破坏核心 runtime 的边界。它应遵守：

```text
extension declaration
  -> registry
  -> permission/policy
  -> runtime materialization
  -> session/event evidence
  -> lifecycle cleanup
```

Plugin、GitHub、debug/db、eval、packaging 都属于这个平面。它们不是 agent loop 的特例，而是围绕 runtime 的能力入口和运维工具。

## 12. Plugin 设计方向

Plugin 最终应该能贡献：

- skills；
- MCP servers；
- providers/models；
- commands；
- workflows；
- TUI/Desktop panes；
- templates；
- eval fixtures。

但每一种贡献都必须进入对应 registry，而不是 plugin 自己绕过 runtime。例如 plugin-provided skill 进入 skill registry，plugin-provided MCP server 进入 MCP config/lifecycle，plugin-provided provider 进入 provider catalog。

这样 disable/remove plugin 时，runtime 才能清理贡献项。

## 13. Operations 设计方向

Operations 能力解决“出问题时怎么查”的问题。

### Debug bundle

理想 debug bundle 应包含：

- runtime version/build info；
- workspace roots；
- redacted config；
- provider/model status；
- MCP status；
- session/event slice；
- task tree；
- skill registry snapshot；
- recent errors；
- checkpoint/restore metadata；
- redaction report。

### DB tools

DB/debug 命令应支持 schema、query、export、rebuild，但要避免 destructive 默认行为。任何修复型命令都应先 dry-run。

### Eval/replay

Eval 不应绕过 session/event。好的 replay 应能从 session trace 还原 provider/tool/task/skill 关键路径，输出 run receipt 和评分。

## 14. 开发过程细化

### Step 1: 安全 scaffold

先实现 list/show/plan/dry-run，不直接执行危险操作。

### Step 2: Machine-readable 输出

所有 operations 命令都应支持 JSON，便于 smoke、CI、Desktop/TUI 消费。

### Step 3: Registry 接入

Plugin 或 extension 的贡献项进入对应 registry，不在 CLI 命令里私自生效。

### Step 4: Lifecycle cleanup

enable/disable/remove 必须能撤销影响。没有 cleanup，就不能打开真实 runtime execution。

### Step 5: Evidence binding

操作结果写入 session/trace/debug artifact，方便复现。

### Step 6: Product visibility

TUI/Desktop 只显示 runtime registry 和 operation status，不维护另一份 extension 状态。

## 15. GitHub/PR workflow 的定位

GitHub 能力不应硬编码在 agent loop 里。更合理的形态是：

```text
GitHub plugin/skill/tool
  -> PR/issue capability
  -> permission
  -> task template
  -> session evidence
```

比如 PR review agent 可以是 profile + skill + GitHub tools + task template 的组合，而不是在核心 loop 中加入 `if github` 分支。

## 16. Packaging lifecycle

源码工作区和 packaged distribution 的 lifecycle 不一样：

- 源码工作区：upgrade/uninstall 默认 dry-run，避免删除开发环境；
- packaged app：可以拥有安装路径、版本更新、卸载清理；
- enterprise/remote：需要更强的 policy 和 audit。

这个边界解释了为什么当前 lifecycle 偏保守。

## 17. 对标差距

| 能力 | OpenCode/Claude Code 参考 | OpenHarness 状态 |
| --- | --- | --- |
| Plugin registry | OpenCode plugin/config | 部分 |
| Plugin runtime execution | OpenCode plugin runtime | 未完成 |
| Plugin contributions | skills/MCP/providers/commands | 未完成 |
| GitHub workflow | hosted/agent workflow | 部分 |
| Debug snapshot | support bundle | 部分 |
| Eval/replay | quality loop | 部分 |
| Packaged lifecycle | distribution-owned ops | 部分 |
| Runbooks | operator docs | 待补 |

Extension/operations 的原则是先可诊断、可回滚，再打开更强执行能力。

## 18. 验收口径

这类能力不能只看命令能否跑，还要看：

- JSON 输出是否稳定；
- secrets 是否 redacted；
- enable/disable/remove 是否可回滚；
- plugin 贡献项是否进入正确 registry；
- debug bundle 是否能复现问题；
- eval/replay 是否回链 session evidence；
- destructive 操作是否默认 dry-run 或有明确确认；
- packaged app 与源码工作区是否走不同 lifecycle。

这能避免 operations 变成一堆难以维护的脚本。
