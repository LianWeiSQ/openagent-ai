use super::*;

const STORAGE_STATUS_SCHEMA: &str = "openagent.storage_status.v1";
const STORAGE_MIGRATION_SCHEMA: &str = "openagent.storage_migration.v1";
const STORAGE_SCHEMA_SET: &str = "openagent.storage.2026-07";
const MIGRATION_DIR: &str = ".openagent-runtime/migrations";
const MIGRATION_BACKUP_DIR: &str = ".openagent-runtime/migration-backups";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StorageMigrationAction {
    kind: String,
    relative_path: String,
    scope: String,
    session_id: Option<String>,
    source_schema: Option<Value>,
    target_schema: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StorageMigrationManifest {
    schema_version: String,
    migration_id: String,
    target_schema_set: String,
    status: String,
    created_at_ms: u64,
    completed_at_ms: Option<u64>,
    rolled_back_at_ms: Option<u64>,
    action_count: usize,
    changed_file_count: usize,
    backup_file_count: usize,
    actions: Vec<StorageMigrationAction>,
    error_code: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct StorageAuditPlan {
    actions: Vec<StorageMigrationAction>,
    blocked_reasons: BTreeMap<String, u64>,
    session_count: u64,
    current_session_count: u64,
    legacy_session_count: u64,
    blocked_session_count: u64,
    runtime_state_count: u64,
    current_runtime_state_count: u64,
    legacy_runtime_state_count: u64,
    transcript_record_count: u64,
    compatible_legacy_record_count: u64,
}

pub(super) fn storage_status_payload(config: &HttpRuntimeConfig) -> Result<Value, String> {
    storage_status_for_root(&session_root(config))
}

pub(super) fn storage_migrate_payload(config: &HttpRuntimeConfig) -> Result<Value, String> {
    let root = session_root(config);
    let plan = audit_storage(&root)?;
    if !plan.blocked_reasons.is_empty() {
        return Err("storage migration is blocked by incompatible or corrupt state".to_string());
    }
    if plan.actions.is_empty() {
        let mut status = storage_status_for_root(&root)?;
        status["migration"] = json!({
            "status": "no_changes",
            "target_schema_set": STORAGE_SCHEMA_SET,
            "changed_file_count": 0,
        });
        return Ok(status);
    }
    let manifest = execute_storage_migration(&root, plan.actions, None)?;
    let mut status = storage_status_for_root(&root)?;
    status["migration"] = public_migration_manifest(&manifest);
    Ok(status)
}

#[derive(Default, Deserialize)]
struct StorageRollbackRequest {
    migration_id: Option<String>,
}

pub(super) fn storage_rollback_payload(
    config: &HttpRuntimeConfig,
    body: &str,
) -> Result<Value, String> {
    let request = serde_json::from_str::<StorageRollbackRequest>(body).unwrap_or_default();
    let root = session_root(config);
    let migration_id = request
        .migration_id
        .filter(|value| valid_migration_id(value))
        .or_else(|| latest_completed_manifest(&root).map(|manifest| manifest.migration_id))
        .ok_or_else(|| "completed storage migration not found".to_string())?;
    let mut manifest = read_migration_manifest(&root, &migration_id)?;
    if manifest.status != "completed" {
        return Err("only a completed storage migration can be rolled back".to_string());
    }
    restore_migration_files(&root, &manifest)?;
    manifest.status = "rolled_back".to_string();
    manifest.rolled_back_at_ms = Some(now_ms());
    write_migration_manifest(&root, &manifest)?;
    let mut status = storage_status_for_root(&root)?;
    status["migration"] = public_migration_manifest(&manifest);
    Ok(status)
}

fn storage_status_for_root(root: &Path) -> Result<Value, String> {
    let plan = audit_storage(root)?;
    let readiness = if !plan.blocked_reasons.is_empty() {
        "blocked"
    } else if !plan.actions.is_empty() {
        "needs_migration"
    } else {
        "ready"
    };
    let latest_migration = latest_migration_manifest(root);
    let rollback_candidate = latest_completed_manifest(root);
    Ok(json!({
        "schema_version": STORAGE_STATUS_SCHEMA,
        "target_schema_set": STORAGE_SCHEMA_SET,
        "readiness": readiness,
        "session_count": plan.session_count,
        "current_session_count": plan.current_session_count,
        "legacy_session_count": plan.legacy_session_count,
        "blocked_session_count": plan.blocked_session_count,
        "runtime_state_count": plan.runtime_state_count,
        "current_runtime_state_count": plan.current_runtime_state_count,
        "legacy_runtime_state_count": plan.legacy_runtime_state_count,
        "transcript_record_count": plan.transcript_record_count,
        "compatible_legacy_record_count": plan.compatible_legacy_record_count,
        "planned_action_count": plan.actions.len(),
        "blocked_count": plan.blocked_reasons.values().sum::<u64>(),
        "blocked_reasons": plan.blocked_reasons,
        "can_migrate": readiness == "needs_migration",
        "can_rollback": rollback_candidate.is_some(),
        "latest_migration": latest_migration.as_ref().map(public_migration_manifest),
        "rollback_candidate": rollback_candidate.as_ref().map(public_migration_manifest),
        "privacy": {
            "paths_included": false,
            "content_included": false,
            "credentials_included": false,
        },
    }))
}

fn audit_storage(root: &Path) -> Result<StorageAuditPlan, String> {
    fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let mut plan = StorageAuditPlan::default();
    let mut entries = fs::read_dir(root)
        .map_err(|error| error.to_string())?
        .flatten()
        .filter(|entry| {
            entry.path().is_dir() && entry.file_name().to_string_lossy() != ".openagent-runtime"
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let session_dir = entry.path();
        let session_id = entry.file_name().to_string_lossy().to_string();
        let state_path = session_dir.join("state.latest.json");
        let transcript_path = session_dir.join("transcript.jsonl");
        let session_path = session_dir.join("session.json");
        if !state_path.exists() && !transcript_path.exists() && !session_path.exists() {
            continue;
        }
        plan.session_count = plan.session_count.saturating_add(1);
        let action_start = plan.actions.len();
        let blocked_start = plan.blocked_reasons.values().sum::<u64>();
        if transcript_path.exists() {
            audit_transcript(&transcript_path, &mut plan);
        }
        if state_path.exists() {
            audit_schema_file(
                root,
                &state_path,
                json!("openagent.session_state.v1"),
                "session_state",
                Some(session_id.clone()),
                &mut plan,
            );
        } else if transcript_path.exists() && session_path.exists() {
            plan.actions.push(StorageMigrationAction {
                kind: "rebuild_session_state".to_string(),
                relative_path: relative_string(root, &state_path)?,
                scope: "session_state".to_string(),
                session_id: Some(session_id.clone()),
                source_schema: None,
                target_schema: json!("openagent.session_state.v1"),
            });
        } else {
            increment_reason(&mut plan, "session_state_missing");
        }
        if session_path.exists() {
            audit_schema_file(
                root,
                &session_path,
                json!("openagent.session.v1"),
                "session_record",
                Some(session_id.clone()),
                &mut plan,
            );
        } else if state_path.exists() {
            plan.actions.push(StorageMigrationAction {
                kind: "create_session_record".to_string(),
                relative_path: relative_string(root, &session_path)?,
                scope: "session_record".to_string(),
                session_id: Some(session_id),
                source_schema: None,
                target_schema: json!("openagent.session.v1"),
            });
        } else {
            increment_reason(&mut plan, "session_record_missing");
        }
        let blocked_end = plan.blocked_reasons.values().sum::<u64>();
        if blocked_end > blocked_start {
            plan.blocked_session_count = plan.blocked_session_count.saturating_add(1);
        } else if plan.actions.len() > action_start {
            plan.legacy_session_count = plan.legacy_session_count.saturating_add(1);
        } else {
            plan.current_session_count = plan.current_session_count.saturating_add(1);
        }
    }

    for (relative, schema) in [
        (
            ".openagent-runtime/provider.json",
            json!("openagent.provider.v1"),
        ),
        (
            ".openagent-runtime/capabilities.json",
            json!("openagent.capabilities.v1"),
        ),
        (
            ".openagent-runtime/extensions.json",
            json!("openagent.extensions.v1"),
        ),
        (".openagent-runtime/turn_jobs.json", json!(1)),
    ] {
        let path = root.join(relative);
        if path.exists() {
            plan.runtime_state_count = plan.runtime_state_count.saturating_add(1);
            let before = plan.actions.len();
            let blocked_before = plan.blocked_reasons.values().sum::<u64>();
            audit_schema_file(root, &path, schema, "runtime_state", None, &mut plan);
            if plan.blocked_reasons.values().sum::<u64>() > blocked_before {
                continue;
            }
            if plan.actions.len() > before {
                plan.legacy_runtime_state_count = plan.legacy_runtime_state_count.saturating_add(1);
            } else {
                plan.current_runtime_state_count =
                    plan.current_runtime_state_count.saturating_add(1);
            }
        }
    }
    audit_runtime_state_directory(
        root,
        ".openagent-runtime/performance",
        json!("openagent.performance_probe.v1"),
        &mut plan,
    );
    audit_runtime_state_directory(root, ".openagent-runtime/mcp_oauth", json!(1), &mut plan);
    Ok(plan)
}

fn audit_runtime_state_directory(
    root: &Path,
    relative: &str,
    schema: Value,
    plan: &mut StorageAuditPlan,
) {
    let directory = root.join(relative);
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten().filter(|entry| {
        entry.path().is_file()
            && entry.path().extension().and_then(|value| value.to_str()) == Some("json")
    }) {
        plan.runtime_state_count = plan.runtime_state_count.saturating_add(1);
        let before = plan.actions.len();
        let blocked_before = plan.blocked_reasons.values().sum::<u64>();
        audit_schema_file(
            root,
            &entry.path(),
            schema.clone(),
            "runtime_state",
            None,
            plan,
        );
        if plan.blocked_reasons.values().sum::<u64>() > blocked_before {
            continue;
        }
        if plan.actions.len() > before {
            plan.legacy_runtime_state_count = plan.legacy_runtime_state_count.saturating_add(1);
        } else {
            plan.current_runtime_state_count = plan.current_runtime_state_count.saturating_add(1);
        }
    }
}

fn audit_schema_file(
    root: &Path,
    path: &Path,
    target_schema: Value,
    scope: &str,
    session_id: Option<String>,
    plan: &mut StorageAuditPlan,
) {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(_) => {
            increment_reason(plan, "state_unreadable");
            return;
        }
    };
    let value = match serde_json::from_str::<Value>(&raw) {
        Ok(value) if value.is_object() => value,
        _ => {
            increment_reason(plan, "state_corrupt");
            return;
        }
    };
    let source_schema = value.get("schema_version").cloned();
    if source_schema.as_ref() == Some(&target_schema) {
        return;
    }
    let known_legacy = source_schema.as_ref().is_none_or(|schema| {
        schema.as_u64() == Some(0)
            || schema
                .as_str()
                .is_some_and(|schema| schema.ends_with(".v0"))
    });
    if !known_legacy {
        increment_reason(plan, "unsupported_schema");
        return;
    }
    let Ok(relative_path) = relative_string(root, path) else {
        increment_reason(plan, "unsafe_state_path");
        return;
    };
    plan.actions.push(StorageMigrationAction {
        kind: "set_schema".to_string(),
        relative_path,
        scope: scope.to_string(),
        session_id,
        source_schema,
        target_schema,
    });
}

fn audit_transcript(path: &Path, plan: &mut StorageAuditPlan) {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => {
            increment_reason(plan, "transcript_unreadable");
            return;
        }
    };
    use std::io::BufRead;
    for line in std::io::BufReader::new(file).lines() {
        let Ok(line) = line else {
            increment_reason(plan, "transcript_unreadable");
            return;
        };
        if line.trim().is_empty() {
            continue;
        }
        let value = match serde_json::from_str::<Value>(&line) {
            Ok(value) if value.is_object() => value,
            _ => {
                increment_reason(plan, "transcript_corrupt");
                return;
            }
        };
        plan.transcript_record_count = plan.transcript_record_count.saturating_add(1);
        if value
            .get("schema_version")
            .and_then(Value::as_str)
            .is_none_or(|schema| schema == "openagent.message.v1")
        {
            plan.compatible_legacy_record_count =
                plan.compatible_legacy_record_count.saturating_add(1);
        }
    }
}

fn execute_storage_migration(
    root: &Path,
    actions: Vec<StorageMigrationAction>,
    fail_after: Option<usize>,
) -> Result<StorageMigrationManifest, String> {
    let migration_id = format!("migration_{}_{}", now_ms(), std::process::id());
    let mut manifest = StorageMigrationManifest {
        schema_version: STORAGE_MIGRATION_SCHEMA.to_string(),
        migration_id,
        target_schema_set: STORAGE_SCHEMA_SET.to_string(),
        status: "prepared".to_string(),
        created_at_ms: now_ms(),
        completed_at_ms: None,
        rolled_back_at_ms: None,
        action_count: actions.len(),
        changed_file_count: 0,
        backup_file_count: 0,
        actions,
        error_code: None,
    };
    backup_migration_files(root, &mut manifest)?;
    write_migration_manifest(root, &manifest)?;
    let result = apply_migration_actions(root, &manifest.actions, fail_after);
    if result.is_err() {
        let _ = restore_migration_files(root, &manifest);
        manifest.status = "failed_rolled_back".to_string();
        manifest.rolled_back_at_ms = Some(now_ms());
        manifest.error_code = Some("migration_apply_failed".to_string());
        let _ = write_migration_manifest(root, &manifest);
        return Err("storage migration failed and original state was restored".to_string());
    }
    let post = audit_storage(root)?;
    if !post.blocked_reasons.is_empty() || !post.actions.is_empty() {
        let _ = restore_migration_files(root, &manifest);
        manifest.status = "failed_rolled_back".to_string();
        manifest.rolled_back_at_ms = Some(now_ms());
        manifest.error_code = Some("migration_validation_failed".to_string());
        let _ = write_migration_manifest(root, &manifest);
        return Err(
            "storage migration validation failed and original state was restored".to_string(),
        );
    }
    manifest.status = "completed".to_string();
    manifest.completed_at_ms = Some(now_ms());
    manifest.changed_file_count = manifest.actions.len();
    write_migration_manifest(root, &manifest)?;
    Ok(manifest)
}

fn apply_migration_actions(
    root: &Path,
    actions: &[StorageMigrationAction],
    fail_after: Option<usize>,
) -> Result<(), String> {
    let store = FileSessionStore::new(root);
    for (index, action) in actions.iter().enumerate() {
        let path = safe_join(root, &action.relative_path)?;
        match action.kind.as_str() {
            "set_schema" => {
                let raw = fs::read_to_string(&path).map_err(|error| error.to_string())?;
                let mut value =
                    serde_json::from_str::<Value>(&raw).map_err(|error| error.to_string())?;
                let object = value
                    .as_object_mut()
                    .ok_or_else(|| "migration target is not an object".to_string())?;
                object.insert("schema_version".to_string(), action.target_schema.clone());
                atomic_write_private_json(&path, &value)?;
            }
            "create_session_record" => {
                let session_id = action
                    .session_id
                    .as_deref()
                    .ok_or_else(|| "migration session id missing".to_string())?;
                let state_path = root.join(session_id).join("state.latest.json");
                let state = read_required_object(&state_path)?;
                let updated_at_ms = state
                    .get("updated_at_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or_else(now_ms);
                atomic_write_private_json(
                    &path,
                    &json!({
                        "schema_version": "openagent.session.v1",
                        "session_id": session_id,
                        "workspace": state.get("workspace").cloned().unwrap_or_else(|| json!(".")),
                        "status": state.get("status").cloned().unwrap_or_else(|| json!("idle")),
                        "created_at_ms": updated_at_ms,
                        "updated_at_ms": updated_at_ms,
                        "active_run_id": state.get("run_id").cloned().unwrap_or(Value::Null),
                    }),
                )?;
            }
            "rebuild_session_state" => {
                let session_id = action
                    .session_id
                    .as_deref()
                    .ok_or_else(|| "migration session id missing".to_string())?;
                let session = store
                    .load_session(session_id)
                    .map_err(|error| error.to_string())?;
                store
                    .save_state(&session, None)
                    .map_err(|error| error.to_string())?;
            }
            _ => return Err("unsupported storage migration action".to_string()),
        }
        if fail_after == Some(index + 1) {
            return Err("injected storage migration failure".to_string());
        }
    }
    Ok(())
}

fn backup_migration_files(
    root: &Path,
    manifest: &mut StorageMigrationManifest,
) -> Result<(), String> {
    let backup_root = migration_backup_root(root, &manifest.migration_id);
    let mut seen = BTreeSet::new();
    for action in &manifest.actions {
        if !seen.insert(action.relative_path.clone()) {
            continue;
        }
        let source = safe_join(root, &action.relative_path)?;
        if !source.exists() {
            continue;
        }
        let target = safe_join(&backup_root, &action.relative_path)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::copy(source, target).map_err(|error| error.to_string())?;
        manifest.backup_file_count = manifest.backup_file_count.saturating_add(1);
    }
    Ok(())
}

fn restore_migration_files(root: &Path, manifest: &StorageMigrationManifest) -> Result<(), String> {
    let backup_root = migration_backup_root(root, &manifest.migration_id);
    let mut seen = BTreeSet::new();
    for action in manifest.actions.iter().rev() {
        if !seen.insert(action.relative_path.clone()) {
            continue;
        }
        let target = safe_join(root, &action.relative_path)?;
        let backup = safe_join(&backup_root, &action.relative_path)?;
        if backup.exists() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            fs::copy(backup, target).map_err(|error| error.to_string())?;
        } else if target.exists() {
            fs::remove_file(target).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn migration_manifest_dir(root: &Path) -> PathBuf {
    root.join(MIGRATION_DIR)
}

fn migration_manifest_path(root: &Path, migration_id: &str) -> PathBuf {
    migration_manifest_dir(root).join(format!("{migration_id}.json"))
}

fn migration_backup_root(root: &Path, migration_id: &str) -> PathBuf {
    root.join(MIGRATION_BACKUP_DIR).join(migration_id)
}

fn write_migration_manifest(
    root: &Path,
    manifest: &StorageMigrationManifest,
) -> Result<(), String> {
    let value = serde_json::to_value(manifest).map_err(|error| error.to_string())?;
    atomic_write_private_json(
        &migration_manifest_path(root, &manifest.migration_id),
        &value,
    )
}

fn read_migration_manifest(
    root: &Path,
    migration_id: &str,
) -> Result<StorageMigrationManifest, String> {
    if !valid_migration_id(migration_id) {
        return Err("invalid migration id".to_string());
    }
    let raw = fs::read_to_string(migration_manifest_path(root, migration_id))
        .map_err(|_| "storage migration not found".to_string())?;
    serde_json::from_str(&raw).map_err(|_| "storage migration manifest is corrupt".to_string())
}

fn latest_migration_manifest(root: &Path) -> Option<StorageMigrationManifest> {
    let mut manifests = fs::read_dir(migration_manifest_dir(root))
        .ok()?
        .flatten()
        .filter(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("json"))
        .filter_map(|entry| fs::read_to_string(entry.path()).ok())
        .filter_map(|raw| serde_json::from_str::<StorageMigrationManifest>(&raw).ok())
        .collect::<Vec<_>>();
    manifests.sort_by_key(|manifest| manifest.created_at_ms);
    manifests.pop()
}

fn latest_completed_manifest(root: &Path) -> Option<StorageMigrationManifest> {
    let mut manifests = fs::read_dir(migration_manifest_dir(root))
        .ok()?
        .flatten()
        .filter_map(|entry| fs::read_to_string(entry.path()).ok())
        .filter_map(|raw| serde_json::from_str::<StorageMigrationManifest>(&raw).ok())
        .filter(|manifest| manifest.status == "completed")
        .collect::<Vec<_>>();
    manifests.sort_by_key(|manifest| manifest.created_at_ms);
    manifests.pop()
}

fn public_migration_manifest(manifest: &StorageMigrationManifest) -> Value {
    json!({
        "migration_id": manifest.migration_id,
        "target_schema_set": manifest.target_schema_set,
        "status": manifest.status,
        "created_at_ms": manifest.created_at_ms,
        "completed_at_ms": manifest.completed_at_ms,
        "rolled_back_at_ms": manifest.rolled_back_at_ms,
        "action_count": manifest.action_count,
        "changed_file_count": manifest.changed_file_count,
        "backup_file_count": manifest.backup_file_count,
        "error_code": manifest.error_code,
    })
}

fn atomic_write_private_json(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temporary = path.with_extension(format!("tmp-{}-{}", std::process::id(), now_ms()));
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    replace_file(&temporary, path)
}

fn replace_file(temporary: &Path, target: &Path) -> Result<(), String> {
    if !target.exists() {
        return fs::rename(temporary, target).map_err(|error| error.to_string());
    }
    let swap = target.with_extension(format!("swap-{}-{}", std::process::id(), now_ms()));
    fs::rename(target, &swap).map_err(|error| error.to_string())?;
    match fs::rename(temporary, target) {
        Ok(()) => {
            fs::remove_file(swap).map_err(|error| error.to_string())?;
            Ok(())
        }
        Err(error) => {
            let _ = fs::rename(&swap, target);
            let _ = fs::remove_file(temporary);
            Err(error.to_string())
        }
    }
}

fn read_required_object(path: &Path) -> Result<Map<String, Value>, String> {
    let raw = fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_json::from_str::<Value>(&raw)
        .map_err(|error| error.to_string())?
        .as_object()
        .cloned()
        .ok_or_else(|| "state file is not an object".to_string())
}

fn relative_string(root: &Path, path: &Path) -> Result<String, String> {
    path.strip_prefix(root)
        .map_err(|_| "state path is outside session root".to_string())
        .map(|relative| relative.to_string_lossy().to_string())
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err("unsafe migration path".to_string());
    }
    Ok(root.join(path))
}

fn valid_migration_id(value: &str) -> bool {
    value.starts_with("migration_")
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn increment_reason(plan: &mut StorageAuditPlan, reason: &str) {
    *plan.blocked_reasons.entry(reason.to_string()).or_default() += 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_session(root: &Path, session_id: &str, marker: &str) -> PathBuf {
        let directory = root.join(session_id);
        fs::create_dir_all(&directory).expect("legacy session directory");
        let state = json!({
            "session_id": session_id,
            "workspace": root.join("workspace").to_string_lossy(),
            "status": "idle",
            "updated_at_ms": 7,
            "messages": [{"role": "user", "content": marker, "metadata": {}}],
            "todos": [],
            "metadata": {},
        });
        fs::write(
            directory.join("state.latest.json"),
            serde_json::to_vec_pretty(&state).expect("legacy state"),
        )
        .expect("write legacy state");
        directory
    }

    #[test]
    fn storage_migration_is_idempotent_private_and_rollback_safe() {
        let root = std::env::temp_dir().join(format!("openagent-storage-migration-{}", now_ms()));
        fs::create_dir_all(root.join("workspace")).expect("workspace");
        let session_id = "session_legacy_fixture";
        let marker = "PRIVATE_SESSION_CONTENT_MUST_NOT_LEAK";
        let session_dir = legacy_session(&root, session_id, marker);
        fs::create_dir_all(root.join(".openagent-runtime")).expect("runtime state");
        fs::write(
            root.join(".openagent-runtime/capabilities.json"),
            r#"{"capabilities":{},"updated_at_ms":0}"#,
        )
        .expect("legacy runtime state");

        let before = fs::read(session_dir.join("state.latest.json")).expect("legacy bytes");
        let failed_plan = audit_storage(&root).expect("failed migration plan");
        assert_eq!(failed_plan.actions.len(), 3);
        assert!(execute_storage_migration(&root, failed_plan.actions, Some(1)).is_err());
        assert_eq!(
            fs::read(session_dir.join("state.latest.json")).expect("rolled back bytes"),
            before
        );
        assert!(!session_dir.join("session.json").exists());

        let plan = audit_storage(&root).expect("migration plan");
        let manifest = execute_storage_migration(&root, plan.actions, None).expect("migration");
        assert_eq!(manifest.status, "completed");
        assert_eq!(manifest.changed_file_count, 3);
        let ready = storage_status_for_root(&root).expect("ready status");
        assert_eq!(ready["readiness"], "ready");
        assert_eq!(ready["planned_action_count"], 0);
        assert!(!ready.to_string().contains(marker));
        let no_changes = audit_storage(&root).expect("idempotent audit");
        assert!(no_changes.actions.is_empty());

        restore_migration_files(&root, &manifest).expect("rollback files");
        assert_eq!(
            fs::read(session_dir.join("state.latest.json")).expect("restored bytes"),
            before
        );
        assert!(!session_dir.join("session.json").exists());
        assert_eq!(
            audit_storage(&root).expect("rollback audit").actions.len(),
            3
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn storage_audit_blocks_unknown_schema_without_changing_state() {
        let root = std::env::temp_dir().join(format!("openagent-storage-blocked-{}", now_ms()));
        let session_dir = legacy_session(&root, "session_future_fixture", "future");
        let path = session_dir.join("state.latest.json");
        let mut state = read_required_object(&path).expect("state");
        state.insert(
            "schema_version".to_string(),
            json!("openagent.session_state.v99"),
        );
        atomic_write_private_json(&path, &Value::Object(state)).expect("future state");
        let before = fs::read(&path).expect("future bytes");
        let plan = audit_storage(&root).expect("blocked audit");
        assert_eq!(plan.blocked_reasons.get("unsupported_schema"), Some(&1));
        assert_eq!(fs::read(path).expect("unchanged bytes"), before);
        let _ = fs::remove_dir_all(root);
    }
}
