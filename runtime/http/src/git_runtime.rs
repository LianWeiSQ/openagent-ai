use super::*;
use std::{
    ffi::OsStr,
    path::{Component, Path},
};

const GIT_WORKFLOW_KEY: &str = "git_workflow";
const GIT_WORKFLOW_SCHEMA: &str = "openagent.git_workflow.v1";
const GIT_WORKFLOW_SOURCE: &str = "git_workflow";
const MAX_GIT_WORKFLOW_PATHS: usize = 200;
const MAX_PR_BODY_CHARS: usize = 20_000;

pub(super) fn git_workflow_payload(
    config: &HttpRuntimeConfig,
    request_path: &str,
) -> Result<Value, String> {
    let session_id = required_session_query(request_path)?;
    let store = FileSessionStore::new(session_root(config));
    let session = store
        .load_session(&session_id)
        .map_err(|error| error.to_string())?;
    let git = git_payload(config, request_path)?;
    let state = session
        .metadata
        .get(GIT_WORKFLOW_KEY)
        .cloned()
        .unwrap_or_else(|| json!({}));
    let pending = session
        .metadata
        .get("pending_approval")
        .filter(|approval| {
            approval.get("source").and_then(Value::as_str) == Some(GIT_WORKFLOW_SOURCE)
        })
        .cloned();
    let branch = git
        .get("branch")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let base_branch = state
        .get("base_branch")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| default_base_branch(&session.directory, branch));
    let head_sha = git_try(&session.directory, &["rev-parse", "HEAD"])
        .unwrap_or_default()
        .trim()
        .to_string();
    let head_subject = git_try(&session.directory, &["log", "-1", "--pretty=%s"])
        .unwrap_or_default()
        .trim()
        .to_string();

    Ok(json!({
        "schema_version": GIT_WORKFLOW_SCHEMA,
        "session_id": session.id,
        "workspace": session.directory.to_string_lossy(),
        "is_repo": git.get("is_repo").cloned().unwrap_or(json!(false)),
        "branch": branch,
        "base_branch": base_branch,
        "head_sha": head_sha,
        "head_subject": head_subject,
        "ahead": git.get("ahead").cloned().unwrap_or_else(|| json!(0)),
        "behind": git.get("behind").cloned().unwrap_or_else(|| json!(0)),
        "changes": git.get("changes").cloned().unwrap_or_else(|| json!([])),
        "change_count": git.get("change_count").cloned().unwrap_or_else(|| json!(0)),
        "summary": state.get("summary").cloned().unwrap_or(Value::Null),
        "handoff": state.get("handoff").cloned().unwrap_or(Value::Null),
        "last_result": state.get("last_result").cloned().unwrap_or(Value::Null),
        "pending": pending,
        "capabilities": {
            "create_branch": true,
            "commit_selected": true,
            "generate_pr_summary": true,
            "github_review_handoff": true,
            "writes_require_approval": true,
        },
    }))
}

pub(super) fn generate_git_workflow_summary(
    config: &HttpRuntimeConfig,
    body: &str,
) -> Result<Value, String> {
    let payload = parse_object(body)?;
    let session_id = required_string(&payload, "session_id")?;
    let store = FileSessionStore::new(session_root(config));
    let mut session = store
        .load_session(&session_id)
        .map_err(|error| error.to_string())?;
    ensure_git_repo(&session.directory)?;
    let current_branch = current_branch(&session.directory)?;
    let existing_state = workflow_state(&session);
    let base_branch = optional_string(&payload, "base_branch")
        .or_else(|| object_string(&existing_state, "base_branch"))
        .unwrap_or_else(|| default_base_branch(&session.directory, &current_branch));
    validate_branch(&session.directory, &base_branch)?;
    let custom_title = optional_string(&payload, "title");
    let summary = build_pr_summary(
        &session.directory,
        &base_branch,
        &current_branch,
        custom_title.as_deref(),
    )?;
    let mut state = existing_state;
    state.insert("schema_version".to_string(), json!(GIT_WORKFLOW_SCHEMA));
    state.insert("base_branch".to_string(), json!(base_branch));
    state.insert("summary".to_string(), summary.clone());
    state.insert("updated_at_ms".to_string(), json!(now_ms()));
    session
        .metadata
        .insert(GIT_WORKFLOW_KEY.to_string(), Value::Object(state));
    store
        .save_state(&session, None)
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "schema_version": GIT_WORKFLOW_SCHEMA,
        "session_id": session.id,
        "status": "ready",
        "summary": summary,
    }))
}

