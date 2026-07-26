use super::*;

const PERFORMANCE_SCHEMA_VERSION: &str = "openagent.performance_probe.v1";
const PERFORMANCE_STATE_DIR: &str = ".openagent-runtime/performance";
const WORKSPACE_SCAN_LIMIT: usize = 50_000;
const WORKSPACE_SCAN_TIMEOUT_MS: u64 = 2_000;

pub(super) fn performance_status_payload(
    config: &HttpRuntimeConfig,
    request_path: &str,
) -> Result<Value, String> {
    let session_id = performance_session_id(request_path);
    let _ = workspace_for_session(config, session_id.as_deref())?;
    let latest = read_json_file(&performance_probe_path(config, session_id.as_deref()));
    let latest = (latest.get("schema_version").and_then(Value::as_str)
        == Some(PERFORMANCE_SCHEMA_VERSION))
    .then_some(latest);
    Ok(json!({
        "schema_version": PERFORMANCE_SCHEMA_VERSION,
        "session_id": session_id,
        "latest": latest,
        "budgets": performance_budgets(),
    }))
}

pub(super) fn run_performance_probe_payload(
    config: &HttpRuntimeConfig,
    request_path: &str,
) -> Result<Value, String> {
    let session_id = performance_session_id(request_path);
    let workspace = workspace_for_session(config, session_id.as_deref())?;
    let started = Instant::now();
    let mut profiles = Vec::new();

    let workspace_started = Instant::now();
    let workspace_scale = scan_workspace(&workspace);
    profiles.push(performance_profile(
        "workspace_scan",
        "大仓库扫描",
        workspace_started.elapsed(),
        performance_budget_ms("workspace_scan", 350),
        workspace_scale,
        5_000,
        "files",
    ));

    let session_started = Instant::now();
    let session_scale = session_id
        .as_deref()
        .map(|id| session_projection_scale(config, id))
        .transpose()?
        .unwrap_or_else(|| json!({"messages": 0, "projected_messages": 0, "transcript_bytes": 0}));
    profiles.push(performance_profile(
        "session_projection",
        "长会话投影",
        session_started.elapsed(),
        performance_budget_ms("session_projection", 180),
        session_scale,
        200,
        "messages",
    ));

    let diff_started = Instant::now();
    let diff_scale = git_diff_scale(&workspace);
    profiles.push(performance_profile(
        "diff_projection",
        "大 Diff 汇总",
        diff_started.elapsed(),
        performance_budget_ms("diff_projection", 350),
        diff_scale,
        5_000,
        "changed_lines",
    ));

    let task_started = Instant::now();
    let task_scale = session_id
        .as_deref()
        .map(|id| session_tasks_payload(config, id))
        .unwrap_or_else(|| json!({"count": 0, "status_counts": {}}));
    profiles.push(performance_profile(
        "task_projection",
        "多任务投影",
        task_started.elapsed(),
        performance_budget_ms("task_projection", 180),
        json!({
            "tasks": task_scale.get("count").and_then(Value::as_u64).unwrap_or(0),
            "status_counts": task_scale.get("status_counts").cloned().unwrap_or_else(|| json!({})),
        }),
        16,
        "tasks",
    ));

    let attachment_started = Instant::now();
    let attachment_scale = session_id
        .as_deref()
        .map(|id| attachment_projection_scale(config, id))
        .transpose()?
        .unwrap_or_else(|| json!({"attachments": 0, "attachment_bytes": 0, "truncated": 0}));
    profiles.push(performance_profile(
        "attachment_projection",
        "大附件投影",
        attachment_started.elapsed(),
        performance_budget_ms("attachment_projection", 100),
        attachment_scale,
        1_000_000,
        "attachment_bytes",
    ));

    let warning_count = profiles
        .iter()
        .filter(|profile| profile.get("status").and_then(Value::as_str) == Some("warning"))
        .count();
    let full_scale_count = profiles
        .iter()
        .filter(|profile| profile.get("coverage").and_then(Value::as_str) == Some("full_scale"))
        .count();
    let payload = json!({
        "schema_version": PERFORMANCE_SCHEMA_VERSION,
        "session_id": session_id,
        "measured_at_ms": now_ms(),
        "status": if warning_count == 0 { "passed" } else { "warning" },
        "warning_count": warning_count,
        "full_scale_count": full_scale_count,
        "profile_count": profiles.len(),
        "total_duration_ms": elapsed_millis(started.elapsed()),
        "profiles": profiles,
        "privacy": {
            "content_included": false,
            "paths_included": false,
            "credentials_included": false,
        },
    });
    write_json_value(
        &performance_probe_path(config, session_id.as_deref()),
        &payload,
    )?;
    Ok(payload)
}

