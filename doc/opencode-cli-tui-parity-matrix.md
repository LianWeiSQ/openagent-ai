# OpenCode CLI/TUI Parity Matrix

This matrix is the audit ledger for bringing OpenAgent's local CLI and TUI
access layers to observable OpenCode parity. A single feature slice is not
"parity" unless every required row below is implemented, verified, linked to a
GitHub issue, and marked complete with evidence.

Reference source: `references/opencode` in the local harness workspace.

## Status Legend

| Status | Meaning |
| --- | --- |
| Supported | OpenAgent has the user-facing capability and tests/smoke evidence. |
| Partial | OpenAgent has adjacent or narrower behavior, but not OpenCode parity. |
| Missing | No OpenAgent user-facing capability exists yet. |
| Deferred | Explicitly accepted non-goal or lower-priority lifecycle behavior. |

| Priority | Meaning |
| --- | --- |
| P0 | Blocks credible CLI/TUI parity for daily coding-agent use. |
| P1 | Important operator workflow or high-frequency ergonomic gap. |
| P2 | Ecosystem, integration, or advanced workflow parity. |
| P3 | Low-level diagnostics or lifecycle parity; may become deferred by decision. |

## Completion Rules

1. Every row must keep an issue link.
2. Work starts only after the row issue is in progress or a narrower child issue
   is linked from the row.
3. A row can move to Supported only after the implementation is merged to
   `main`, the verification command is run, and completion evidence is recorded.
4. The goal is not complete while any P0/P1 row is Partial or Missing.
5. P2/P3 rows must either be Supported or explicitly Deferred with a recorded
   decision explaining why OpenAgent should not mirror OpenCode there.

## Current Baseline

OpenAgent is now Rust-only. The Python CLI/TUI/runtime tree referenced by older
receipts has been removed from `main`; compatibility is guarded by Rust crates,
compiled binary smoke tests, and the golden JSON fixtures under
`tests/golden/rust_rewrite/`.

- CLI entry: `cli` exposes the legacy OpenAgent command
  surface: `tui`, `serve`, `web`, `client`, `attach`, `run`, `session`,
  `models`, `stats`, `command`, `config`, `auth`, `providers`, `mcp`, and
  `doctor`.
- The Rust CLI now has binary smoke coverage for root/subcommand help,
  OpenCode-aligned `run` flags, JSON `run` events, `models`, `config`,
  `auth`/`providers`, and `mcp` file flows.
- TUI/App Bridge protocol/state contracts are owned by the Rust
  `runtime/tui`, `runtime/http`, and App Bridge client crates.
- Baseline tests: `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`, and focused
  `openagent-cli` binary smoke tests.

## 2026-07-04 Rust Capability Update

The older rows below were written while the Rust CLI was still restoring the
legacy command surface. That is no longer the current state. The compiled
`openagent` binary now routes the OpenCode-facing surface directly from
`cli/src/cli.rs`, including `agent`, `plugin`, `github`, `pr`, `debug`, `db`,
`upgrade`, `uninstall`, `acp`, top-level `import`/`export`, `generate`,
`console`, and `skills`.

Current Rust command surface:

- `run` exposes OpenCode-style flags for session continuation, fork/share,
  title, model/provider/variant, agent profile selection, remote attach,
  thinking output, and permission bypass.
- `agent` supports profile list/create/show/delete/run, built-in subagents,
  OpenCode Markdown agents, task permissions, skill config, preloaded skills,
  model options, and workspace isolation metadata.
- `plugin` supports local/module/remote registry install, list/show,
  enable/disable, remove, and manifest-backed dry-run command dispatch. It does
  not yet execute an npm/plugin runtime.
- `models` supports provider filtering, refresh, offline/catalog mode, verbose
  capability metadata, cache TTL, snapshot fallback, and provider-specific
  environment defaults.
- `mcp` supports local/remote config, auth token storage/status, debug/doctor,
  App Bridge lifecycle actions, and provider-loop MCP tool execution. Browser
  OAuth and dynamic client registration remain a P0 gap.
- `session` plus top-level `import`/`export` support list/export/import/share,
  checkpoints, restore, and delete.
