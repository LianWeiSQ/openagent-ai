# Goal State

## Active Context Compression Goal

- original_user_request: 收口 ContextPackBuilder 为唯一模型输入入口；补齐 ContextItem taxonomy；依次实现 typed ContextEpoch、micro compact、semantic anchor registry、budget allocator、compact eval/golden suite；每阶段完整测试并推送 GitHub。
- objective_locked: 按上述顺序完成七个可独立验证、可独立推送的上下文压缩阶段。
- current_slice: S5 completed - continuation-critical facts 已升级为版本化 `SemanticAnchor` 与确定性 registry；manual/automatic compaction 将当前语义快照写入 typed epoch，CLI/HTTP 可跨 compaction、restart 与 receipt replay 恢复同一 registry。
- slice_boundary: `openagent.semantic_anchor.v1`、`openagent.semantic_anchor_registry.v1`、goal/constraint/decision/progress/file/critical-context/blocker/next-step/recovery-point taxonomy、authority/priority/latest 冲突解析、stable ID/content hash、ContextPack pinned item 与完整性校验、typed epoch/session/replay、CLI/HTTP redacted diagnostics、golden 与架构文档；不实现 S6 budget allocator。
- success_criteria: anchors 具有稳定身份、规范 hash、来源、authority、scope 与 references；live todo/checkpoint 不重复占用模型上下文，compaction 时按 durable ID 快照；anchors 不走普通 drop/truncate，provider 不接收 provenance/reference，公开 diagnostics 不泄漏正文或凭据；旧空-registry pack hash 保持兼容；CLI/HTTP、跨 epoch/restart/replay、golden、全 workspace tests、strict clippy、format/diff/secret scan 通过；阶段提交已推送。
- milestones: S1-S5 completed; S6-S7 pending.
- last_receipts:
  - 2026-07-22: changed: 新增 `openagent.semantic_anchor.v1` 与 `openagent.semantic_anchor_registry.v1`，把目标、约束、决策、进展、文件、关键上下文、阻塞、下一步和恢复点变成稳定 ID、authority/priority 冲突解析、规范 hash 与 typed references；ContextPack 将已注册 anchor 投影为独立 pinned item 并拒绝 missing/orphan/tampered item，provider 只接收 XML 转义后的 ID/kind/content；manual/automatic compaction 合并上一 epoch、structured work state、active todo、checkpoint 与当前 recovery boundary，完整 registry 写入 parent-linked epoch，CLI/HTTP restart 和 receipt replay 恢复同 hash；公开 diagnostics 只暴露净化 source、hash 和 counts，空 registry 不改变历史 pack hash。verified: versioned protocol/taxonomy golden、authority/latest/path-case/reference canonicalization、Core budget/drop/integrity/provider leakage、Session epoch parent chain/event redaction、CLI fake-provider continue、HTTP repeated compaction/restart/public diagnostics/zero-side-effect replay；最终 `cargo test --workspace --all-targets --quiet` 318 tests、`cargo clippy --workspace --all-targets -- -D warnings`、format/diff/credential scan 全通过。next: S6 - budget allocator，按 taxonomy、anchor kind、scope、recency 与 recoverability 分配硬/软预算并提供可解释 drop/fitting receipt。
  - 2026-07-22: changed: 新增 `openagent.context_micro_compaction.v1`，`ContextPackBuilder` 在预算选择前对超字节或超行数的普通 tool result 做确定性 UTF-8 head/tail 投影，仅在净 token 节省为正时生效；原始 transcript v2 part 不变，投影 metadata 清除 raw output，并记录 content hash、尺寸、节省量和 durable message/part recovery reference；loaded skill 受保护；CLI/HTTP 持久化 trace，公开 diagnostics 脱敏，receipt replay/restart 重建同 hash。verified: Core versioned golden、单行 Unicode/行数阈值/taxonomy/禁用/skill 保护，Protocol session raw ledger，CLI 与 HTTP 真实两步 provider E2E、HTTP replay/restart/零副作用，全 workspace tests、strict clippy、format/diff/high-confidence secret scan 全通过。next: S5 - semantic anchor registry，把任务目标、关键决策、约束、文件与恢复点注册为可预算、可引用、跨 epoch 保留的 typed anchors。
  - 2026-07-22: changed: 新增 `openagent.context_epoch.v1`，统一 manual/automatic compaction 的身份、触发、原因、边界、parent、pack provenance 与 structured work state；SessionStore 只写 typed epoch、原始 ledger 可枚举完整 parent 链，Core 统一 typed/legacy metadata 投影，CLI/HTTP 共用恢复路径，旧 free-form transcript 保持只读兼容，事件只记录脱敏 diagnostics。verified: protocol/schema validation 与 golden、session parent chain/legacy boundary/loaded skill、Core provider projection、CLI 39 tests、HTTP manual/automatic + restart/replay、`cargo test --workspace --all-targets`、workspace strict clippy、format/diff/secret scan 全通过。next: S4 - micro compact，优先治理大 tool output，并保留可恢复的摘要、引用与截断 provenance。
  - 2026-07-22: changed: 新增 `openagent.context_item_taxonomy.v1`，按 category/origin/scope/compaction 四维分类全部内置来源；builder 升级旧 item，trace/receipt 与 HTTP diagnostics 暴露脱敏 taxonomy，provider payload 不携带 taxonomy；新增 source golden 与历史 trace 兼容测试。verified: `cargo test --workspace --all-targets`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo fmt --all -- --check`、`git diff --check`、新增行 secret scan 全通过。next: S3 - 将 compaction boundary 升级为 typed ContextEpoch。
- blockers: none

---

- original_user_request: 持续推进统一 ContextPackBuilder；统一系统提示词、项目指令、会话历史、attachments、skills、MCP tool manifests、todo、checkpoint、模型参数，解决回答不够深入、探索度不够的问题。
- objective_locked: 让 OpenHarness 的 `ContextPackBuilder` 成为 Rust Agent Runtime 唯一的 prompt/context 装配入口，并让 CLI/TUI/Bridge/Desktop 共享同一份可预算、可恢复、可观测的上下文契约。
- product_boundary:
  - `/Users/william/coding/harness/openharness`: ContextPack schema、builder、runtime materialization、session persistence、provider adapter、CLI/TUI/Bridge 接入与测试。
  - `/Users/william/coding/harness/app`: 只消费 Bridge 暴露的 context trace/diagnostics；不在前端重新拼 prompt。
- non_goals:
  - 不在本目标内扩散无关 UI、Git、LSP、MCP lifecycle 或 CLI 小命令。
  - 不把 provider 私钥、私有 Base URL 或真实请求正文写入 trace、日志、fixture 或 Git。
  - 不一次性删除旧路径；必须先 shadow compare，再切默认入口，最后清理 legacy。
  - 不推 GitHub，除非 William 明确要求。
- success_criteria:
  - 所有 provider turn 都只从一个 `ContextPackBuilder` 结果生成 messages、tools 和 model options。
  - 系统提示词、项目/用户指令、历史、attachments、skills、MCP manifests、todo、checkpoint/work state 均为结构化 context item，并有明确优先级、来源、稳定 ID 和去重规则。
  - token budget、模型能力、压缩边界、截断与 drop reason 可追踪；同一输入产生确定性 pack/hash。
  - 每个 turn 持久化脱敏后的 context receipt；retry/resume/recovery 能复用或重建等价 pack。
  - CLI、TUI、Bridge、Desktop 不再各自拼上下文；跨入口 golden parity 测试通过。
  - attachments 保持类型与元数据，不再只拼成普通文本；skills/MCP tool schema 与实际可调用工具一致。
  - 上下文 inspector 能解释“用了什么、丢了什么、为什么”，但不泄漏 secret 或超量正文。
  - 代表性工程任务的指令遵循、文件探索、工具选择和最终答案完整度基线不回退，并有可重复验收。
- architecture_contract:
  - `ContextPackInput`: 只接受结构化来源，不接受各入口提前拼好的大字符串。
  - `ContextPackBuilder`: 完成规范化、优先级、去重、预算、压缩选择、稳定 hash 和 trace。
  - `ContextPack`: 输出 provider messages、tool manifests、model options、receipt/trace 和预算统计。
  - provider adapter: 只做 wire API 转换，不再决定上下文内容。
  - session store: 保存来源引用、pack hash、receipt 和 compaction/recovery 边界。
- milestones:
  - id: M0
    status: completed
    value: 锁定 context schema、优先级和基线；让现有 builder 在 Bridge provider turn 中 shadow build/compare，不改变线上结果。
    risk: low
    verify: context contract tests + shadow diff fixture + secret redaction test。
    visible_to_user: context diagnostics 能显示 legacy 与 builder 差异。
  - id: M1
    status: completed
    value: Bridge/provider 主链切到 builder，统一 system prompt、项目指令、历史和 model options，并持久化 turn receipt。
    risk: high
    verify: provider-loop integration、retry/resume 等价 pack、历史与指令 golden tests。
    visible_to_user: 回答能够稳定继承项目指令和会话上下文。
  - id: M2
    status: completed
    value: 结构化接入 attachments、skills、MCP tool manifests、todo、checkpoint/work state，工具清单与实际执行器一致。
    risk: medium
    verify: typed attachment、skill/MCP availability、todo/checkpoint recovery 端到端测试。
    visible_to_user: 附件、技能、MCP 和待办在同一轮可靠生效。
  - id: M3
    status: completed
    value: 完成模型感知 token budget、稳定前缀、去重、压缩、截断和缓存策略。
    risk: high
    verify: 不同模型预算 fixture、compaction boundary、deterministic hash、超长会话恢复测试。
    visible_to_user: 长会话不再随机丢关键上下文，响应延迟和 token 使用可解释。
  - id: M4
    status: completed
    value: CLI/TUI/Bridge 全部迁移到统一 builder；Desktop 只通过 Bridge 传结构化输入并读取 diagnostics。
    risk: medium
    verify: cross-surface golden parity + CLI/TUI/HTTP smoke。
    visible_to_user: 从任何入口运行同一任务，核心上下文语义一致。
  - id: M5
    status: completed
    value: 完成可观测性、安全与可靠性，包括 context inspector、脱敏、receipt replay、失败分类和性能指标。
    risk: medium
    verify: redaction、corrupt/missing receipt recovery、SSE diagnostics、性能预算测试。
    visible_to_user: 能看懂 Agent 为什么这么回答，并能诊断上下文缺失。
  - id: M6
    status: completed
    value: 默认启用统一 builder，删除 legacy 拼接路径，完成深度/探索质量验收。
    risk: high
    verify: 全量 Rust tests、入口 smoke、legacy scan、代表性仓库探索评测对比。
    visible_to_user: 统一 ContextPackBuilder 正式成为唯一执行路径。
- current_slice: M6 与长期目标已完成；统一 `ContextPackBuilder` 已成为 Rust Agent Runtime 唯一的 prompt/context 装配入口，CLI/TUI/Bridge/Desktop 共用结构化输入、provider 边界校验、receipt/replay/restart 与 diagnostics 契约。
- slice_boundary: 目标内无剩余实现或验收事项；后续新增能力应作为新的独立目标启动，不继续扩散本 ContextPackBuilder 主线。
- allowed_actions: 后续每轮只推进一个 milestone 内的高价值闭环；集中验证；receipt 只写 changed/verified/next。
- interrupt_policy: 默认每轮一个 slice；任何 stop/status 立即交还控制权；只有明确要求 autopilot/持续跑才连续推进。
- last_receipts:
  - 2026-07-17: changed: 完成 M6 最终发布门禁，统一 history/source contract 与 provider pack 边界正式收口；顺带清理 Rust 1.96 暴露的 LSP/session/tools 机械 lint，使全 workspace strict clippy 恢复为零告警。verified: `cargo test --workspace --all-targets` 共 296 tests、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、51 条 lint 相关定向回归、App `npm run ci:p0`、298 文件 secret scan、Desktop session/context/replay/restart/provider recovery smoke、100 分代表性仓库 exploration quality baseline、CLI/Bridge parity、生产 source/provider boundary scan、format/diff check 全通过；未发现 legacy prompt/context 装配旁路，未推 GitHub。next: none；长期目标完成。
  - 2026-07-17: changed: Core 新增统一 history materializer 与旧/新消息下标映射，旧 profile system 淘汰、compaction 转 typed work state、任意 legacy system 去重并并入统一 system source；attachment/work-state 位置在移除旧 system 后保持正确；CLI/HTTP/replay 删除重复分类逻辑，Bridge 新增旧 transcript 跨重启/replay 回归。verified: provider/tools/core/session/CLI/HTTP/Bridge/TUI/eval 相关 257 tests、Core/CLI/HTTP scoped strict clippy、format/diff/legacy/secret scan 全通过；旧 profile、legacy system、compaction、attachment、restart、replay 均覆盖且 provider/tool 副作用为 0。next: M6 最终发布门禁，跑全 workspace 测试与入口 smoke，复验 exploration quality baseline 和所有 ContextPack source/provider 边界，确认无剩余 legacy 旁路后再完成长期目标。
  - 2026-07-17: changed: Core 新增 `ContextSystemSources`、typed profile/instruction/skill trace sources 与脱敏 `ContextSystemDiagnostics`，Builder 成为唯一 system materializer；CLI/HTTP/replay 删除各自 system-message 拼接并持久化/公开 diagnostics；删除无调用者的旧公开 `build_agent_system_prompt` 旁路；修复 Builder message 转换丢失 subagent profile metadata。verified: 相关 255 Rust tests、Core/CLI/HTTP scoped strict clippy、format/diff/secret scan 全通过，动态 instruction 刷新、公开 diagnostics、CLI/Bridge parity、subagent 4 条回归均通过。next: 禁止生产 `ContextPackInput.messages` 直接注入预拼 `Role::System`，把旧会话 system history 显式迁移为 typed legacy source，并增加恢复/parity invariant。
  - 2026-07-17: changed: `openagent-eval` 新增 exploration quality observation/rubric/scorer/baseline comparison，量化 context/tool/file/answer coverage、失败与重复工具调用；HTTP 新增代表性 Rust workspace 多步 Agent Loop，跨轮恢复 todo/checkpoint，并同时验证 project instruction、typed attachment、preloaded skill、5 类可用工具、`ls + grep + 4 read` 文件探索和证据化最终答案；版本化 100 分基线可阻止浅答、漏文件、工具失败和答案证据回退。verified: provider/tools/core/session/CLI/HTTP/Bridge/TUI/eval 相关 254 tests、eval/http scoped strict clippy、format/diff/secret scan 全通过。next: 把 profile prompt、project/user instructions、skills catalog/preload 从 CLI/HTTP 各自的 system-message 物化逻辑迁入共享结构化 source materializer/Builder 输入，删除剩余入口重复装配。
  - 2026-07-17: changed: CLI 与 HTTP provider adapter 改为只接收并校验完整 `ContextPack`，拒绝 messages/tools/model options 或 hash 被篡改的输入；删除 legacy `build_run_prompt` 附件文本拼接，HTTP fixture 改用 structured turn attachment；新增生产入口 invariant，锁定 payload builder 只能存在于 provider 库及两个受控 adapter。verified: provider/tools/core/session/CLI/HTTP/Bridge/TUI 相关 248 tests、core/http/cli scoped strict clippy、HTTP golden、format/diff/secret scan 全通过。next: M6 建立代表性仓库探索质量基线与对照，量化指令遵循、文件覆盖、工具选择、无效调用和最终答案完整度；再据证据收口 CLI/HTTP 仍重复的 source materialization。
  - 2026-07-17: changed: 统一 `ContextFailure` 稳定码覆盖 unavailable/corrupt/budget exceeded/source drift/replay unsupported，CLI/Bridge/SSE/Desktop 共用；active Context envelope 记录 materialize/build/persist/provider payload build+serialize/bytes 指标，公开 API 严格 allowlist，Inspector 展示失败与性能；新增 5k 消息构建基线。verified: 相关 Rust 247 tests、core/http/cli scoped strict clippy、Desktop build 与 session lifecycle Playwright smoke、format/diff/secret scan 全通过；workspace strict clippy 仅被既有 LSP 4 条 Rust 1.96 lint 阻塞。next: M6 扫描并删除 legacy prompt/context 拼接路径，确认 builder 为唯一默认入口，再做代表性仓库探索深度对比。
  - 2026-07-17: changed: Bridge 新增 `POST /api/sessions/{session_id}/context/replay`、`context/replayed` 事件与 client 方法；receipt 私有保存可重建规格并按历史 materialized message 边界截断，replay 分类为 verified/drifted/rebuilt/unrecoverable，missing/corrupt latest 恢复为 durable recovery receipt；Desktop Inspector 新增“验证重建”及差异原因，公开 API/事件不返回 prompt、附件正文、secret 或私有 replay spec；verified: 后续消息不会污染历史 pack，corrupt latest 可跨重启恢复，provider/tool/checkpoint/MCP lifecycle 副作用均为 0，未知模型参数值不持久化；相关 Rust 247 tests、strict scoped clippy、Desktop build 与 session lifecycle Playwright smoke、format/diff/secret scan 全通过；next: M5 建立统一失败分类矩阵和 context build/provider payload 性能预算、压力基线。
  - 2026-07-17: changed: Context Pack envelope 持久化脱敏 trace；Bridge 新增 `GET /api/sessions/{session_id}/context`、`context/updated` 事件和 client 方法，API 严格 allowlist 且不返回 prompt/attachment content/secret；Desktop Inspector 新增上下文卡片，展示来源决策、预算、消息/工具数量、prefix reuse、重建和历史，并按 session 隔离、事件刷新、重启恢复；verified: 相关 Rust 241 tests 全通过，Desktop build 与真实 provider/session lifecycle Playwright smoke 通过，验证 Inspector 可见、跨会话不串数据、Bridge 重启后仍恢复；本轮 crate `--no-deps -D warnings` clippy、format/diff/secret scan 通过，workspace strict clippy 仍被既有 LSP 4 条 Rust 1.96 lint 阻塞；next: M5 receipt replay + missing/corrupt receipt 自动重建与失败分类，再建立 context build/provider payload 性能预算。
  - 2026-07-17: changed: 本地 CLI 每个 provider step 改为消费 active `ContextPackBuilder`，结构化恢复 profile/system、history、attachments、skills、MCP manifests、todo、checkpoint/work state 与模型参数；provider crate 统一 OpenAgent 文本模型 128K context 规格，tools crate 统一 CLI/Bridge 内置 agent catalog，移除跨入口 `task` schema 漂移；local 与 Bridge parity 测试捕获真实 provider 请求并锁定 messages、完整 tools schema、model options、receipt 和 provider hash 一致；verified: provider/tools/core/session/CLI/HTTP/Bridge/TUI 共 239 tests 全通过，scoped lib/bin strict clippy、format、diff check 通过，真实 provider parity 通过；next: M5 建立脱敏 context diagnostics/Inspector 契约、receipt replay 与 missing/corrupt receipt 恢复。
  - 2026-07-16: changed: Bridge client 新增共享 `RemoteTurnRequest`/`RemoteTurnAttachment`；Desktop 既有请求、TUI composer、CLI `run --attach` 与 `client --file` 现在保持原始用户正文并单独提交 typed attachments/model options，兼容 legacy options；远端入口端到端 receipt 能看到 attachment source，provider retry/fallback 在 CLI 事件流中不再丢失；verified: Bridge client、CLI、TUI、HTTP Runtime 集中回归 178 tests 全通过，strict scoped clippy、format 与 diff check 通过；next: 将本地 CLI Agent Loop 的 system/profile/history/skills/tools/MCP/todo/checkpoint/model options 装配迁到 ContextPackBuilder，并建立本地 CLI 与 Bridge 的 golden pack/receipt parity。
  - 2026-07-16: changed: `ContextPackBuilder` 将 provider 可见消息固定分为 stable system/project/skills 前缀与 dynamic history/todo/checkpoint/work-state 后缀；稳定项按规范化语义指纹跨来源去重，并在 trace 中记录 winner/drop reason；新增独立 `openagent.context_stable_prefix.v1`，hash 覆盖实际稳定消息、可调用 tool manifests、model options 与 model ID，receipt 暴露 cache eligibility、miss/changed/reused 与原始 run/step 来源，明确标注为 logical prefix reuse 而非 provider 计费缓存命中；verified: Core 确认动态历史变化不改变 prefix hash、稳定指令变化会改变 hash、语义重复项确定性去重；HTTP 确认多 step `miss -> reused`、503 retry 复用完全相同 pack、compact/runtime restart 后仍复用原稳定前缀，动态项目指令刷新为 `changed`；Core/Session/HTTP 集中回归 107 tests 全通过，scoped strict clippy、format、diff check 通过；next: M4 迁移并锁定 CLI/TUI/Bridge 的跨入口 parity，Desktop 只提交结构化输入并消费 diagnostics。
  - 2026-07-16: changed: `ContextPackBuilder` 在 required pinned items 总量超过模型 item budget 时执行确定性 source-aware fitting，按 checkpoint/attachment/todo/runtime/work state/system/latest-user 顺序释放空间，各来源使用 header/head/tail 或 instruction XML section 保真；item ID 保持不变，metadata 与 trace 增加 original token/bytes、retained bytes、`required_context_budget` reason 和 strategy，receipt 汇总 truncated count/reason/strategy；`strategy=error` 显式关闭 fitting，继续返回 `required_budget_exhausted`；system 裁剪专门保护 profile 开头、`<instructions>` 头尾和系统尾部，避免项目指令位于 monolithic system prompt 中部时被吞掉；verified: Core 新增五类 required source 确定性/严格模式测试，HTTP fake provider 真实请求验证 profile、项目指令、latest user、attachment 头尾保留、中部 sentinel 未发送、请求有界且不触发无意义 auto compaction；Core/Session/HTTP 集中回归 107 tests 全通过，scoped strict clippy、format、diff check、secret scan 通过；next: M3 stable prefix + semantic dedupe + cache reuse，生成独立稳定前缀 hash/边界并验证同 session 多 step/retry/重启的可复用性。
  - 2026-07-16: changed: active provider pack 在 `model_context_budget` 持续丢弃历史或 required overflow 时自动选择最近用户轮次之前的 compactable prefix，将用户任务、assistant 进度、工具发现/阻塞和 active todo 写成 bounded structured work state，追加携带 typed metadata 的 durable compaction boundary 后重建 pack；receipt 持久化 before/after budget 与 rebuild reason，work state 排在保留消息之前，新增 run-scoped message ID 避免 compaction 后 transcript ID 冲突；verified: 新增第四轮触发自动压缩、第五轮 runtime restart 恢复的 HTTP 端到端测试，确认 provider 请求完成、latest user 保留、work state 恢复、receipt before/after 可解释且 message ID 唯一；原 trim-only 路径仍通过，Core/Session/HTTP 集中回归 105 tests 全通过，scoped strict clippy、format、diff check、secret scan 通过；next: M3 required context 的 source-aware fitting/truncation，让超大系统指令、attachments、todo/work state 在保留语义和明确 truncation reason 的前提下可装入预算，避免只能在 rebuild 后失败。
  - 2026-07-16: changed: 新增 `ContextPackBudget` 与 model-aware build options，按模型 context window/reserved output/safety margin 计算输入上限，并按 provider 可见消息/tool/model options 估算；system 与最新 user 成为 required item，普通历史使用 `model_context_budget`、必需项使用 `required_budget_exhausted`，Bridge 每个 provider step 用同一 active pack 发起真实请求且内部 budget 参数不泄漏到 wire payload；fake provider 改为按 Content-Length 完整读取大请求；verified: 小模型 deterministic core fixture 与大历史 HTTP 端到端均通过，真实第二轮 payload 保留最新问题并剔除超预算旧回答；Core/Session/HTTP 集中回归 104 tests 全通过，scoped strict clippy、format、diff check、secret scan 通过；next: M3 自动 compaction/rebuild，在 required context overflow 或持续预算压力时生成/刷新 structured work state 后重建等价 pack。
  - 2026-07-16: changed: `ContextPackInput` 新增 typed `ContextTodo`、`ContextCheckpoint`、`ContextWorkState`；`todowrite/todoread` 结果同步到 `Session.todos` 并立即持久化；Bridge 每个 provider step 注入 active todo 与最新 checkpoint，compact 创建真实 session compaction boundary，重启后恢复为 typed work state；checkpoint API 统一读取 session store index，compact boundary 失败不再静默成功；verified: Core/Session/HTTP 集中回归 102 tests 全通过（含 21 HTTP unit + 55 integration），端到端覆盖 todo 写入、下一 step provider payload、checkpoint、compact、runtime restart、work state 恢复与 receipt redaction；scoped lib clippy、format、diff check 通过，workspace strict clippy 仍被既有 LSP/MCP/turn lint 阻塞；next: M3 模型感知 token budget、稳定前缀、去重、压缩与 drop reason。
  - 2026-07-16: changed: 新增 `ContextAttachment`/`ContextAttachmentKind` 和稳定内容寻址 ID，`ContextPackInput` 显式接收 attachments 并按来源消息顺序生成 pinned typed item；Bridge 不再用 `build_run_prompt` 改写用户正文，session v2 将完整附件保存为 file part、公开 metadata 只保留摘要，恢复后 active pack 重建等价附件；text/file/image/folder 类型入口统一，receipt 只记录 attachment kind/count，不写正文；verified: `cargo test -p openagent-core` 通过（11 tests），`cargo test -p openagent-http-runtime` 通过（21 unit + 54 integration），覆盖正文与附件分离、file part 恢复、稳定 ID、provider payload、503 重试 payload 完全一致和 receipt redaction；format/diff/secret scan、core/protocol/session/http scoped strict clippy 通过，workspace strict clippy 仍被既有 LSP/MCP/turn/provider lint 阻塞；next: M2 typed todo + checkpoint/work state，统一 pending/resolved todo、compaction/checkpoint 边界和恢复来源。
  - 2026-07-16: changed: `ContextItem` 新增 message/tool_manifest/trace_only delivery contract，`ContextPackInput` 新增显式 skills 与 tool_manifests 来源，receipt 记录 delivery counts；Bridge 对 profile skills 只加载一次并生成稳定 typed source，MCP manifest 只从已注册且本轮可见的真实工具集合生成，provider messages 不重复注入 skill/MCP 内容；verified: `cargo test -p openagent-core` 通过（10 tests），`cargo test -p openagent-http-runtime` 通过（21 unit + 54 integration），MCP 两个 step 的 receipt tool names 与实际 HTTP tools 完全一致，subagent 预加载 skill typed receipt 可恢复，format/diff/core strict clippy/secret scan 通过；next: M2 typed attachments，保留附件类型、路径/媒体元数据和稳定 ID，移除 `build_run_prompt` 的附件文本拼接。
  - 2026-07-16: changed: Bridge/provider 主链从 trace-only shadow 切到 active `ContextPackBuilder`，provider adapter 只消费 pack messages/tools/model options；非 shadow pack 完整保留历史消息 metadata/tool calls；每个 run/step 在网络请求前写入可去重、最多 64 条的脱敏 `context.pack_built` event 与 `session.metadata.context_pack_receipts`，runtime 重启后可恢复；verified: `cargo test -p openagent-core` 通过（9 tests），`cargo test -p openagent-http-runtime` 通过（21 unit + 54 integration），503 重试两次 HTTP payload 完全一致，动态项目指令跨轮刷新且 receipt hash 更新，runtime 重启后 receipt history 保留，`cargo fmt --all -- --check`、`git diff --check`、core strict clippy 通过；HTTP strict clippy 仍被既有 MCP/turn/provider lint 阻塞；next: M2 将 attachments、skills、MCP manifests、todo、checkpoint/work state 作为 typed ContextItem 接入 active pack，并校验 tool manifests 与实际执行器一致。
  - 2026-07-16: changed: `ContextPack` 升级为 `openagent.context_pack.v1`，统一携带 messages、tool manifests、model options、items、trace、pack/provider-input hash 和脱敏 `openagent.context_pack_receipt.v1`；Bridge provider turn 每个 step 执行 trace-only shadow build，把 messages/tools/model options/hash 的 match/mismatch 与 receipt 持久化到 `session.metadata.context_pack_shadow`，不改变实际 provider 请求；verified: `cargo test -p openagent-core` 通过（8 tests），HTTP fake-provider shadow integration 通过，receipt redaction 覆盖 prompt/tool schema/model value/sandbox token，`cargo fmt --all -- --check` 与 `git diff --check` 通过；严格 workspace clippy 仍被既有 LSP/MCP/turn-runtime lint 阻塞，core `--no-deps` clippy 通过；next: M1 将 Bridge/provider 主链切到 builder，并把每个 turn 的 context receipt 作为正式 session artifact 持久化与恢复。
  - 2026-07-16: changed: 长期主线从已完成的 App/Core 解耦切换为统一 ContextPackBuilder，并建立 M0-M6 路线与最终验收标准；verified: 代码审计确认 `src/core.rs` 已有 trace-only builder 雏形，但 HTTP provider 主链仍走 `runtime_materialized_provider_messages_for_agent`，attachments 仍通过 `build_run_prompt` 文本化，统一接入尚未完成；next: M0 contract + shadow compare。
- next_recommended_slice: S6 - budget allocator；默认下一轮只完成分层预算、保留额度、确定性选择、overflow/fitting 与完整测试，完成后独立推送。
- blockers: none

## Resume Prompt

```text
[$goal-long-run] 继续推进 OpenHarness 统一 ContextPackBuilder 长期目标。读取 `/Users/william/coding/harness/openharness/.goal/state.md`，保持目标和 M0-M6 顺序不变；默认每轮只做一个高价值 slice，先声明 This slice / Will not do / Verify，完成后集中验证并只记录 changed / verified / next；不推 GitHub，除非我明确要求。
```