pub(super) fn request_git_workflow_action(
    config: &HttpRuntimeConfig,
    body: &str,
) -> Result<Value, String> {
    let payload = parse_object(body)?;
    let session_id = required_string(&payload, "session_id")?;
    let workflow_action = required_string(&payload, "action")?;
    let store = FileSessionStore::new(session_root(config));
    let mut session = store
        .load_session(&session_id)
        .map_err(|error| error.to_string())?;
    ensure_git_repo(&session.directory)?;
    if session.metadata.contains_key("pending_approval") {
        return Err("resolve the current approval before starting another Git action".to_string());
    }

    let input = validated_action_input(&session, &workflow_action, &payload)?;
    let request_id = new_id("approval_git");
    let turn_id = new_id("git_workflow");
    let (tool_name, label, permission_pattern) = action_identity(&workflow_action)?;
    let approval = json!({
        "request_id": request_id,
        "turn_id": turn_id,
        "run_id": turn_id,
        "source": GIT_WORKFLOW_SOURCE,
        "workflow_action": workflow_action,
        "tool_name": tool_name,
        "input": input,
        "preview": {
            "path": session.directory.to_string_lossy(),
            "label": label,
        },
        "reason": "permission_required",
        "permission_action": "ask",
        "permission_pattern": permission_pattern,
        "created_at_ms": now_ms(),
    });
    session
        .metadata
        .insert("pending_approval".to_string(), approval.clone());
    let mut state = workflow_state(&session);
    state.insert("schema_version".to_string(), json!(GIT_WORKFLOW_SCHEMA));
    state.insert(
        "pending_action".to_string(),
        json!({
            "request_id": request_id,
            "action": workflow_action,
            "label": label,
            "created_at_ms": now_ms(),
        }),
    );
    state.insert("updated_at_ms".to_string(), json!(now_ms()));
    session
        .metadata
        .insert(GIT_WORKFLOW_KEY.to_string(), Value::Object(state));
    store
        .save_state(&session, Some(&turn_id))
        .map_err(|error| error.to_string())?;
    let _ = store.record_event(
        &session.id,
        &turn_id,
        "approval.requested",
        SessionEventOptions {
            kind: "approval".to_string(),
            status: "pending".to_string(),
            attributes: BTreeMap::from([
                ("request_id".to_string(), json!(request_id)),
                ("source".to_string(), json!(GIT_WORKFLOW_SOURCE)),
                ("workflow_action".to_string(), json!(workflow_action)),
            ]),
            ..SessionEventOptions::default()
        },
    );
    let mut events = vec![json!({
        "method": "turn/approval_requested",
        "params": {
            "session_id": session.id,
            "thread_id": session.id,
            "turn_id": turn_id,
            "request_id": request_id,
            "status": "waiting_approval",
            "approval": approval,
        }
    })];
    append_bridge_events(&store.root, &session.id, &turn_id, &mut events);
    Ok(json!({
        "schema_version": GIT_WORKFLOW_SCHEMA,
        "session_id": session.id,
        "turn_id": turn_id,
        "request_id": request_id,
        "status": "waiting_approval",
        "approval": approval,
        "events": events,
    }))
}

