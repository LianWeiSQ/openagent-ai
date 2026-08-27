use super::*;

const MAX_WORKSPACE_MEMORY_ENTRIES: usize = 64;
const MAX_ACTIVE_CONTEXT_MEMORIES: usize = 12;

fn workspace_memory_lock() -> &'static Mutex<()> {
    static WORKSPACE_MEMORY_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    WORKSPACE_MEMORY_LOCK.get_or_init(|| Mutex::new(()))
}

fn workspace_memory_path(store: &FileSessionStore, session: &Session) -> PathBuf {
    let workspace_identity = session.directory.to_string_lossy();
    let workspace_key = canonical_json_fingerprint(&json!(workspace_identity)).replace(':', "_");
    store
        .root
        .join(".openagent-runtime")
        .join("workspace_memory")
        .join(format!("{workspace_key}.json"))
}

fn read_workspace_memory(store: &FileSessionStore, session: &Session) -> Value {
    let path = workspace_memory_path(store, session);
    let value = read_json_file(&path);
    if value.get("schema_version").and_then(Value::as_str) == Some("openagent.workspace_memory.v1")
    {
        value
    } else {
        json!({
            "schema_version": "openagent.workspace_memory.v1",
            "workspace": session.directory.to_string_lossy(),
            "workspace_key": path.file_stem().and_then(|value| value.to_str()).unwrap_or_default(),
            "revision": 0,
            "updated_at_ms": 0,
            "entries": [],
        })
    }
}

fn write_workspace_memory(
    store: &FileSessionStore,
    session: &Session,
    mut ledger: Value,
) -> Result<(), String> {
    let Some(object) = ledger.as_object_mut() else {
        return Err("workspace memory ledger must be an object".to_string());
    };
    let revision = object
        .get("revision")
        .and_then(Value::as_u64)
        .unwrap_or_default()
        .saturating_add(1);
    object.insert("revision".to_string(), json!(revision));
    object.insert("updated_at_ms".to_string(), json!(now_ms()));
    object.insert(
        "workspace".to_string(),
        json!(session.directory.to_string_lossy()),
    );
    if let Some(entries) = object.get_mut("entries").and_then(Value::as_array_mut)
        && entries.len() > MAX_WORKSPACE_MEMORY_ENTRIES
    {
        entries.drain(..entries.len() - MAX_WORKSPACE_MEMORY_ENTRIES);
    }
    write_json_value(&workspace_memory_path(store, session), &ledger)
}

pub(super) fn persist_workspace_memory_from_compaction(
    store: &FileSessionStore,
    session: &Session,
    run_id: &str,
    boundary_message_id: &str,
    summary: &str,
    state: Option<&Value>,
    automatic: bool,
) -> Result<Value, String> {
    let _guard = workspace_memory_lock()
        .lock()
        .map_err(|_| "workspace memory lock unavailable".to_string())?;
    let mut ledger = read_workspace_memory(store, session);
    let entries = ledger
        .get_mut("entries")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "workspace memory entries are invalid".to_string())?;
    if let Some(existing) = entries.iter().find(|entry| {
        entry
            .get("source_boundary_message_id")
            .and_then(Value::as_str)
            == Some(boundary_message_id)
    }) {
        return Ok(existing.clone());
    }
    let memory_id = canonical_json_fingerprint(&json!({
        "session_id": session.id,
        "boundary_message_id": boundary_message_id,
    }))
    .replace(':', "_");
    let entry = json!({
        "id": memory_id,
        "kind": "compaction_work_state",
        "content": summary,
        "state": state.cloned().unwrap_or(Value::Null),
        "active": true,
        "automatic": automatic,
        "created_at_ms": now_ms(),
        "updated_at_ms": now_ms(),
        "source_session_id": session.id,
        "source_run_id": run_id,
        "source_boundary_message_id": boundary_message_id,
        "status": "active",
    });
    entries.push(entry.clone());
    write_workspace_memory(store, session, ledger)?;
    Ok(entry)
}

pub(super) fn mark_workspace_memory_boundary_undone(
    store: &FileSessionStore,
    session: &Session,
    boundary_message_id: &str,
) -> Result<(), String> {
    let _guard = workspace_memory_lock()
        .lock()
        .map_err(|_| "workspace memory lock unavailable".to_string())?;
    let mut ledger = read_workspace_memory(store, session);
    let mut changed = false;
    if let Some(entries) = ledger.get_mut("entries").and_then(Value::as_array_mut) {
        for entry in entries {
            if entry
                .get("source_boundary_message_id")
                .and_then(Value::as_str)
                == Some(boundary_message_id)
                && let Some(object) = entry.as_object_mut()
            {
                object.insert("active".to_string(), json!(false));
                object.insert("status".to_string(), json!("source_compaction_undone"));
                object.insert("updated_at_ms".to_string(), json!(now_ms()));
                changed = true;
            }
        }
    }
    if changed {
        write_workspace_memory(store, session, ledger)?;
    }
    Ok(())
}

