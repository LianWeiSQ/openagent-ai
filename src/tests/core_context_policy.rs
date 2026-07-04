use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use openagent_core::{
    ContextItem, ContextPackBuildOptions, ContextPackBuilder, ContextPackInput,
    InstructionContextLoader, InstructionLoadOptions, PermissionManager, SkillRegistry,
    SkillRegistryOptions, check_context_budget, estimate_text_tokens, format_context_budget_error,
    load_context_budget_options, pattern_for, permission_rule,
};
use openagent_protocol::{
    ChatMessage, Model, PermissionAction, PermissionRuleset, Role, ToolSchema,
};
use serde::Serialize;
use serde_json::{Value, json};

#[test]
fn core_context_policy_fixture_matches_legacy_oracle() -> Result<(), Box<dyn Error>> {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../tests/golden/rust_rewrite/core_context_policy.json"
    ))?;
    assert_eq!(fixture, core_context_policy_fixture()?);
    Ok(())
}

#[test]
fn permission_manager_uses_last_matching_rule_and_payload_patterns() -> Result<(), Box<dyn Error>> {
    let mut manager = PermissionManager::new();
    manager.set_ruleset(PermissionRuleset::None);
    manager.add_rule(permission_rule(
        "skill",
        PermissionAction::Allow,
        Some("code-*"),
    ));
    manager.add_rule(permission_rule(
        "skill",
        PermissionAction::Deny,
        Some("code-secret"),
    ));

    assert_eq!(
        manager.decide(&json!({"name": "skill", "input": {"name": "code-review"}})),
        PermissionAction::Allow
    );
    assert_eq!(
        manager.decide(&json!({"name": "skill", "input": {"name": "code-secret"}})),
        PermissionAction::Deny
    );
    let denied = manager
        .check(&json!({"name": "skill", "input": {"name": "code-secret"}}))
        .expect_err("deny must block in check");
    assert!(denied.contains("Permission denied"));
    assert_eq!(pattern_for(&json!({"file_path": "a.txt"})), "a.txt");
    assert_eq!(
        manager.decide(&json!({"name": "bash", "input": {"command": "echo hi"}})),
        PermissionAction::Deny
    );
    Ok(())
}

#[test]
fn instruction_loader_and_skill_registry_cover_filesystem_workflows() -> Result<(), Box<dyn Error>>
{
    let root = setup_goal6_fixture_named("instruction")?;
    let workspace = root.join("repo/project/workspace");
    let user_dir = root.join("user");

    let instructions = InstructionContextLoader::new(
        &workspace,
        Some(InstructionLoadOptions {
            max_file_bytes: 8,
            max_total_bytes: 64,
            user_config_dir: Some(user_dir),
            ..InstructionLoadOptions::default()
        }),
    )
    .load();
    assert_eq!(instructions.items[0].display_path, "OPENAGENT.md");
    assert_eq!(instructions.items[0].content, "Workspac");
    assert!(instructions.truncated);
    assert!(
        instructions
            .issues
            .contains(&"truncated:OPENAGENT.md".to_string())
    );

    let registry = SkillRegistry::new_with_options(
        Some(&workspace),
        None,
        Some(root.join("home")),
        SkillRegistryOptions {
            include_builtin_skills: false,
        },
    );
    let report = registry.report(Some("review"), Some(5));
    assert_eq!(report.loaded_count, 2);
    assert_eq!(report.invalid_count, 1);
    assert_eq!(report.duplicate_count, 1);
    assert_eq!(report.skills[0].name, "code-review");
    assert_eq!(
        registry.search("external evidence", None)[0].name,
        "research"
    );
    Ok(())
}