pub(super) fn resolve_git_workflow_approval(
    store: &FileSessionStore,
    mut session: Session,
    approval: Value,
    response: &Value,
) -> Result<Value, String> {
    let request_id = approval
        .get("request_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let turn_id = approval
        .get("turn_id")
        .or_else(|| approval.get("run_id"))
        .and_then(Value::as_str)
        .unwrap_or("git_workflow")
        .to_string();
    let workflow_action = approval
        .get("workflow_action")
        .and_then(Value::as_str)
        .ok_or_else(|| "Git approval is missing workflow_action".to_string())?
        .to_string();
    let action = response
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("deny");
    session.metadata.remove("pending_approval");
    let mut state = workflow_state(&session);
    state.remove("pending_action");

    let (status, result) = if action == "allow" {
        match execute_git_workflow_action(
            &session.directory,
            &workflow_action,
            approval.get("input").unwrap_or(&Value::Null),
            OsStr::new("gh"),
        ) {
            Ok(result) => ("completed", result),
            Err(error) => (
                "failed",
                json!({
                    "action": workflow_action,
                    "status": "failed",
                    "error": error,
                    "updated_at_ms": now_ms(),
                }),
            ),
        }
    } else {
        (
            "denied",
            json!({
                "action": workflow_action,
                "status": "denied",
                "error": "approval denied",
                "updated_at_ms": now_ms(),
            }),
        )
    };
    if workflow_action == "create_branch" && status == "completed" {
        if let Some(base_branch) = result.get("base_branch").cloned() {
            state.insert("base_branch".to_string(), base_branch);
        }
        state.remove("summary");
        state.remove("handoff");
    }
    if workflow_action == "commit" && status == "completed" {
        state.remove("summary");
        state.remove("handoff");
    }
    if workflow_action == "create_pr" && status == "completed" {
        state.insert("handoff".to_string(), result.clone());
    }
    state.insert("last_result".to_string(), result.clone());
    state.insert("updated_at_ms".to_string(), json!(now_ms()));
    session
        .metadata
        .insert(GIT_WORKFLOW_KEY.to_string(), Value::Object(state));
    store
        .save_state(&session, Some(&turn_id))
        .map_err(|error| error.to_string())?;
    let _ = store.record_event(
        &session.id,
        &turn_id,
        "approval.resolved",
        SessionEventOptions {
            kind: "approval".to_string(),
            status: status.to_string(),
            attributes: BTreeMap::from([
                ("request_id".to_string(), json!(request_id)),
                ("source".to_string(), json!(GIT_WORKFLOW_SOURCE)),
                ("workflow_action".to_string(), json!(workflow_action)),
            ]),
            ..SessionEventOptions::default()
        },
    );
    let mut events = vec![
        json!({
            "method": "turn/approval_resolved",
            "params": {
                "session_id": session.id,
                "thread_id": session.id,
                "turn_id": turn_id,
                "request_id": request_id,
                "status": status,
                "approval": approval,
            }
        }),
        json!({
            "method": "git/workflow_updated",
            "params": {
                "session_id": session.id,
                "turn_id": turn_id,
                "request_id": request_id,
                "status": status,
                "action": workflow_action,
                "result": result,
            }
        }),
    ];
    append_bridge_events(&store.root, &session.id, &turn_id, &mut events);
    Ok(json!({
        "schema_version": GIT_WORKFLOW_SCHEMA,
        "session_id": session.id,
        "turn_id": turn_id,
        "request_id": request_id,
        "status": status,
        "action": workflow_action,
        "result": result,
        "events": events,
    }))
}

fn execute_git_workflow_action(
    root: &Path,
    action: &str,
    input: &Value,
    gh_binary: &OsStr,
) -> Result<Value, String> {
    match action {
        "create_branch" => {
            let branch = required_value_string(input, "branch")?;
            validate_branch(root, &branch)?;
            let base_branch = current_branch(root)?;
            git(
                root,
                &["switch".to_string(), "-c".to_string(), branch.clone()],
            )?;
            Ok(json!({
                "action": action,
                "status": "completed",
                "branch": branch,
                "base_branch": base_branch,
                "updated_at_ms": now_ms(),
            }))
        }
        "commit" => {
            let message = required_value_string(input, "message")?;
            let paths = value_paths(input)?;
            let mut add_args = vec!["add".to_string(), "--all".to_string(), "--".to_string()];
            add_args.extend(paths.iter().cloned());
            git(root, &add_args)?;
            let mut commit_args = vec![
                "commit".to_string(),
                "--only".to_string(),
                "-m".to_string(),
                message.clone(),
                "--".to_string(),
            ];
            commit_args.extend(paths.iter().cloned());
            git(root, &commit_args)?;
            let sha = git(root, &["rev-parse".to_string(), "HEAD".to_string()])?
                .trim()
                .to_string();
            Ok(json!({
                "action": action,
                "status": "completed",
                "commit": sha,
                "message": message,
                "paths": paths,
                "updated_at_ms": now_ms(),
            }))
        }
        "create_pr" => {
            let title = required_value_string(input, "title")?;
            let body = required_value_string(input, "body")?;
            let base_branch = required_value_string(input, "base_branch")?;
            let head_branch = required_value_string(input, "head_branch")?;
            validate_branch(root, &base_branch)?;
            validate_branch(root, &head_branch)?;
            git(
                root,
                &[
                    "push".to_string(),
                    "--set-upstream".to_string(),
                    "origin".to_string(),
                    head_branch.clone(),
                ],
            )?;
            let mut args = vec![
                "pr".to_string(),
                "create".to_string(),
                "--title".to_string(),
                title.clone(),
                "--body".to_string(),
                body,
                "--base".to_string(),
                base_branch.clone(),
                "--head".to_string(),
                head_branch.clone(),
            ];
            if input.get("draft").and_then(Value::as_bool).unwrap_or(false) {
                args.push("--draft".to_string());
            }
            let output = external(root, gh_binary, &args, "GitHub CLI")?;
            let url = output
                .lines()
                .rev()
                .find(|line| {
                    line.trim().starts_with("http://") || line.trim().starts_with("https://")
                })
                .map(str::trim)
                .unwrap_or_default()
                .to_string();
            if url.is_empty() {
                return Err("GitHub CLI completed without returning a pull request URL".to_string());
            }
            Ok(json!({
                "action": action,
                "status": "completed",
                "title": title,
                "base_branch": base_branch,
                "head_branch": head_branch,
                "url": url,
                "updated_at_ms": now_ms(),
            }))
        }
        _ => Err(format!("unsupported Git workflow action: {action}")),
    }
}

fn validated_action_input(
    session: &Session,
    action: &str,
    payload: &Map<String, Value>,
) -> Result<Value, String> {
    match action {
        "create_branch" => {
            let branch = required_string(payload, "branch")?;
            validate_branch(&session.directory, &branch)?;
            if branch == current_branch(&session.directory)? {
                return Err("the requested branch is already checked out".to_string());
            }
            Ok(json!({"branch": branch}))
        }
        "commit" => {
            let message = required_string(payload, "message")?;
            if message.chars().count() > 500 {
                return Err("commit message must be 500 characters or fewer".to_string());
            }
            let paths = map_paths(payload)?;
            Ok(json!({"message": message, "paths": paths}))
        }
        "create_pr" => {
            let title = required_string(payload, "title")?;
            let body = required_string(payload, "body")?;
            if title.chars().count() > 256 {
                return Err("pull request title must be 256 characters or fewer".to_string());
            }
            if body.chars().count() > MAX_PR_BODY_CHARS {
                return Err(format!(
                    "pull request body must be {MAX_PR_BODY_CHARS} characters or fewer"
                ));
            }
            let base_branch = required_string(payload, "base_branch")?;
            let head_branch = current_branch(&session.directory)?;
            if base_branch == head_branch {
                return Err(
                    "create or switch to a feature branch before handing off a review".to_string(),
                );
            }
            validate_branch(&session.directory, &base_branch)?;
            git(
                &session.directory,
                &[
                    "remote".to_string(),
                    "get-url".to_string(),
                    "origin".to_string(),
                ],
            )?;
            Ok(json!({
                "title": title,
                "body": body,
                "base_branch": base_branch,
                "head_branch": head_branch,
                "draft": payload.get("draft").and_then(Value::as_bool).unwrap_or(true),
            }))
        }
        _ => Err(format!("unsupported Git workflow action: {action}")),
    }
}

fn build_pr_summary(
    root: &Path,
    base_branch: &str,
    head_branch: &str,
    custom_title: Option<&str>,
) -> Result<Value, String> {
    let base_ref = resolve_base_ref(root, base_branch)?;
    let committed = git_try(
        root,
        &["diff", "--numstat", &format!("{base_ref}...HEAD"), "--"],
    )
    .unwrap_or_default();
    let working = git_try(root, &["diff", "--numstat", "HEAD", "--"]).unwrap_or_default();
    let mut stats = BTreeMap::<String, (u64, u64, bool)>::new();
    merge_numstat(&mut stats, &committed);
    merge_numstat(&mut stats, &working);
    for path in git_try(root, &["ls-files", "--others", "--exclude-standard"])
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        let target = root.join(path);
        let additions = fs::read_to_string(target)
            .map(|content| content.lines().count() as u64)
            .unwrap_or_default();
        stats
            .entry(path.to_string())
            .or_insert((additions, 0, false));
    }
    let additions = stats.values().map(|item| item.0).sum::<u64>();
    let deletions = stats.values().map(|item| item.1).sum::<u64>();
    let commits = git_try(root, &["log", "--format=%s", &format!("{base_ref}..HEAD")])
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(20)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let latest_subject = git_try(root, &["log", "-1", "--pretty=%s"])
        .unwrap_or_default()
        .trim()
        .to_string();
    let title = custom_title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            commits
                .first()
                .cloned()
                .filter(|value| !value.is_empty())
                .or_else(|| (!latest_subject.is_empty()).then_some(latest_subject))
                .unwrap_or_else(|| format!("Update {head_branch}"))
        });
    let mut body = vec!["## Summary".to_string()];
    if commits.is_empty() {
        body.push("- Prepare the current workspace changes for review.".to_string());
    } else {
        body.extend(commits.iter().map(|commit| format!("- {commit}")));
    }
    body.push(String::new());
    body.push("## Changes".to_string());
    if stats.is_empty() {
        body.push("- No file changes detected against the selected base branch.".to_string());
    } else {
        body.extend(
            stats
                .iter()
                .take(80)
                .map(|(path, (added, removed, binary))| {
                    if *binary {
                        format!("- `{path}` (binary)")
                    } else {
                        format!("- `{path}` (+{added} -{removed})")
                    }
                }),
        );
    }
    body.push(String::new());
    body.push("## Verification".to_string());
    body.push(
        "- Review the attached diff and recorded OpenAgent verification evidence.".to_string(),
    );
    let generated_at_ms = now_ms();
    Ok(json!({
        "title": title,
        "body": body.join("\n"),
        "base_branch": base_branch,
        "head_branch": head_branch,
        "file_count": stats.len(),
        "additions": additions,
        "deletions": deletions,
        "commits": commits,
        "generated_at_ms": generated_at_ms,
    }))
}