fn performance_session_id(request_path: &str) -> Option<String> {
    query_param(request_path, "session_id").filter(|value| !value.trim().is_empty())
}

fn performance_probe_path(config: &HttpRuntimeConfig, session_id: Option<&str>) -> PathBuf {
    let key = session_id.unwrap_or("workspace");
    session_root(config)
        .join(PERFORMANCE_STATE_DIR)
        .join(format!("{key}.json"))
}

fn performance_budgets() -> Value {
    json!({
        "workspace_scan_ms": performance_budget_ms("workspace_scan", 350),
        "session_projection_ms": performance_budget_ms("session_projection", 180),
        "diff_projection_ms": performance_budget_ms("diff_projection", 350),
        "task_projection_ms": performance_budget_ms("task_projection", 180),
        "attachment_projection_ms": performance_budget_ms("attachment_projection", 100),
    })
}

fn performance_budget_ms(profile: &str, default: u64) -> u64 {
    let key = format!("OPENAGENT_PERF_{}_BUDGET_MS", profile.to_ascii_uppercase());
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
        .clamp(1, 30_000)
}

fn performance_profile(
    id: &str,
    label: &str,
    duration: Duration,
    budget_ms: u64,
    scale: Value,
    target: u64,
    scale_key: &str,
) -> Value {
    let duration_ms = elapsed_millis(duration);
    let observed = scale.get(scale_key).and_then(Value::as_u64).unwrap_or(0);
    json!({
        "id": id,
        "label": label,
        "status": if duration_ms <= budget_ms { "passed" } else { "warning" },
        "duration_ms": duration_ms,
        "budget_ms": budget_ms,
        "coverage": if observed >= target { "full_scale" } else { "sampled" },
        "target": target,
        "scale_key": scale_key,
        "observed": observed,
        "scale": scale,
    })
}

fn elapsed_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn scan_workspace(root: &Path) -> Value {
    let started = Instant::now();
    let mut stack = vec![root.to_path_buf()];
    let mut files = 0_u64;
    let mut sampled_bytes = 0_u64;
    let mut truncated = false;
    while let Some(directory) = stack.pop() {
        if files as usize >= WORKSPACE_SCAN_LIMIT
            || elapsed_millis(started.elapsed()) >= WORKSPACE_SCAN_TIMEOUT_MS
        {
            truncated = true;
            break;
        }
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if matches!(
                    name.as_ref(),
                    ".git" | ".openagent" | "node_modules" | "target"
                ) {
                    continue;
                }
                stack.push(path);
            } else if path.is_file() {
                files = files.saturating_add(1);
                sampled_bytes = sampled_bytes.saturating_add(
                    entry
                        .metadata()
                        .ok()
                        .map(|metadata| metadata.len())
                        .unwrap_or(0),
                );
                if files as usize >= WORKSPACE_SCAN_LIMIT {
                    truncated = true;
                    break;
                }
            }
        }
    }
    json!({
        "files": files,
        "sampled_bytes": sampled_bytes,
        "truncated": truncated,
        "file_limit": WORKSPACE_SCAN_LIMIT,
        "timeout_ms": WORKSPACE_SCAN_TIMEOUT_MS,
    })
}

fn session_projection_scale(config: &HttpRuntimeConfig, session_id: &str) -> Result<Value, String> {
    let store = FileSessionStore::new(session_root(config));
    let session = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    let projected = store
        .list_messages_with_parts(session_id, Some(200), None)
        .map_err(|error| error.to_string())?;
    let transcript_bytes = fs::metadata(
        session_root(config)
            .join(session_id)
            .join("transcript.jsonl"),
    )
    .ok()
    .map(|metadata| metadata.len())
    .unwrap_or(0);
    Ok(json!({
        "messages": session.messages.len(),
        "projected_messages": projected.len(),
        "transcript_bytes": transcript_bytes,
    }))
}