#[test]
fn skill_registry_discovers_builtin_skills_and_workspace_overrides() -> Result<(), Box<dyn Error>> {
    let root = setup_goal6_fixture_named("builtin-skills")?;
    let workspace = root.join("repo/project/workspace");
    let empty_home = root.join("empty-home");
    fs::create_dir_all(&empty_home)?;

    let builtins = SkillRegistry::new_with_options(
        Some(&workspace),
        None,
        Some(&empty_home),
        SkillRegistryOptions {
            include_builtin_skills: true,
        },
    );
    assert!(
        builtins
            .get("openai-docs")
            .is_some_and(|skill| skill.location.contains("skill/openagent"))
    );

    write_skill(
        &workspace,
        ".openagent/skills/openai-docs/SKILL.md",
        "openai-docs",
        "Workspace override for OpenAI docs",
        "Workspace override wins.",
    )?;
    let overridden = SkillRegistry::new_with_options(
        Some(&workspace),
        None,
        Some(&empty_home),
        SkillRegistryOptions {
            include_builtin_skills: true,
        },
    );
    let loaded = overridden
        .get("openai-docs")
        .ok_or("missing overridden openai-docs skill")?;
    assert!(loaded.location.contains(".openagent/skills/openai-docs"));
    assert_eq!(loaded.content.trim(), "Workspace override wins.");

    let disabled = SkillRegistry::new_with_options(
        Some(&workspace),
        None,
        Some(&empty_home),
        SkillRegistryOptions {
            include_builtin_skills: false,
        },
    );
    assert!(
        disabled
            .all()
            .iter()
            .all(|skill| !skill.location.contains("skill/openagent"))
    );
    Ok(())
}

fn core_context_policy_fixture() -> Result<Value, Box<dyn Error>> {
    let root = setup_goal6_fixture()?;
    let workspace = root.join("repo/project/workspace");
    let user_dir = root.join("user");

    let model = Model {
        id: "context-fixture".to_string(),
        provider_id: "fixture".to_string(),
        name: "Context Fixture".to_string(),
        context_window: 96,
        max_output: 24,
        capabilities: Default::default(),
        pricing: Default::default(),
    };
    let budget_messages = vec![
        ChatMessage {
            role: Role::User,
            content: "find matches".to_string(),
            name: None,
            tool_call_id: None,
            metadata: BTreeMap::new(),
        },
        ChatMessage {
            role: Role::Tool,
            content: "x".repeat(1200),
            name: Some("code_search".to_string()),
            tool_call_id: None,
            metadata: BTreeMap::new(),
        },
    ];
    let budget_tools = vec![ToolSchema {
        name: "large_tool".to_string(),
        description: "A".repeat(120),
        schema: Some(json!({
            "type": "object",
            "properties": {"query": {"type": "string", "description": "B".repeat(80)}},
        })),
        group: "default".to_string(),
        dangerous: false,
    }];
    let budget_options = json!({"context_budget": {"strategy": "compact", "bytes_per_token": 4}});
    let budget_result = check_context_budget(
        Some("You are helpful."),
        &budget_messages,
        &budget_tools,
        Some(&model),
        Some(&budget_options),
        "goal6",
    )?
    .ok_or("budget result missing")?;

    let invalid_strategy = load_context_budget_options(
        Some(&json!({"context_budget": {"strategy": ""}})),
        Some(&model),
    )
    .err()
    .ok_or("invalid strategy unexpectedly passed")?;
    let invalid_compaction =
        load_context_budget_options(Some(&json!({"compaction": {"auto": "yes"}})), Some(&model))
            .err()
            .ok_or("invalid compaction unexpectedly passed")?;

    let context_pack = ContextPackBuilder::new(Some(ContextPackBuildOptions {
        token_budget: Some(24),
        bytes_per_token: 4,
        trace_only: true,
    }))
    .build(ContextPackInput {
        messages: vec![
            chat(Role::User, "old request"),
            ChatMessage {
                role: Role::Tool,
                content: "grep preview".to_string(),
                name: Some("grep".to_string()),
                tool_call_id: Some("call-grep".to_string()),
                metadata: BTreeMap::new(),
            },
            chat(Role::User, "new request"),
        ],
        metadata: BTreeMap::from([
            (
                "context_compaction".to_string(),
                json!({
                    "schema_version": 1,
                    "format": "structured_work_state",
                    "state": {"task": "Continue Rust rewrite", "next_steps": ["Port context"]},
                    "summary": "ignored",
                    "compacted_until": 2,
                    "updated_at": 1781841000000u64,
                }),
            ),
            (
                "execution".to_string(),
                json!({
                    "mode": "opensandbox",
                    "sandbox_id": "sbx_fixture",
                    "remote_workdir": "/workspace/project",
                    "connection": {"token": "secret"},
                }),
            ),
        ]),
        todos: vec![json!({"content": "port context", "status": "in_progress", "priority": "high", "id": "todo-context"})],
        runtime_context: Some("[Runtime]\nGoal 6 fixture".to_string()),
        sandbox_metadata: None,
        extra_items: vec![
            ContextItem::new("diag", "diagnostic", "fixture", "low", 1),
            ContextItem::new("diag", "diagnostic", "fixture", "high", 9),
        ],
    });

    let instructions = InstructionContextLoader::new(
        &workspace,
        Some(InstructionLoadOptions {
            max_file_bytes: 8,
            max_total_bytes: 64,
            user_config_dir: Some(user_dir),
            ..InstructionLoadOptions::default()
        }),
    )
    .load();
    let instruction_context_items = instructions.to_context_items();

    let registry = SkillRegistry::new_with_options(
        Some(&workspace),
        None,
        Some(root.join("home")),
        SkillRegistryOptions {
            include_builtin_skills: false,
        },
    );
    let report = registry.report(Some("review"), Some(5));
    let loaded = registry
        .get("code-review")
        .ok_or("missing code-review skill")?;

    let payload = json!({
        "schema_version": 1,
        "permission": permission_decisions()?,
        "context_budget": {
            "config": to_value(load_context_budget_options(
                Some(&json!({
                    "compaction": {"auto": false, "prune": false, "reserved": 16},
                    "context_budget": {"strategy": "compact", "input_safety_margin_tokens": 8},
                })),
                Some(&model),
            )?)?,
            "result": to_value(&budget_result)?,
            "error": format_context_budget_error(&budget_result),
            "invalid_strategy": invalid_strategy,
            "invalid_compaction": invalid_compaction,
        },
        "context_pack": {
            "estimated_input_tokens": context_pack.estimated_input_tokens,
            "items": context_pack.items.iter().map(context_item_fixture).collect::<Vec<_>>(),
            "trace": to_value(&context_pack.trace)?,
            "estimate_text_tokens": estimate_text_tokens("abcd", 3),
        },
        "instructions": {
            "total_bytes": instructions.total_bytes,
            "truncated": instructions.truncated,
            "issues": instructions.issues,
            "items": instructions.items.iter().map(instruction_item_fixture).collect::<Vec<_>>(),
            "context_items": instruction_context_items.iter().map(instruction_context_item_fixture).collect::<Vec<_>>(),
        },
        "skills": {
            "report": {
                "skill_count": report.skills.len(),
                "loaded_count": report.loaded_count,
                "scanned_files": report.scanned_files,
                "invalid_count": report.invalid_count,
                "duplicate_count": report.duplicate_count,
                "skills": to_value(&report.skills)?,
                "issues": report.issues.iter().map(skill_issue_summary).collect::<Vec<_>>(),
            },
            "loaded": to_value(&loaded)?,
            "search_all": to_value(registry.search("external evidence", None))?,
        },
    });
    Ok(scrub_fixture_root(payload, &root))
}