fn merge_numstat(stats: &mut BTreeMap<String, (u64, u64, bool)>, raw: &str) {
    for line in raw.lines() {
        let mut fields = line.splitn(3, '\t');
        let Some(added) = fields.next() else { continue };
        let Some(removed) = fields.next() else {
            continue;
        };
        let Some(path) = fields.next() else { continue };
        let binary = added == "-" || removed == "-";
        let entry = stats.entry(path.to_string()).or_insert((0, 0, binary));
        entry.0 = entry
            .0
            .saturating_add(added.parse::<u64>().unwrap_or_default());
        entry.1 = entry
            .1
            .saturating_add(removed.parse::<u64>().unwrap_or_default());
        entry.2 |= binary;
    }
}

fn resolve_base_ref(root: &Path, base_branch: &str) -> Result<String, String> {
    if git_try(root, &["rev-parse", "--verify", base_branch]).is_some() {
        return Ok(base_branch.to_string());
    }
    let remote = format!("origin/{base_branch}");
    if git_try(root, &["rev-parse", "--verify", &remote]).is_some() {
        return Ok(remote);
    }
    Err(format!("base branch not found: {base_branch}"))
}

fn default_base_branch(root: &Path, current: &str) -> String {
    if let Some(remote_head) = git_try(
        root,
        &["symbolic-ref", "refs/remotes/origin/HEAD", "--short"],
    )
    .map(|value| value.trim().trim_start_matches("origin/").to_string())
    .filter(|value| !value.is_empty())
    {
        return remote_head;
    }
    for candidate in ["main", "master"] {
        if git_try(root, &["rev-parse", "--verify", candidate]).is_some() {
            return candidate.to_string();
        }
    }
    current.to_string()
}