pub(super) fn runtime_workspace_memory_context_items(
    store: &FileSessionStore,
    session: &Session,
) -> Vec<ContextItem> {
    let ledger = read_workspace_memory(store, session);
    let mut entries = ledger
        .get("entries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("active").and_then(Value::as_bool) == Some(true))
        .filter(|entry| {
            entry
                .get("content")
                .and_then(Value::as_str)
                .is_some_and(|content| !content.trim().is_empty())
        })
        .cloned()
        .collect::<Vec<_>>();
    if entries.len() > MAX_ACTIVE_CONTEXT_MEMORIES {
        entries.drain(..entries.len() - MAX_ACTIVE_CONTEXT_MEMORIES);
    }
    entries
        .into_iter()
        .filter_map(|entry| {
            let memory_id = entry.get("id")?.as_str()?.to_string();
            let content = entry.get("content")?.as_str()?.trim();
            let mut item = ContextItem::new(
                format!("workspace_memory:{memory_id}"),
                "memory",
                "runtime.workspace_memory",
                format!("<workspace_memory id=\"{memory_id}\">\n{content}\n</workspace_memory>"),
                CONTEXT_PRIORITY_WORK_STATE - 1,
            );
            item.stable_prefix = true;
            item.metadata
                .insert("memory_id".to_string(), json!(memory_id));
            for key in [
                "source_session_id",
                "source_run_id",
                "source_boundary_message_id",
                "created_at_ms",
                "automatic",
            ] {
                if let Some(value) = entry.get(key) {
                    item.metadata.insert(key.to_string(), value.clone());
                }
            }
            Some(item)
        })
        .collect()
}

pub(super) fn workspace_memories_payload(
    config: &HttpRuntimeConfig,
    session_id: &str,
) -> Result<Value, String> {
    let store = FileSessionStore::new(session_root(config));
    let session = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    let mut ledger = read_workspace_memory(&store, &session);
    let projection = session
        .metadata
        .get("context_pack")
        .and_then(|pack| pack.get("trace"))
        .and_then(Value::as_array)
        .map(|trace| {
            trace
                .iter()
                .filter(|entry| entry.get("kind").and_then(Value::as_str) == Some("memory"))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let projection_by_id = projection
        .iter()
        .filter_map(|entry| {
            entry
                .get("item_id")
                .and_then(Value::as_str)
                .map(|id| (id.to_string(), entry.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    if let Some(entries) = ledger.get_mut("entries").and_then(Value::as_array_mut) {
        for entry in entries.iter_mut() {
            let Some(object) = entry.as_object_mut() else {
                continue;
            };
            let item_id = object
                .get("id")
                .and_then(Value::as_str)
                .map(|id| format!("workspace_memory:{id}"))
                .unwrap_or_default();
            object.insert(
                "last_context_projection".to_string(),
                projection_by_id
                    .get(&item_id)
                    .cloned()
                    .unwrap_or(Value::Null),
            );
        }
        entries.reverse();
    }
    let active_count = ledger
        .get("entries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("active").and_then(Value::as_bool) == Some(true))
        .count();
    Ok(json!({
        "schema_version": "openagent.workspace_memory_projection.v1",
        "session_id": session_id,
        "workspace": session.directory.to_string_lossy(),
        "revision": ledger.get("revision").cloned().unwrap_or_else(|| json!(0)),
        "updated_at_ms": ledger.get("updated_at_ms").cloned().unwrap_or_else(|| json!(0)),
        "active_count": active_count,
        "count": ledger.get("entries").and_then(Value::as_array).map_or(0, Vec::len),
        "entries": ledger.get("entries").cloned().unwrap_or_else(|| json!([])),
        "last_context_projection": projection,
    }))
}

pub(super) fn update_workspace_memory_payload(
    config: &HttpRuntimeConfig,
    session_id: &str,
    memory_id: &str,
    body: &str,
) -> Result<Value, String> {
    let body: Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
    let active = body
        .get("active")
        .and_then(Value::as_bool)
        .ok_or_else(|| "memory update requires boolean active".to_string())?;
    let store = FileSessionStore::new(session_root(config));
    let session = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    let _guard = workspace_memory_lock()
        .lock()
        .map_err(|_| "workspace memory lock unavailable".to_string())?;
    let mut ledger = read_workspace_memory(&store, &session);
    let entry = ledger
        .get_mut("entries")
        .and_then(Value::as_array_mut)
        .and_then(|entries| {
            entries
                .iter_mut()
                .find(|entry| entry.get("id").and_then(Value::as_str) == Some(memory_id))
        })
        .ok_or_else(|| "workspace memory not found".to_string())?;
    if let Some(object) = entry.as_object_mut() {
        object.insert("active".to_string(), json!(active));
        object.insert(
            "status".to_string(),
            json!(if active { "active" } else { "disabled" }),
        );
        object.insert("updated_at_ms".to_string(), json!(now_ms()));
    }
    write_workspace_memory(&store, &session, ledger)?;
    let run_id = new_id("memory_update");
    store
        .record_event(
            session_id,
            &run_id,
            "memory.updated",
            SessionEventOptions {
                kind: "context".to_string(),
                attributes: BTreeMap::from([
                    ("memory_id".to_string(), json!(memory_id)),
                    ("active".to_string(), json!(active)),
                ]),
                ..SessionEventOptions::default()
            },
        )
        .map_err(|error| error.to_string())?;
    workspace_memories_payload(config, session_id)
}