fn permission_decisions() -> Result<Value, Box<dyn Error>> {
    let mut readonly = PermissionManager::new();
    readonly.set_ruleset(PermissionRuleset::Readonly);
    let mut plan_only = PermissionManager::new();
    plan_only.set_ruleset(PermissionRuleset::PlanOnly);
    let mut custom = PermissionManager::new();
    custom.set_ruleset(PermissionRuleset::None);
    custom.add_rule(permission_rule(
        "skill",
        PermissionAction::Allow,
        Some("code-review"),
    ));
    Ok(json!({
        "readonly_write": readonly.decide(&json!({"name": "write", "input": {"file_path": "a.txt", "content": "x"}})),
        "readonly_ls": readonly.decide(&json!({"name": "ls", "input": {}})),
        "readonly_skill": readonly.decide(&json!({"name": "skill", "input": {"name": "code-review"}})),
        "readonly_todowrite": readonly.decide(&json!({"name": "todowrite", "input": {"todos": []}})),
        "plan_only_todowrite": plan_only.decide(&json!({"name": "todowrite", "input": {"todos": []}})),
        "custom_skill": custom.decide(&json!({"name": "skill", "input": {"name": "code-review"}})),
        "pattern_for_file": pattern_for(&json!({"file_path": "src/core.rs", "command": "ignored"})),
        "pattern_for_name": pattern_for(&json!({"name": "code-review"})),
        "pattern_for_json": pattern_for(&json!({"b": 2, "a": 1})),
    }))
}

fn setup_goal6_fixture() -> Result<PathBuf, Box<dyn Error>> {
    setup_goal6_fixture_at(PathBuf::from("/tmp/openagent-rust-rewrite-fixture-goal6"))
}

fn setup_goal6_fixture_named(name: &str) -> Result<PathBuf, Box<dyn Error>> {
    setup_goal6_fixture_at(PathBuf::from(format!(
        "/tmp/openagent-rust-rewrite-fixture-goal6-{name}"
    )))
}