fn workflow_state(session: &Session) -> Map<String, Value> {
    session
        .metadata
        .get(GIT_WORKFLOW_KEY)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn ensure_git_repo(root: &Path) -> Result<(), String> {
    git(
        root,
        &["rev-parse".to_string(), "--is-inside-work-tree".to_string()],
    )?;
    Ok(())
}

fn current_branch(root: &Path) -> Result<String, String> {
    let branch = git(root, &["branch".to_string(), "--show-current".to_string()])?
        .trim()
        .to_string();
    if branch.is_empty() {
        return Err("detached HEAD is not supported by the Git workflow".to_string());
    }
    Ok(branch)
}

fn validate_branch(root: &Path, branch: &str) -> Result<(), String> {
    if branch.trim().is_empty() || branch.chars().count() > 240 {
        return Err("invalid branch name".to_string());
    }
    git(
        root,
        &[
            "check-ref-format".to_string(),
            "--branch".to_string(),
            branch.to_string(),
        ],
    )?;
    Ok(())
}

fn map_paths(payload: &Map<String, Value>) -> Result<Vec<String>, String> {
    value_paths(payload.get("paths").unwrap_or(&Value::Null))
}

fn value_paths(value: &Value) -> Result<Vec<String>, String> {
    let raw_paths = if value.is_object() {
        value.get("paths").and_then(Value::as_array)
    } else {
        value.as_array()
    }
    .ok_or_else(|| "commit requires at least one selected path".to_string())?;
    if raw_paths.is_empty() || raw_paths.len() > MAX_GIT_WORKFLOW_PATHS {
        return Err(format!(
            "commit paths must contain 1 to {MAX_GIT_WORKFLOW_PATHS} entries"
        ));
    }
    let mut paths = Vec::with_capacity(raw_paths.len());
    for raw in raw_paths {
        let path = raw
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "commit path must be a non-empty string".to_string())?;
        let candidate = Path::new(path);
        if candidate.is_absolute()
            || candidate.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(format!("commit path escapes the workspace: {path}"));
        }
        if !paths.iter().any(|existing| existing == path) {
            paths.push(path.to_string());
        }
    }
    Ok(paths)
}