- `approval`/`question` queues are exposed in CLI and App Bridge and are
  rendered/resolved by the Rust TUI and Desktop approval dock.
- The Rust TUI has session, file, model, agent, variant/thinking, theme, and
  color-scheme pickers; approval/question docks; diff/checkpoint renderers; and
  App Bridge attach/control coverage.

Remaining parity risk:

- Some OpenCode-equivalent commands are still local workflow scaffolds rather
  than full hosted integrations: plugin execution, GitHub agent install,
  well-known provider login, and packaging lifecycle commands.
- Subagent foreground execution, resume, nesting guards, workspace isolation,
  and HTTP background queueing exist, but CLI background task execution and a
  full wait/promote/cancel lifecycle are not yet OpenCode-level.
- Skill CLI/API observability is now closed for Step10: CLI skills golden,
  HTTP `/api/skills` golden, `skill.discovered`/`skill.loaded` session events,
  and compaction protection are covered. AgentProfile/SkillConfig/TaskConfig
  parsing is now shared, and the first `SessionRunnerFacade` layer now
  centralizes ToolContext construction, question-answer JSON parsing,
  `item/toolCall/*` event construction, tool-result message/projection payloads,
  skill session-event payloads, terminal turn event envelope construction, and
  shared usage/trace payload helpers plus terminal outcome state for CLI and
  HTTP. The next runtime risk is the still-duplicated provider-step/tool-call/task
  loop, which should move behind the same facade.
- HTTP provider catalog and fallback handling now keep catalog filtering
  separate from execution model selection: explicit session/profile models are
  preserved for provider calls while the model list can still curate supported
  display records. Broader provider login/catalog parity remains open.
- Long-running local TUI rendering is covered by Rust state/render snapshots and
  App Bridge tests; it is not yet a full terminal automation suite.

Current local verification anchors:

| Area | Capability | Evidence |
| --- | --- | --- |
| CLI command surface | Root/subcommand help, OpenCode `run` flags, JSON run/model output | `cli/tests/cli_commands.rs::binary_help_smoke_covers_legacy_command_surface` and `binary_run_and_models_smokes_are_machine_readable` |
| CLI agents/subagents | Built-in agents, OpenCode Markdown agents, task routing, workspace isolation | `cli/tests/cli_commands.rs::binary_agent_registry_exposes_builtin_subagents`, `binary_agent_registry_loads_opencode_markdown_agents`, and task-subagent tests |
| TUI controls | Session, file, model, agent, variant/thinking pickers; approval/question/diff docks | `runtime/tui/src/tests.rs` picker, interaction, render, and App Bridge tests |
| HTTP/App Bridge | Sessions, turns, approvals/questions, diff/checkpoint, MCP, agents, skills, task trees, provider catalog/fallback contract | `cargo test -p openagent-http-runtime --test http_runtime -q` covers the full HTTP runtime contract |
| Shared runner contract | Shared profile schema, ToolContext construction, question-answer parsing, tool-call events, tool-result projection, skill event payloads, terminal turn event envelopes, usage/trace payload helpers, and terminal outcome state | `cargo test -p openagent-tools -q`; `cargo test -p openagent-cli binary_approval_and_question_responses_resume_paused_runs --test cli_commands -q`; `cargo test -p openagent-http-runtime --test http_runtime -q`; `cargo check -p openagent-cli -p openagent-http-runtime` |

## CLI Matrix