fn git_diff_scale(workspace: &Path) -> Value {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["diff", "--numstat", "--no-ext-diff", "HEAD"])
        .output();
    let Ok(output) = output else {
        return json!({"files": 0, "changed_lines": 0, "available": false});
    };
    if !output.status.success() {
        return json!({"files": 0, "changed_lines": 0, "available": false});
    }
    let mut files = 0_u64;
    let mut additions = 0_u64;
    let mut deletions = 0_u64;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split('\t');
        let added = fields.next().and_then(|value| value.parse::<u64>().ok());
        let deleted = fields.next().and_then(|value| value.parse::<u64>().ok());
        if added.is_none() && deleted.is_none() {
            continue;
        }
        files = files.saturating_add(1);
        additions = additions.saturating_add(added.unwrap_or(0));
        deletions = deletions.saturating_add(deleted.unwrap_or(0));
    }
    json!({
        "files": files,
        "additions": additions,
        "deletions": deletions,
        "changed_lines": additions.saturating_add(deletions),
        "available": true,
    })
}

fn attachment_projection_scale(
    config: &HttpRuntimeConfig,
    session_id: &str,
) -> Result<Value, String> {
    let store = FileSessionStore::new(session_root(config));
    let session = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    let mut attachments = 0_u64;
    let mut attachment_bytes = 0_u64;
    let mut truncated = 0_u64;
    for message in &session.messages {
        let Some(items) = message
            .metadata
            .get("attachments")
            .and_then(Value::as_array)
        else {
            continue;
        };
        for item in items {
            attachments = attachments.saturating_add(1);
            attachment_bytes = attachment_bytes.saturating_add(
                item.get("original_content_bytes")
                    .or_else(|| item.get("size_bytes"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            );
            if item
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                truncated = truncated.saturating_add(1);
            }
        }
    }
    Ok(json!({
        "attachments": attachments,
        "attachment_bytes": attachment_bytes,
        "truncated": truncated,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn performance_probe_is_persisted_bounded_and_content_safe() {
        let root = std::env::temp_dir().join(format!("openagent-performance-{}", now_ms()));
        let workspace = root.join("workspace");
        let sessions = root.join("sessions");
        fs::create_dir_all(&workspace).expect("workspace");
        for index in 0..250 {
            let directory = workspace.join(format!("crate-{}", index / 25));
            fs::create_dir_all(&directory).expect("fixture directory");
            fs::write(
                directory.join(format!("file-{index}.rs")),
                "fn fixture() {}\n",
            )
            .expect("fixture file");
        }
        let config = HttpRuntimeConfig {
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(sessions.to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };
        let created = create_session_payload(&config, "{}").expect("create session");
        let session_id = created["session_id"].as_str().expect("session id");
        let secret_content = "PRIVATE_ATTACHMENT_CONTENT_MUST_NOT_LEAK";
        let turn = start_turn_payload(
            &config,
            session_id,
            &json!({
                "input": "performance fixture",
                "tool_call": {
                    "call_id": "call_performance_fixture",
                    "name": "read",
                    "input": {"file_path": "crate-0/file-0.rs"}
                },
                "attachments": [{
                    "kind": "file",
                    "name": "fixture.txt",
                    "size_bytes": secret_content.len(),
                    "original_content_bytes": 1_200_000,
                    "content": secret_content,
                    "truncated": true,
                }]
            })
            .to_string(),
        )
        .expect("fixture turn");
        assert_eq!(turn["status"], "completed");

        let probe = run_performance_probe_payload(
            &config,
            &format!("/api/performance/probe?session_id={session_id}"),
        )
        .expect("performance probe");
        assert_eq!(probe["schema_version"], PERFORMANCE_SCHEMA_VERSION);
        assert_eq!(probe["profile_count"], 5);
        assert_eq!(probe["privacy"]["content_included"], false);
        assert_eq!(probe["profiles"][0]["scale"]["files"], 250);
        assert_eq!(probe["profiles"][4]["scale"]["attachments"], 1);
        assert_eq!(probe["profiles"][4]["scale"]["attachment_bytes"], 1_200_000);
        assert!(!probe.to_string().contains(secret_content));

        let status = performance_status_payload(
            &config,
            &format!("/api/performance?session_id={session_id}"),
        )
        .expect("persisted performance status");
        assert_eq!(status["latest"]["measured_at_ms"], probe["measured_at_ms"]);
        let _ = fs::remove_dir_all(root);
    }
}