fn action_identity(action: &str) -> Result<(&'static str, &'static str, &'static str), String> {
    match action {
        "create_branch" => Ok(("git_branch", "创建 Git 分支", "git:branch:write")),
        "commit" => Ok(("git_commit", "提交选中的变更", "git:commit:write")),
        "create_pr" => Ok((
            "github_pull_request",
            "推送并创建 Pull Request",
            "github:pull_request:write",
        )),
        _ => Err(format!("unsupported Git workflow action: {action}")),
    }
}

fn required_session_query(request_path: &str) -> Result<String, String> {
    query_param(request_path, "session_id")
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "session_id is required".to_string())
}

fn parse_object(body: &str) -> Result<Map<String, Value>, String> {
    serde_json::from_str::<Value>(body)
        .map_err(|error| error.to_string())?
        .as_object()
        .cloned()
        .ok_or_else(|| "request body must be a JSON object".to_string())
}

fn required_string(payload: &Map<String, Value>, field: &str) -> Result<String, String> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| format!("{field} is required"))
}

fn optional_string(payload: &Map<String, Value>, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn object_string(payload: &Map<String, Value>, field: &str) -> Option<String> {
    optional_string(payload, field)
}

fn required_value_string(payload: &Value, field: &str) -> Result<String, String> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| format!("Git approval is missing {field}"))
}

fn git(root: &Path, args: &[String]) -> Result<String, String> {
    external(root, OsStr::new("git"), args, "Git")
}

fn git_try(root: &Path, args: &[&str]) -> Option<String> {
    let args = args
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    git(root, &args).ok()
}