fn setup_goal6_fixture_at(root: PathBuf) -> Result<PathBuf, Box<dyn Error>> {
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    let workspace = root.join("repo/project/workspace");
    let user_dir = root.join("user");
    fs::create_dir_all(&workspace)?;
    fs::create_dir_all(&user_dir)?;
    fs::write(root.join("repo/AGENTS.md"), "Parent instruction")?;
    fs::write(workspace.join("OPENAGENT.md"), "Workspace rule")?;
    fs::create_dir_all(workspace.join(".openagent/rules"))?;
    fs::write(workspace.join(".openagent/rules/b.md"), "Rule B")?;
    fs::write(workspace.join(".openagent/rules/a.md"), "Rule A")?;
    fs::write(user_dir.join("OPENAGENT.md"), "User instruction")?;

    write_skill(
        &workspace,
        ".openagent/skills/code-review/SKILL.md",
        "code-review",
        "Review code carefully",
        "Inspect diffs and tests.",
    )?;
    write_skill(
        &workspace,
        ".openagent/skills/research/SKILL.md",
        "research",
        "Research external sources",
        "Collect evidence.",
    )?;
    write_skill(
        &workspace,
        ".claude/skills/code-review/SKILL.md",
        "code-review",
        "duplicate",
        "Duplicate should not win.",
    )?;
    let broken = workspace.join(".openagent/skills/broken/SKILL.md");
    if let Some(parent) = broken.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(broken, "# no frontmatter\n")?;
    Ok(root)
}

fn write_skill(
    base: &Path,
    relative: &str,
    name: &str,
    description: &str,
    body: &str,
) -> Result<(), Box<dyn Error>> {
    let path = base.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        path,
        format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"),
    )?;
    Ok(())
}

fn chat(role: Role, content: &str) -> ChatMessage {
    ChatMessage {
        role,
        content: content.to_string(),
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::new(),
    }
}

fn context_item_fixture(item: &ContextItem) -> Value {
    json!({
        "id": item.id,
        "kind": item.kind,
        "source": item.source,
        "content": item.content,
        "priority": item.priority,
        "token_estimate": item.token_estimate,
        "pinned": item.pinned,
        "stable_prefix": item.stable_prefix,
        "metadata": item.metadata,
    })
}

fn instruction_item_fixture(item: &openagent_core::InstructionItem) -> Value {
    json!({
        "display_path": item.display_path,
        "source": item.source,
        "scope": item.scope,
        "content": item.content,
        "bytes_read": item.bytes_read,
        "truncated": item.truncated,
    })
}

fn instruction_context_item_fixture(item: &ContextItem) -> Value {
    json!({
        "kind": item.kind,
        "source": item.source,
        "content": item.content,
        "priority": item.priority,
        "pinned": item.pinned,
        "stable_prefix": item.stable_prefix,
        "metadata": item.metadata,
    })
}

fn skill_issue_summary(issue: &openagent_core::SkillIssue) -> Value {
    json!({
        "kind": issue.kind,
        "path": Path::new(&issue.path).file_name().and_then(|name| name.to_str()).unwrap_or_default(),
        "duplicate_of": issue.duplicate_of.as_ref().and_then(|path| {
            Path::new(path).file_name().and_then(|name| name.to_str()).map(str::to_string)
        }),
    })
}

fn scrub_fixture_root(value: Value, root: &Path) -> Value {
    let stable = "/tmp/openagent-rust-rewrite-fixture-goal6";
    let mut replacements = vec![(root.to_string_lossy().to_string(), stable.to_string())];
    if let Ok(resolved) = root.canonicalize() {
        replacements.push((resolved.to_string_lossy().to_string(), stable.to_string()));
    }
    replacements.push((format!("/private{stable}"), stable.to_string()));
    scrub_value(value, &replacements)
}

fn scrub_value(value: Value, replacements: &[(String, String)]) -> Value {
    match value {
        Value::String(text) => {
            let mut result = text;
            for (needle, replacement) in replacements {
                result = result.replace(needle, replacement);
            }
            Value::String(result)
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| scrub_value(item, replacements))
                .collect(),
        ),
        Value::Object(items) => Value::Object(
            items
                .into_iter()
                .map(|(key, value)| (key, scrub_value(value, replacements)))
                .collect(),
        ),
        other => other,
    }
}

fn to_value<T: Serialize>(value: T) -> Result<Value, serde_json::Error> {
    serde_json::to_value(value)
}