| ID | Capability | OpenCode evidence | OpenAgent status | Gap | Priority | Issue | Verification command | Completion evidence |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| CLI-01 | `run` advanced flags | `packages/opencode/src/cli/cmd/run.ts` registers `--command`, `--continue`, `--session`, `--fork`, `--share`, `--model`, `--agent`, `--file`, `--format`, `--title`, `--attach`, `--dir`, `--variant`, `--thinking`, and `--dangerously-skip-permissions`. | Supported | Rust `run` exposes these flags and routes prompt/stdin/file/custom-command/session/fork/share/agent/attach/model options. Remaining risk is breadth of live-provider environments, not CLI surface. | P1 | [#41](https://github.com/LianWeiSQ/openagent-ai/issues/41) | `cargo test -p openagent-cli binary_help_smoke_covers_legacy_command_surface -q`; `cargo test -p openagent-cli binary_run_and_models_smokes_are_machine_readable -q` | Rust CLI help and JSON smoke tests cover the OpenCode flag surface and machine-readable run events. |
| CLI-02 | Remote App Bridge attach workflow | `packages/opencode/src/cli/cmd/tui/attach.ts` provides `attach <url>` with dir/session/continue/fork/auth options. | Supported | `openagent attach <url>` supports workspace/session/continue/fork, Bearer and Basic auth, App Bridge REST/SSE turns, interrupts, approvals/questions, and interactive `/sessions`, `/tasks`, `/task`, `/resume`, `/new`, `/fork`, `/interrupt`. | P1 | [#42](https://github.com/LianWeiSQ/openagent-ai/issues/42), [#62](https://github.com/LianWeiSQ/openagent-ai/issues/62), [#66](https://github.com/LianWeiSQ/openagent-ai/issues/66) | `cargo test -p openagent-http-runtime remote_runtime_client_round_trips_tui_approval -q` and `cargo test -p openagent-tui app_bridge_terminal_session_picker_searches_and_resumes -q` | App Bridge client and TUI attach tests cover auth, session selection, and remote interaction plumbing. |
| CLI-03 | MCP management commands | `packages/opencode/src/cli/cmd/mcp.ts` provides add/list/auth/logout/debug flows and remote OAuth support. | Partial | Local/remote config, auth token status/storage, debug/doctor, App Bridge lifecycle, and provider-loop MCP execution exist. Full browser OAuth flow and dynamic client registration remain open. | P0 | [#43](https://github.com/LianWeiSQ/openagent-ai/issues/43), [#65](https://github.com/LianWeiSQ/openagent-ai/issues/65), [#69](https://github.com/LianWeiSQ/openagent-ai/issues/69) | `cargo test -p openagent-cli binary_terminal_runs_remote_bridge_command -q`; `cargo test -p openagent-http-runtime remote_runtime_client_provider_loop_executes_mcp_tool -q` | Current MCP flows are implemented; row stays Partial until OAuth/dynamic-registration parity is complete. |
| CLI-04 | Provider-aware credentials | `packages/opencode/src/cli/cmd/providers.ts` implements provider login/list/logout, methods, and well-known provider behavior. | Partial | `auth`/`providers` support login/list/methods/logout, env-only discovery, redaction, provider defaults, and native Anthropic routing. Remaining gaps are security-reviewed well-known provider URL login and a fuller provider catalog/login model. | P0 | [#44](https://github.com/LianWeiSQ/openagent-ai/issues/44), [#67](https://github.com/LianWeiSQ/openagent-ai/issues/67), [#68](https://github.com/LianWeiSQ/openagent-ai/issues/68), [#70](https://github.com/LianWeiSQ/openagent-ai/issues/70), [#71](https://github.com/LianWeiSQ/openagent-ai/issues/71) | `cargo test -p openagent-cli binary_models_uses_provider_specific_model_environment -q`; `cargo test -p openagent-cli binary_run_uses_auth_file_provider_config_without_skip_doctor -q` | Provider-specific env/model and auth-file routing are covered; row remains Partial for catalog/login breadth. |
| CLI-05 | Refreshable verbose model listing | `packages/opencode/src/cli/cmd/models.ts` supports provider filtering, refresh, and verbose output. | Supported | Rust `models` supports provider filter, `--refresh`, `--offline`, `--catalog`, `--verbose`, TTL, explicit models URL, cache, and snapshot fallback. | P2 | [#45](https://github.com/LianWeiSQ/openagent-ai/issues/45) | `cargo test -p openagent-cli binary_models_catalog_and_backlog_commands_are_deep_local_workflows -q` | Model refresh/catalog/fallback and capability metadata are covered by CLI tests. |
| CLI-06 | Session import/share/export parity | `packages/opencode/src/cli/cmd/export.ts`, `import.ts`, and `run.ts --share` cover share/export/import workflows. | Supported | `session export/import/share`, top-level `import`/`export`, `run --share`, checkpoints, restore, and delete are exposed. | P1 | [#46](https://github.com/LianWeiSQ/openagent-ai/issues/46) | `cargo test -p openagent-cli binary_session_checkpoints_and_restore_revert_workspace_and_transcript -q` | Session restore/checkpoint tests cover the file/session side; import/export/share remain covered by CLI command fixture/golden. |
| CLI-07 | Plugin install and config registration | `packages/opencode/src/cli/cmd/plug.ts` installs npm plugins and mutates config. | Partial | `plugin install/list/show/enable/disable/remove/run` registers local/module/remote entries and reads manifests, but `run` is a dry-run registry dispatch and does not execute a plugin runtime. | P2 | [#47](https://github.com/LianWeiSQ/openagent-ai/issues/47) | `cargo test -p openagent-cli binary_models_catalog_and_backlog_commands_are_deep_local_workflows -q` | Registry/config slice exists; row stays Partial until plugin runtime execution/config boot is implemented. |
| CLI-08 | Reusable agent profile management | `packages/opencode/src/cli/cmd/agent.ts` supports creating/listing agents with mode, model, and permissions. | Supported | `agent list/create/show/delete/run` supports built-ins, project profiles, OpenCode Markdown agents, modes, permissions, model/options, task permissions, skills, and workspace isolation. | P2 | [#48](https://github.com/LianWeiSQ/openagent-ai/issues/48) | `cargo test -p openagent-cli binary_agent_registry_exposes_builtin_subagents -q`; `cargo test -p openagent-cli binary_agent_registry_loads_opencode_markdown_agents -q` | Agent registry, Markdown profiles, built-in subagents, and profile execution are covered by CLI tests. |
| CLI-09 | Server network parity and ACP mode | `packages/opencode/src/cli/cmd/serve.ts`, `cli/network.ts`, and `cli/cmd/acp.ts` expose hostname/port/mdns/cors and ACP server mode. | Partial | App Bridge serve/web/client and `acp manifest|serve` exist; HTTP runtime has CORS/mDNS flags. Remaining gap is full network discovery and ACP protocol breadth. | P2 | [#49](https://github.com/LianWeiSQ/openagent-ai/issues/49) | `cargo test -p openagent-http-runtime remote_runtime_client_models_and_agents_payloads -q` and `openagent acp manifest` | ACP manifest/server path exists; network parity is not yet full OpenCode breadth. |
| CLI-10 | GitHub agent and PR helpers | `packages/opencode/src/cli/cmd/github.ts` and `pr.ts` implement GitHub agent install/run and PR checkout/share import. | Partial | `github status/issue/pr/workflow` and `pr list/view/checkout/template/review` exist using `gh` and local workflow scaffolds. Missing OpenCode-level GitHub agent install/run and share import flows. | P2 | [#50](https://github.com/LianWeiSQ/openagent-ai/issues/50) | `cargo test -p openagent-cli binary_models_catalog_and_backlog_commands_are_deep_local_workflows -q` | Local workflow scaffold and PR helper surface exist; row stays Partial for full GitHub agent parity. |
| CLI-11 | Debug and session-store inspection | `packages/opencode/src/cli/cmd/db.ts` and `debug/snapshot.ts` expose database and snapshot diagnostics. | Partial | `debug info/paths/env/sessions/file/rg/bundle` and `db path/summary/rebuild/query/schema/export-sql` exist. OpenCode snapshot/debug parity is still broader than current file-ledger diagnostics. | P3 | [#51](https://github.com/LianWeiSQ/openagent-ai/issues/51) | `cargo test -p openagent-cli binary_models_catalog_and_backlog_commands_are_deep_local_workflows -q` | DB rebuild/query and debug bundle surfaces exist; row stays Partial for snapshot-level parity. |
| CLI-12 | Lifecycle commands | OpenCode docs expose `upgrade` and `uninstall` lifecycle commands. | Deferred | `upgrade`/`uninstall` return explicit source-tree-managed dry-run plans. Destructive lifecycle behavior is a packaging decision, not a local harness parity blocker. | P3 | [#52](https://github.com/LianWeiSQ/openagent-ai/issues/52) | `openagent upgrade --help`; `openagent uninstall --help` | Deferred until a packaged distribution owns upgrade/uninstall semantics. |
| CLI-13 | Skills CLI and diagnostics | `packages/opencode/src/cli/cmd/debug/skill.ts` and skill services expose skill discovery/debug workflows. | Supported | `openagent skills list/show/doctor`, HTTP `/api/skills`, model-invocable filtering, profile skill roots, permission hiding, `skill.discovered`/`skill.loaded` events, loaded-skill compaction protection, built-in skill discovery, workspace override, Claude frontmatter subset, and fork-skill task handoff are implemented. | P1 | Skill Step10 | `cargo test -p openagent-cli --test cli_commands -q`; `cargo test -p openagent-http-runtime --test http_runtime -q`; `cargo test -p openagent-session --test session_trace -q`; `cargo test -p openagent-tools -q` | Step10 closed in `e145353`: CLI skills golden, HTTP `/api/skills` golden, session skill events, and compaction protection are covered. Shared profile/schema extraction is closed in `4354027`; remaining skill-adjacent work is the shared SessionRunner loop, not missing Step10 behavior. |
| CLI-14 | Task/subagent routing from CLI run | `packages/opencode/src/tool/task` and prompt guidance encourage Task tool use for search and delegated work. | Partial | CLI run supports explicit Task tool calls, `@subagent` manual routing, description-based auto routing, nesting guards, permissions, resume, and workspace isolation. CLI background task execution is not implemented yet. | P0 | Subagent task parity | `cargo test -p openagent-cli binary_run_executes_task_subagent_tool -q`; `cargo test -p openagent-cli binary_run_auto_routes_prompt_to_matching_subagent_description -q`; `cargo test -p openagent-cli binary_run_executes_subagent_in_isolated_workspace -q` | Foreground/nested/isolation paths are covered; row stays Partial for background lifecycle parity. |

## TUI Matrix

| ID | Capability | OpenCode evidence | OpenAgent status | Gap | Priority | Issue | Verification command | Completion evidence |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| TUI-01 | Full session manager actions | `packages/opencode/src/cli/cmd/tui/app.tsx`, `dialog-session-list.tsx`, and `routes/session/index.tsx` support list/search/select/delete/rename/share/fork/compact/copy/child navigation. | Partial | Rust TUI now has session picker search/resume and management views/actions, but subagent child navigation and full OpenCode session-pane parity remain incomplete. | P1 | [#53](https://github.com/LianWeiSQ/openagent-ai/issues/53) | `cargo test -p openagent-tui app_bridge_terminal_session_picker_manages_real_session_actions -q` | Search/resume/manage slices are covered; row stays Partial for child/subagent navigation. |
| TUI-02 | Composer history, stash, and keymap interactions | `footer.prompt.tsx`, `runtime.queue.ts`, and `component/prompt/index.tsx` implement history navigation, slash parsing, prompt stash, and keybindings. | Partial | Rust TUI has history/stash state, slash command routing, picker controls, and key-event tests. Configurable leader/editor keymaps remain incomplete. | P1 | [#54](https://github.com/LianWeiSQ/openagent-ai/issues/54) | `cargo test -p openagent-tui key_event_flow_opens_file_picker_filters_and_attaches -q`; `cargo test -p openagent-tui key_event_flow_opens_session_picker_filters_and_resumes -q` | Core key flows are covered; row stays Partial for configurable keymap parity. |
| TUI-03 | Rich file and image attachments | `packages/app/src/components/prompt-input/attachments.ts`, `submit.ts`, and `footer.prompt.tsx` handle file/resource/image attachments and mentions. | Partial | Rust TUI supports `@file`, fuzzy picker insertion, line ranges, and image attachment classification. Paste/drop media and external resource attachments remain incomplete. | P1 | [#55](https://github.com/LianWeiSQ/openagent-ai/issues/55) | `cargo test -p openagent-tui composer_expands_file_line_ranges_and_image_attachments -q`; `cargo test -p openagent-tui composer_file_picker_and_attach_controls_insert_references -q` | File/image local attachment flows are covered; row stays Partial for paste/drop/resources. |
| TUI-04 | Rich approval dock with diff context | `footer.permission.tsx`, `permission.shared.ts`, and session permission routes support allow once, always allow, reject with note, and diff context. | Supported | Rust TUI and Desktop approval dock support allow once, allow always, deny with note, persisted history, and diff/preview context. | P0 | [#56](https://github.com/LianWeiSQ/openagent-ai/issues/56) | `cargo test -p openagent-tui approval_events_render_diff_preview_and_support_allow_always -q`; `cargo test -p openagent-tui approval_can_be_denied_with_note_from_command -q`; `npm --prefix desktop run smoke:approval-dock` | TUI unit tests and Desktop smoke cover approval dock UX and persistence. |
| TUI-05 | Question prompt flow | `footer.question.tsx` and `session-data.ts` manage question queues and replies. | Supported | Rust TUI and App Bridge support question queues, option/custom answers, reply, dismiss, persisted history, and live resume. | P2 | [#57](https://github.com/LianWeiSQ/openagent-ai/issues/57) | `cargo test -p openagent-tui question_events_support_answer_and_dismiss -q`; `cargo test -p openagent-tui key_event_flow_answers_question_option_from_dock -q` | Question dock flows are implemented and tested. |
| TUI-06 | Diff review and revert workflow | `routes/session/index.tsx` and permission routes render diffs, undo/redo, revert markers, and snapshots. | Supported | App Bridge exposes diff/undo/redo/checkpoints/restore; Rust TUI renders structured diff and markers; Desktop checkpoint restore UI has smoke coverage. | P0 | [#58](https://github.com/LianWeiSQ/openagent-ai/issues/58) | `cargo test -p openagent-tui patch_events_render_structured_diff_and_undo_redo_markers -q`; `npm --prefix desktop run smoke:checkpoint-restore-ui` | Diff, undo/redo markers, and Desktop restore flow are covered. |
| TUI-07 | Model, agent, and variant switcher | OpenCode registers model/agent/variant list/cycle/favorite commands in `tui/app.tsx` and `run/runtime.ts`. | Supported | Rust TUI has model picker, agent picker, variant picker, and thinking picker over App Bridge `/api/models` and `/api/agents`. | P0 | [#59](https://github.com/LianWeiSQ/openagent-ai/issues/59) | `cargo test -p openagent-tui app_bridge_terminal_model_picker_fetches_and_sets_model -q`; `cargo test -p openagent-tui app_bridge_terminal_agent_picker_fetches_and_sets_agent -q`; `cargo test -p openagent-tui app_bridge_terminal_variant_and_thinking_pickers_fetch_and_set -q` | Picker fetch/set flows are covered by Rust TUI tests. |
| TUI-08 | Interrupt feedback and cancellation states | `run/runtime.ts` and prompt/session routes expose abort/interrupt feedback. | Supported | OpenAgent supports cooperative interrupt and App Bridge turn cancellation; provider/tool boundary UX can still be polished. | P1 | [#60](https://github.com/LianWeiSQ/openagent-ai/issues/60) | `cargo test -p openagent-http-runtime remote_runtime_client_round_trips_tui_approval -q` plus focused interrupt tests | Interrupt support exists in App Bridge; UX polish remains a lower-risk follow-up. |
| TUI-09 | Session panes and subagent navigation | `runtime.lifecycle.ts`, `routes/session/index.tsx`, and TUI types expose panes, child sessions, and subagent tabs. | Partial | App Bridge exposes `/api/sessions/{id}/tasks` and attach mode has `/tasks`/`/task`; Rust TUI still lacks full subagent tab/pane navigation and plugin panes. | P2 | [#61](https://github.com/LianWeiSQ/openagent-ai/issues/61) | `cargo test -p openagent-http-runtime task_subagent_nested_tree_and_governance_guards -q` | Task tree data is covered; row stays Partial for TUI subagent pane UX. |
| TUI-10 | Interactive App Bridge attach | `cli/cmd/tui/attach.ts`, `run.ts --interactive --attach`, and server TUI-control routes support attaching a TUI to an existing server. | Supported | OpenAgent supports `openagent attach <url>` for a local curses TUI backed by App Bridge REST/SSE sessions, turns, interrupts, approvals, global `/api/events` replay, and the `/tui/*` control slice: authenticated append/submit/clear prompt controls, help/session opens, command execution, toast display, publish mapping, session selection, control polling, and control responses. Broader TUI model/theme/palette/plugin gaps remain tracked in their own rows. | P0 | [#62](https://github.com/LianWeiSQ/openagent-ai/issues/62), [#63](https://github.com/LianWeiSQ/openagent-ai/issues/63), [#66](https://github.com/LianWeiSQ/openagent-ai/issues/66) | `cargo test -p openagent-tui app_bridge_terminal_session_picker_searches_and_resumes -q`; `cargo test -p openagent-http-runtime remote_runtime_client_round_trips_tui_approval -q` | App Bridge attach/control path is implemented in Rust and covered by remote runtime/TUI tests. |
| TUI-11 | Curses TUI consumes App Bridge SSE | `stream.transport.ts`, `stream.ts`, and `session-data.ts` separate event transport from rendering. | Supported | OpenAgent exposes global App Bridge SSE at `/api/events` with stable `global_sequence` ids, `Last-Event-ID`/`last_sequence` replay, auth parity with other API/SSE routes, and remote TUI routing into matching turn records with turn-scoped fallback/dedupe. | P1 | [#63](https://github.com/LianWeiSQ/openagent-ai/issues/63) | `cargo test -p openagent-http-runtime remote_runtime_client_round_trips_tui_approval -q`; `cargo test -p openagent-tui app_bridge_terminal_transcript_reads_real_session_messages -q` | Rust App Bridge/TUI tests cover SSE-backed session and transcript consumption. |
| TUI-12 | Command palette, keymap, and plugin layer | `tui/app.tsx`, `keymap.tsx`, and `TuiPluginRuntime` provide command palette/keymaps/plugin routes. | Partial | Rust TUI has slash commands, remote control actions, and multiple pickers. It still lacks a full command palette abstraction, configurable keymap, and plugin pane/runtime slots. | P2 | [#64](https://github.com/LianWeiSQ/openagent-ai/issues/64) | `cargo test -p openagent-tui remote_control_open_models_dispatches_picker_fetch -q`; `cargo test -p openagent-tui remote_control_file_picker_dispatches_and_selects_into_composer -q` | Control/picker foundation exists; row stays Partial for palette/keymap/plugin parity. |

## P0 Implementation Queue

P0 rows are the first implementation tranche after this matrix:

1. Shared SessionRunner facade: the first facade layer now owns shared
   ToolContext construction, question-answer parsing, tool-call event
   construction, tool-result session projection, skill event payloads, and
   terminal turn event envelopes plus usage/trace payload helpers and terminal
   outcome state for CLI and HTTP. Continue moving provider calls, task
   handoff, skill loading, pending approval/question resume, and remaining
   session events into the common runner until the duplicated loops disappear.
2. [CLI-14 / Task background](#cli-matrix): complete background task state
   machine across queued/running/completed/failed/cancelled plus
   wait/promote/cancel/resume. HTTP has the queue foundation; CLI is still
   foreground-only for `background=true`.
3. [CLI-03 / #69](https://github.com/LianWeiSQ/openagent-ai/issues/69):
   full MCP browser OAuth flow and dynamic client registration.
4. [CLI-04 / #70/#71](https://github.com/LianWeiSQ/openagent-ai/issues/70):
   well-known provider login and broader provider catalog/login parity.
5. [TUI-09 / #61](https://github.com/LianWeiSQ/openagent-ai/issues/61):
   subagent task tree panes/tabs and child navigation in TUI/Desktop.

## Maintenance Checklist

For every future parity slice:

1. Update the row issue from backlog to in progress.
2. Implement on a dedicated branch named `codex/<row-id>-<short-title>`.
3. Run the row verification command and the nearest CLI/TUI regression tests.
4. Update this matrix row with completion evidence.
5. Push the branch, then have the main agent review and merge to `main`.
6. Close the issue only after `origin/main` contains the commit and evidence.