fn external(root: &Path, binary: &OsStr, args: &[String], label: &str) -> Result<String, String> {
    let output = Command::new(binary)
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_HTTP_LOW_SPEED_LIMIT", "1")
        .env("GIT_HTTP_LOW_SPEED_TIME", "30")
        .args(args)
        .output()
        .map_err(|error| format!("failed to launch {label}: {error}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    Err(if detail.is_empty() {
        format!("{label} exited with status {}", output.status)
    } else {
        format!("{label}: {detail}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn run(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[cfg(unix)]
    fn git_workflow_requires_approval_and_persists_review_handoff_state() {
        let root = std::env::temp_dir().join(format!("openagent-git-workflow-{}", now_ms()));
        let workspace = root.join("workspace");
        let sessions = root.join("sessions");
        let remote = root.join("origin.git");
        fs::create_dir_all(&workspace).expect("workspace");
        run(&workspace, &["init", "-q", "-b", "main"]);
        run(
            &workspace,
            &["config", "user.email", "openagent-test@example.invalid"],
        );
        run(&workspace, &["config", "user.name", "OpenAgent Test"]);
        fs::write(workspace.join("README.md"), "baseline\n").expect("baseline");
        run(&workspace, &["add", "README.md"]);
        run(&workspace, &["commit", "-q", "-m", "baseline"]);
        fs::create_dir_all(&remote).expect("remote parent");
        run(&remote, &["init", "-q", "--bare"]);
        run(
            &workspace,
            &["remote", "add", "origin", remote.to_string_lossy().as_ref()],
        );
        run(&workspace, &["push", "-q", "-u", "origin", "main"]);

        let config = HttpRuntimeConfig {
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(sessions.to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };
        let created = create_session_payload(
            &config,
            &stable_json_dumps(&json!({"cwd": workspace.to_string_lossy()})),
        )
        .expect("create session");
        let session_id = created["session_id"].as_str().expect("session id");

        let branch_request = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: "/api/git/workflow/actions".to_string(),
                headers: BTreeMap::new(),
                body: stable_json_dumps(&json!({
                    "session_id": session_id,
                    "action": "create_branch",
                    "branch": "feature/review-handoff",
                })),
            },
            &config,
        );
        assert_eq!(branch_request.status, 202);
        assert_eq!(
            current_branch(&workspace).expect("branch before approval"),
            "main"
        );
        let request_id = branch_request
            .body
            .as_ref()
            .and_then(|body| body["request_id"].as_str())
            .expect("branch request id")
            .to_string();
        let approvals =
            pending_approvals_payload(&config, &format!("/api/approvals?session_id={session_id}"));
        assert_eq!(approvals["count"], 1);
        let resolved = respond_global_approval_payload(
            &config,
            &request_id,
            &stable_json_dumps(&json!({"action": "allow", "scope": "once"})),
        )
        .expect("approve branch");
        assert_eq!(resolved["status"], "completed");
        assert_eq!(
            current_branch(&workspace).expect("branch after approval"),
            "feature/review-handoff"
        );

        fs::write(workspace.join("README.md"), "baseline\nreview workflow\n")
            .expect("change readme");
        let commit_request = request_git_workflow_action(
            &config,
            &stable_json_dumps(&json!({
                "session_id": session_id,
                "action": "commit",
                "message": "Add review workflow",
                "paths": ["README.md"],
            })),
        )
        .expect("request commit");
        assert_ne!(
            git_try(&workspace, &["status", "--porcelain"])
                .unwrap_or_default()
                .trim(),
            ""
        );
        respond_global_approval_payload(
            &config,
            commit_request["request_id"]
                .as_str()
                .expect("commit request id"),
            &stable_json_dumps(&json!({"action": "allow"})),
        )
        .expect("approve commit");
        assert_eq!(
            git_try(&workspace, &["log", "-1", "--pretty=%s"])
                .unwrap_or_default()
                .trim(),
            "Add review workflow"
        );

        let summary = generate_git_workflow_summary(
            &config,
            &stable_json_dumps(&json!({
                "session_id": session_id,
                "base_branch": "main",
            })),
        )
        .expect("generate summary");
        assert_eq!(summary["summary"]["title"], "Add review workflow");
        assert!(
            summary["summary"]["body"]
                .as_str()
                .is_some_and(|body| body.contains("README.md"))
        );

        let fake_gh = root.join("fake-gh");
        fs::write(
            &fake_gh,
            "#!/bin/sh\nprintf 'https://example.invalid/review/42\\n'\n",
        )
        .expect("fake gh");
        let mut permissions = fs::metadata(&fake_gh)
            .expect("fake gh metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&fake_gh, permissions).expect("fake gh executable");
        let handoff = execute_git_workflow_action(
            &workspace,
            "create_pr",
            &json!({
                "title": summary["summary"]["title"],
                "body": summary["summary"]["body"],
                "base_branch": "main",
                "head_branch": "feature/review-handoff",
                "draft": true,
            }),
            fake_gh.as_os_str(),
        )
        .expect("review handoff");
        assert_eq!(handoff["url"], "https://example.invalid/review/42");

        let recovered = git_workflow_payload(
            &config,
            &format!("/api/git/workflow?session_id={session_id}"),
        )
        .expect("recover workflow");
        assert_eq!(recovered["base_branch"], "main");
        assert_eq!(recovered["summary"]["title"], "Add review workflow");
        assert!(recovered["pending"].is_null());

        let _ = fs::remove_dir_all(root);
    }
}
