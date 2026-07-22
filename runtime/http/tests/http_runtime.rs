use std::{
    error::Error,
    fs,
    io::{ErrorKind, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU16, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use openagent_bridge_server_client::{RemoteAuth, RemoteRuntimeClient};
use openagent_eval::{
    ExplorationQualityResult, ExplorationQualityRubric, compare_exploration_quality,
    exploration_observation_from_bridge_turn, score_exploration_quality,
};
use openagent_http_runtime::http_runtime_fixture;
use openagent_session::FileSessionStore;
use serde_json::Value;

type FakeProviderServer = (u16, thread::JoinHandle<()>, Arc<Mutex<Vec<String>>>);
type FakeDocsServer = (u16, thread::JoinHandle<()>);
type FakeMcpServer = (u16, thread::JoinHandle<()>, Arc<Mutex<Vec<String>>>);

#[test]
fn http_runtime_fixture_matches_legacy_oracle() -> Result<(), Box<dyn Error>> {
    let fixture = read_fixture()?;
    assert_eq!(http_runtime_fixture(), fixture);
    Ok(())
}

#[test]
fn binary_health_json_smoke_matches_docker_contract() -> Result<(), Box<dyn Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_openagent-http-runtime"))
        .arg("--health-json")
        .output()?;
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stderr)?, "");
    let payload: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        payload,
        section(&read_fixture()?, "docker")["expected_stdout_json"]
    );
    Ok(())
}

#[test]
fn dockerfile_matches_smoke_contract() -> Result<(), Box<dyn Error>> {
    let dockerfile = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Dockerfile.openagent-http-runtime"),
    )?;
    let lines = dockerfile
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(Value::from)
        .collect::<Vec<_>>();
    assert_eq!(
        Value::Array(lines),
        section(&read_fixture()?, "docker")["dockerfile"]
    );
    Ok(())
}

#[test]
fn bridge_http_routes_are_api_only_and_cover_sse_auth_and_tui_control() -> Result<(), Box<dyn Error>>
{
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-routes")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let mut server = spawn_runtime(port, &workspace, &session_root)?;
    wait_for_server(port)?;

    let unauthorized = http_request(port, "GET", "/api/health", &[], "")?;
    assert!(unauthorized.starts_with("HTTP/1.1 401"));
    assert!(unauthorized.contains("WWW-Authenticate: Bearer"));

    let basic = http_request(
        port,
        "GET",
        "/api/health",
        &[("Authorization", "Basic b3BlbmFnZW50OnBhc3M=")],
        "",
    )?;
    assert_eq!(json_body(&basic)?["ok"], true);

    let index = authorized_request(port, "GET", "/", "", true)?;
    assert!(index.starts_with("HTTP/1.1 404"));
    let unknown = authorized_request(port, "GET", "/bridge-console.js", "", true)?;
    assert!(unknown.starts_with("HTTP/1.1 404"));

    let created = json_body(&authorized_request(
        port,
        "POST",
        "/api/sessions",
        &format!("{{\"cwd\":\"{}\"}}", workspace.to_string_lossy()),
        false,
    )?)?;
    let session_id = created["session_id"].as_str().expect("session id");
    let started = json_body(&authorized_request(
        port,
        "POST",
        &format!("/api/sessions/{session_id}/turns"),
        "{\"input\":\"hello over bridge\"}",
        false,
    )?)?;
    let turn_id = started["turn_id"].as_str().expect("turn id");

    let turn_events = authorized_request(
        port,
        "GET",
        &format!("/api/turns/{turn_id}/events"),
        "",
        true,
    )?;
    assert!(turn_events.contains("content-type: text/event-stream"));
    assert!(turn_events.contains("event: item/agentMessage/delta"));
    assert!(turn_events.contains("event: turn/completed"));

    let global_events = authorized_request(port, "GET", "/api/events?last_event_id=0", "", true)?;
    assert!(global_events.contains("event: turn/completed"));

    let interrupted = json_body(&authorized_request(
        port,
        "POST",
        &format!("/api/turns/{turn_id}/interrupt"),
        "",
        false,
    )?)?;
    assert_eq!(interrupted["status"], "interrupted");

    let queued = json_body(&authorized_request(
        port,
        "POST",
        "/tui/append-prompt",
        "{\"text\":\"queued prompt\"}",
        false,
    )?)?;
    assert_eq!(queued["queued"], true);
    let next = json_body(&authorized_request(
        port,
        "GET",
        "/tui/control/next",
        "",
        false,
    )?)?;
    assert_eq!(next["path"], "/tui/append-prompt");
    assert_eq!(next["body"]["text"], "queued prompt");

    let _ = server.kill();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn cors_allowlist_reflects_allowed_origin_and_rejects_unknown_origin() -> Result<(), Box<dyn Error>>
{
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-cors-allowlist")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let mut server = spawn_runtime(port, &workspace, &session_root)?;
    wait_for_server(port)?;

    let allowed = http_request(
        port,
        "GET",
        "/api/health",
        &[
            ("Authorization", "Bearer secret"),
            ("Origin", "http://client.test"),
        ],
        "",
    )?;
    assert!(allowed.starts_with("HTTP/1.1 200"));
    assert!(allowed.contains("Access-Control-Allow-Origin: http://client.test"));
    assert!(allowed.contains("Vary: Origin"));

    let denied = http_request(
        port,
        "GET",
        "/api/health",
        &[
            ("Authorization", "Bearer secret"),
            ("Origin", "https://evil.example"),
        ],
        "",
    )?;
    assert!(denied.starts_with("HTTP/1.1 403"));
    assert!(denied.contains("cors_origin_denied"));
    assert!(!denied.contains("Access-Control-Allow-Origin"));

    let denied_preflight = http_request(
        port,
        "OPTIONS",
        "/api/health",
        &[("Origin", "https://evil.example")],
        "",
    )?;
    assert!(denied_preflight.starts_with("HTTP/1.1 403"));

    let denied_sse = http_request(
        port,
        "GET",
        "/api/events?live_timeout_ms=250",
        &[
            ("Authorization", "Bearer secret"),
            ("Accept", "text/event-stream"),
            ("Origin", "https://evil.example"),
        ],
        "",
    )?;
    assert!(denied_sse.starts_with("HTTP/1.1 403"));

    let _ = server.kill();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn files_route_uses_source_only_scan_profile() -> Result<(), Box<dyn Error>> {
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-files-source-only")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(workspace.join("src"))?;
    fs::write(workspace.join("src").join("main.rs"), "fn main() {}\n")?;
    for runtime_dir in [
        "target",
        "jobs/run-1",
        ".openagent/sessions",
        ".runtime_http",
        "dist",
        "build",
        "node_modules/pkg",
        "runs",
        "__pycache__",
    ] {
        fs::create_dir_all(workspace.join(runtime_dir))?;
    }
    fs::write(workspace.join("target").join("cache.rs"), "ignored\n")?;
    fs::write(
        workspace.join("jobs").join("run-1").join("job.log"),
        "ignored\n",
    )?;
    fs::write(
        workspace
            .join(".openagent")
            .join("sessions")
            .join("session.json"),
        "ignored\n",
    )?;
    fs::write(workspace.join("dist").join("bundle.js"), "ignored\n")?;

    let mut server = spawn_runtime(port, &workspace, &session_root)?;
    wait_for_server(port)?;

    let payload = json_body(&authorized_request(
        port,
        "GET",
        "/api/files?depth=3",
        "",
        false,
    )?)?;
    let paths = payload["entries"]
        .as_array()
        .expect("entries array")
        .iter()
        .filter_map(|entry| entry["path"].as_str())
        .collect::<Vec<_>>();
    assert!(paths.contains(&"src"));
    assert!(paths.contains(&"src/main.rs"));
    for ignored in [
        "target",
        "jobs",
        ".openagent",
        ".runtime_http",
        "dist",
        "build",
        "node_modules",
        "runs",
        "__pycache__",
    ] {
        assert!(
            !paths.iter().any(|path| path.starts_with(ignored)),
            "source-only file tree leaked {ignored}: {paths:?}"
        );
    }

    let _ = server.kill();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn remote_runtime_client_round_trips_tui_approval() -> Result<(), Box<dyn Error>> {
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-client-approval")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let mut server = spawn_runtime(port, &workspace, &session_root)?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "run approved command",
        serde_json::json!({
            "permission": "PLAN_ONLY",
            "tool_call": {
                "call_id": "call_bash",
                "name": "bash",
                "input": {"command": "printf approved"}
            }
        }),
    )?;
    assert_eq!(started["status"], "waiting_approval");
    let approval = started["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| event["method"] == "turn/approval_requested")
        .and_then(|event| event["params"]["approval"].as_object())
        .cloned()
        .expect("approval");
    let mut response = Value::Object(approval);
    response["action"] = Value::String("allow".to_string());
    response["scope"] = Value::String("once".to_string());

    let resolved = client.respond_approval(&response)?;
    let events = resolved["events"].as_array().expect("resolved events");

    assert!(events.iter().any(|event| {
        event["method"] == "item/toolCall/completed" && event["params"]["output"] == "approved"
    }));
    assert!(events.iter().any(|event| {
        event["method"] == "turn/completed" && event["params"]["status"] == "completed"
    }));

    let global_events = client.global_events(0)?;
    assert!(
        global_events
            .iter()
            .any(|event| event["method"] == "turn/approval_resolved")
    );

    let _ = server.kill();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn bridge_protocol_contract_and_client_live_subscription() -> Result<(), Box<dyn Error>> {
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-protocol-contract")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let mut server = spawn_runtime(port, &workspace, &session_root)?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let protocol = client.protocol()?;
    assert_eq!(protocol["protocol"], "openagent.bridge");
    assert_eq!(protocol["protocol_version"], 1);
    assert_eq!(
        protocol["event_schema_version"],
        "openagent.bridge_event.v1"
    );
    assert!(
        protocol["compatibility"]["required_event_fields"]
            .as_array()
            .is_some_and(|fields| fields.iter().any(|field| field == "schema_version"))
    );
    assert!(
        protocol["compatibility"]["required_event_fields"]
            .as_array()
            .is_some_and(|fields| fields.iter().any(|field| field == "event_id"))
    );
    assert_eq!(
        protocol["compatibility"]["event_identity"]["field"],
        "event_id"
    );
    assert_eq!(protocol["endpoints"]["global_events"], "GET /api/events");
    assert_eq!(
        protocol["endpoints"]["retry"],
        "POST /api/turns/{turn_id}/retry"
    );
    assert!(
        protocol["event_methods"]
            .as_array()
            .is_some_and(|methods| methods.iter().any(|method| method == "turn/completed"))
    );
    assert!(
        protocol["event_methods"]
            .as_array()
            .is_some_and(|methods| methods.iter().any(|method| method == "turn/retrying"))
    );
    assert!(
        protocol["event_methods"]
            .as_array()
            .is_some_and(|methods| methods.iter().any(|method| method == "turn/fallback"))
    );

    let session_id = client.create_session(&workspace, None)?;
    let live_client = client.clone();
    let live = thread::spawn(move || {
        live_client
            .global_events_live(0, Duration::from_millis(1500))
            .map_err(|error| error.to_string())
    });
    thread::sleep(Duration::from_millis(150));

    let started = client.start_turn(
        &session_id,
        "write protocol note",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_protocol_write",
                "name": "write",
                "input": {"file_path": "protocol.txt", "content": "contract\n"}
            }
        }),
    )?;
    assert_eq!(started["status"], "completed");
    let turn_id = started["turn_id"].as_str().expect("turn id");
    let returned_completed_event_id = started["events"]
        .as_array()
        .expect("returned events")
        .iter()
        .find(|event| event["method"] == "turn/completed")
        .and_then(|event| event["event_id"].as_str())
        .expect("returned completed event_id")
        .to_string();
    assert!(
        returned_completed_event_id.starts_with(&format!("bridge_evt:{session_id}:{turn_id}:"))
    );

    let live_events = live
        .join()
        .map_err(|_| "live subscription thread panicked".to_string())?
        .map_err(|error| format!("live subscription failed: {error}"))?;
    assert!(live_events.iter().any(|event| {
        event["schema_version"] == "openagent.bridge_event.v1"
            && event["protocol_version"] == 1
            && event["method"] == "item/toolCall/completed"
            && event["params"]["call_id"] == "call_protocol_write"
    }));
    assert!(live_events.iter().any(|event| {
        event["method"] == "turn/completed" && event["global_sequence"].as_u64().is_some()
    }));
    let live_completed_event_id = live_events
        .iter()
        .find(|event| event["method"] == "turn/completed")
        .and_then(|event| event["event_id"].as_str())
        .expect("live completed event_id");
    assert_eq!(live_completed_event_id, returned_completed_event_id);

    let turn_events = client.turn_events_live(turn_id, 0, Duration::from_millis(500))?;
    assert!(turn_events.iter().any(|event| {
        event["schema_version"] == "openagent.bridge_event.v1"
            && event["protocol_version"] == 1
            && event["method"] == "turn/completed"
            && event["event_id"] == returned_completed_event_id
    }));

    let legacy_run_dir = session_root
        .join("session_legacy")
        .join("runs")
        .join("turn_legacy");
    fs::create_dir_all(&legacy_run_dir)?;
    fs::write(
        legacy_run_dir.join("app_events.jsonl"),
        serde_json::to_string(&serde_json::json!({
            "sequence": 1,
            "method": "turn/completed",
            "params": {
                "thread_id": "session_legacy",
                "turn_id": "turn_legacy",
                "status": "completed",
                "final_answer": "legacy ok"
            },
            "created_at_ms": 1781842000400u64
        }))? + "\n",
    )?;
    let legacy_events = authorized_request(
        port,
        "GET",
        "/api/turns/turn_legacy/events?last_event_id=0",
        "",
        true,
    )?;
    assert!(legacy_events.contains("event: turn/completed"));
    assert!(legacy_events.contains("legacy ok"));

    let _ = server.kill();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn remote_runtime_client_manages_session_lifecycle() -> Result<(), Box<dyn Error>> {
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-session-lifecycle")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let mut server = spawn_runtime(port, &workspace, &session_root)?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let renamed =
        client.update_session(&session_id, serde_json::json!({"title": "Alpha Session"}))?;
    assert_eq!(renamed["session"]["title"], "Alpha Session");

    let search = client.search_sessions("Alpha")?;
    assert_eq!(search.len(), 1);
    assert_eq!(search[0]["session_id"], session_id);

    let child_id = client.create_session(&workspace, Some(&session_id))?;
    let children = client.children(&session_id)?;
    assert!(
        children
            .iter()
            .any(|child| child["session_id"] == child_id && child["forked_from"] == session_id)
    );

    let share = client.share_session(&session_id)?;
    assert_eq!(share["shared"], true);
    assert!(
        share["url"]
            .as_str()
            .unwrap_or_default()
            .starts_with("openagent://share/")
    );
    let unshare = client.unshare_session(&session_id)?;
    assert_eq!(unshare["shared"], false);

    let compact = client.compact_session(&session_id)?;
    assert_eq!(compact["status"], "compacted");
    assert!(compact["summary"]["summary"].as_str().is_some());
    assert_eq!(
        compact["summary"]["schema_version"],
        "openagent.context_epoch.v1"
    );
    assert_eq!(compact["summary"]["trigger"], "manual");
    assert_eq!(compact["summary"]["compacted_message_count"], 0);
    assert!(compact["summary"]["boundary_message_id"].is_null());

    let archived = client.update_session(&session_id, serde_json::json!({"archived": true}))?;
    assert_eq!(archived["session"]["archived"], true);

    let deleted_child = client.delete_session(&child_id)?;
    assert_eq!(deleted_child["removed"], true);
    let deleted_parent = client.delete_session(&session_id)?;
    assert_eq!(deleted_parent["removed"], true);

    let _ = server.kill();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn remote_runtime_client_reads_session_transcript() -> Result<(), Box<dyn Error>> {
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-session-transcript")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let mut server = spawn_runtime(port, &workspace, &session_root)?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(&session_id, "hello transcript", serde_json::json!({}))?;
    assert_eq!(started["status"], "completed");

    let transcript = client.session_messages(&session_id, Some(2))?;
    let messages = transcript["messages"].as_array().expect("messages");
    let messages_v2 = transcript["messages_v2"].as_array().expect("messages_v2");

    assert_eq!(transcript["session_id"], session_id);
    assert_eq!(transcript["message_count"], 2);
    assert_eq!(transcript["message_v2_count"], 2);
    assert_eq!(transcript["limit"], 2);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages_v2.len(), 2);
    assert_eq!(messages[0]["index"], 0);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"], "hello transcript");
    assert_eq!(messages_v2[0]["info"]["role"], "user");
    assert_eq!(messages_v2[0]["parts"][0]["kind"], "text");
    assert_eq!(messages_v2[0]["parts"][0]["content"], "hello transcript");
    assert_eq!(messages[1]["index"], 1);
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages_v2[1]["info"]["role"], "assistant");
    assert_eq!(messages_v2[1]["parts"][0]["kind"], "text");
    assert!(
        !messages[1]["content"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .is_empty()
    );

    let _ = server.kill();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn remote_runtime_client_tracks_file_diff_undo_and_redo() -> Result<(), Box<dyn Error>> {
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-file-diff")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let mut server = spawn_runtime(port, &workspace, &session_root)?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let file_path = workspace.join("notes.txt");

    let write = client.start_turn(
        &session_id,
        "write notes",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_write_notes",
                "name": "write",
                "input": {"file_path": "notes.txt", "content": "alpha\n"}
            }
        }),
    )?;
    assert_eq!(write["status"], "completed");
    assert_eq!(fs::read_to_string(&file_path)?, "alpha\n");

    let diff = client.session_diff(&session_id)?;
    assert_eq!(diff["undo_count"], 1);
    assert_eq!(diff["redo_count"], 0);
    assert!(
        diff["latest"]["diff"]
            .as_str()
            .unwrap_or_default()
            .contains("+alpha")
    );
    assert_eq!(diff["latest"]["side_by_side"]["path"], "notes.txt");
    assert_eq!(diff["latest"]["side_by_side"]["old_label"], "a/notes.txt");
    assert_eq!(diff["latest"]["side_by_side"]["new_label"], "b/notes.txt");
    assert!(
        diff["latest"]["side_by_side"]["rows"]
            .as_array()
            .expect("side-by-side rows")
            .iter()
            .any(|row| row["kind"] == "added" && row["new"] == "alpha")
    );

    let undo = client.undo_session(&session_id)?;
    assert_eq!(undo["status"], "undone");
    assert!(!file_path.exists());
    assert_eq!(undo["redo_count"], 1);

    let redo = client.redo_session(&session_id)?;
    assert_eq!(redo["status"], "redone");
    assert_eq!(fs::read_to_string(&file_path)?, "alpha\n");
    assert_eq!(redo["undo_count"], 1);

    let edited = client.start_turn(
        &session_id,
        "edit notes",
        serde_json::json!({
            "permission": "FULL",
            "tool_calls": [
                {
                    "call_id": "call_read_notes",
                    "name": "read",
                    "input": {"file_path": "notes.txt"}
                },
                {
                    "call_id": "call_edit_notes",
                    "name": "edit",
                    "input": {
                        "file_path": "notes.txt",
                        "old_string": "alpha",
                        "new_string": "beta"
                    }
                }
            ]
        }),
    )?;
    assert_eq!(edited["status"], "completed");
    assert_eq!(fs::read_to_string(&file_path)?, "beta\n");
    let edit_diff = client.session_diff(&session_id)?;
    assert_eq!(edit_diff["undo_count"], 2);
    let edit_rows = edit_diff["latest"]["side_by_side"]["rows"]
        .as_array()
        .expect("edit side-by-side rows");
    assert!(
        edit_rows
            .iter()
            .any(|row| row["kind"] == "removed" && row["old"] == "alpha")
    );
    assert!(
        edit_rows
            .iter()
            .any(|row| row["kind"] == "added" && row["new"] == "beta")
    );

    let edit_undo = client.undo_session(&session_id)?;
    assert_eq!(edit_undo["status"], "undone");
    assert_eq!(fs::read_to_string(&file_path)?, "alpha\n");

    let edit_redo = client.redo_session(&session_id)?;
    assert_eq!(edit_redo["status"], "redone");
    assert_eq!(fs::read_to_string(&file_path)?, "beta\n");

    let _ = server.kill();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn remote_runtime_client_controls_model_agent_variant_and_thinking() -> Result<(), Box<dyn Error>> {
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-profile")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let mut server = spawn_runtime(port, &workspace, &session_root)?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;

    let models = client.models()?;
    assert!(
        models["models"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
    assert!(
        models["variants"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item == "deep"))
    );

    let agents = client.agents()?;
    assert!(
        agents["agents"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["id"] == "coder"))
    );

    let updated = client.update_session(
        &session_id,
        serde_json::json!({
            "agent": "coder",
            "model": "server-local",
            "variant": "deep",
            "thinking": "high"
        }),
    )?;
    assert_eq!(updated["session"]["metadata"]["agent"], "coder");
    assert_eq!(updated["session"]["metadata"]["variant"], "deep");
    assert_eq!(updated["session"]["metadata"]["thinking"], "high");

    let started = client.start_turn(&session_id, "profile turn", serde_json::json!({}))?;
    let turn_started = started["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| event["method"] == "turn/started")
        .expect("turn started event");
    assert_eq!(turn_started["params"]["agent"], "coder");
    assert_eq!(turn_started["params"]["model"], "server-local");
    assert_eq!(turn_started["params"]["variant"], "deep");
    assert_eq!(turn_started["params"]["thinking"], "high");
    assert_eq!(started["turn"]["agent"], "coder");
    assert_eq!(started["turn"]["variant"], "deep");
    assert_eq!(started["turn"]["trace"]["agent"], "coder");
    assert!(
        started["turn"]["usage"]["total_tokens"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );

    let override_started = client.start_turn(
        &session_id,
        "override profile",
        serde_json::json!({"agent": "reviewer", "variant": "fast", "thinking": "low"}),
    )?;
    let override_event = override_started["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| event["method"] == "turn/started")
        .expect("turn started event");
    assert_eq!(override_event["params"]["agent"], "reviewer");
    assert_eq!(override_event["params"]["variant"], "fast");
    assert_eq!(override_event["params"]["thinking"], "low");

    let session = client.get_session(&session_id)?;
    assert_eq!(session["metadata"]["agent"], "reviewer");
    assert_eq!(session["metadata"]["variant"], "fast");

    let _ = server.kill();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn runtime_agent_system_prompt_refreshes_instructions_each_turn() -> Result<(), Box<dyn Error>> {
    let first = serde_json::json!({
        "id": "resp_dynamic_http_first",
        "output_text": "first http answer",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    });
    let second = serde_json::json!({
        "id": "resp_dynamic_http_second",
        "output_text": "second http answer",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![first, second])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-dynamic-system-prompt")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(workspace.join(".openagent/agents"))?;
    fs::write(
        workspace.join(".openagent/agents/dynamic.md"),
        r#"---
id: dynamic
name: Dynamic
mode: primary
tools: ["read", "skill"]
model: gpt-dynamic-http
---
You are the HTTP dynamic profile.
"#,
    )?;
    fs::write(
        workspace.join("OPENAGENT.md"),
        "HTTP_FIRST_TURN_INSTRUCTION",
    )?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let first_turn = client.start_turn(
        &session_id,
        "first http turn",
        serde_json::json!({"agent": "dynamic"}),
    )?;
    assert_eq!(first_turn["status"], "completed");
    fs::write(
        workspace.join("OPENAGENT.md"),
        "HTTP_SECOND_TURN_INSTRUCTION",
    )?;
    let second_turn = client.start_turn(
        &session_id,
        "second http turn",
        serde_json::json!({"agent": "dynamic"}),
    )?;
    assert_eq!(second_turn["status"], "completed");
    let context = client.session_context(&session_id, Some(2))?;
    assert_eq!(
        context["latest"]["system_diagnostics"]["profile_id"],
        "dynamic"
    );
    assert!(
        context["latest"]["system_diagnostics"]["content_hash"]
            .as_str()
            .is_some_and(|hash| !hash.is_empty())
    );

    let _ = server.kill();
    let _ = server.wait();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("HTTP_FIRST_TURN_INSTRUCTION"));
    assert!(!requests[0].contains("HTTP_SECOND_TURN_INSTRUCTION"));
    assert!(requests[1].contains("HTTP_SECOND_TURN_INSTRUCTION"));
    assert!(!requests[1].contains("HTTP_FIRST_TURN_INSTRUCTION"));
    assert!(requests[1].contains("You are the HTTP dynamic profile."));
    let state: Value = serde_json::from_str(&fs::read_to_string(
        session_root.join(&session_id).join("state.latest.json"),
    )?)?;
    let receipts = state["metadata"]["context_pack_receipts"]
        .as_array()
        .expect("context pack receipts");
    assert_eq!(receipts.len(), 2);
    assert_ne!(
        receipts[0]["receipt"]["provider_input_hash"],
        receipts[1]["receipt"]["provider_input_hash"]
    );
    assert_ne!(
        receipts[0]["receipt"]["stable_prefix"]["hash"],
        receipts[1]["receipt"]["stable_prefix"]["hash"]
    );
    assert_eq!(receipts[0]["prefix_cache"]["status"], "miss");
    assert_eq!(receipts[1]["prefix_cache"]["status"], "changed");
    assert_eq!(
        receipts[1]["system_diagnostics"]["schema_version"],
        "openagent.context_system_diagnostics.v1"
    );
    assert_eq!(receipts[1]["system_diagnostics"]["profile_id"], "dynamic");
    assert!(
        receipts[1]["system_diagnostics"]["instruction_count"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    assert!(
        !receipts[1]
            .to_string()
            .contains("HTTP_SECOND_TURN_INSTRUCTION")
    );
    for turn in [&first_turn, &second_turn] {
        let run_id = turn["turn"]["id"].as_str().expect("turn id");
        let context_events = read_session_event_records(&session_root, &session_id, run_id)?
            .into_iter()
            .filter(|event| event["event"] == "context.pack_built")
            .collect::<Vec<_>>();
        assert_eq!(context_events.len(), 1);
        assert_eq!(context_events[0]["attributes"]["mode"], "active");
    }
    let mut restarted = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;
    let recovered = client.get_session(&session_id)?;
    assert_eq!(
        recovered["metadata"]["context_pack"]["receipt"],
        state["metadata"]["context_pack"]["receipt"]
    );
    assert_eq!(
        recovered["metadata"]["context_pack_receipts"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    let _ = restarted.kill();

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn bridge_provider_health_uses_runtime_provider_config_without_leaking_key()
-> Result<(), Box<dyn Error>> {
    let (provider_port, provider_thread, _provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![serde_json::json!({
            "data": [{"id": "gpt-5.5"}, {"id": "gpt-5.4"}]
        })])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-provider-health")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    let auth_file = temp.join("missing-auth.json");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "runtime-secret"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_MODEL", "gpt-5.5"),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAGENT_AUTH_FILE", auth_file.to_str().unwrap_or("")),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let cached = client.models()?;
    assert_eq!(cached["model_endpoint_checked"], false);
    assert_eq!(cached["model"], "gpt-5.5");
    assert_eq!(cached["api_key"], "set");
    assert!(!cached.to_string().contains("runtime-secret"));

    let health = client.provider_health()?;
    assert_eq!(health["provider"], "openai");
    assert_eq!(health["base_url"], provider_base_url);
    assert_eq!(health["base_url_source"], "env");
    assert_eq!(health["model"], "gpt-5.5");
    assert_eq!(health["model_source"], "env");
    assert_eq!(health["wire_api"], "responses");
    assert_eq!(health["api_key"], "set");
    assert_eq!(health["api_key_source"], "env");
    assert_eq!(health["healthy"], true);
    assert_eq!(health["model_endpoint_checked"], true);
    assert_eq!(health["model_endpoint_ok"], true);
    assert_eq!(health["model_count"], 2);
    assert_eq!(health["configured_model_available"], true);
    assert!(
        health["models"]
            .as_array()
            .is_some_and(|models| models.iter().any(|model| model["id"] == "gpt-5.4"))
    );
    assert!(!health.to_string().contains("runtime-secret"));

    provider_thread.join().expect("provider thread joins");
    let _ = server.kill();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn remote_runtime_client_uses_real_provider_endpoint_for_plain_turn() -> Result<(), Box<dyn Error>>
{
    let (provider_port, provider_thread) = spawn_fake_openai_responses_provider()?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-real-provider")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(&session_id, "ask provider", serde_json::json!({}))?;

    assert_eq!(started["status"], "completed");
    assert_eq!(started["turn"]["final_answer"], "real provider answer");
    assert_eq!(started["turn"]["usage"]["input_tokens"], 7);
    assert_eq!(started["turn"]["usage"]["output_tokens"], 3);
    assert!(
        started["events"]
            .as_array()
            .expect("events")
            .iter()
            .any(|event| event["method"] == "item/agentMessage/delta"
                && event["params"]["delta"] == "real provider answer")
    );

    let state: Value = serde_json::from_str(&fs::read_to_string(
        session_root.join(&session_id).join("state.latest.json"),
    )?)?;
    let context_pack = &state["metadata"]["context_pack"];
    assert_eq!(
        context_pack["schema_version"],
        "openagent.turn_context_pack.v1"
    );
    assert_eq!(context_pack["mode"], "active");
    assert_eq!(context_pack["run_id"], started["turn"]["id"]);
    assert_eq!(context_pack["step"], 1);
    assert_eq!(
        context_pack["receipt"]["schema_version"],
        "openagent.context_pack_receipt.v1"
    );
    assert_eq!(context_pack["receipt"]["message_count"], 1);
    assert!(
        context_pack["receipt"]["tool_manifest_count"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    assert_eq!(
        context_pack["receipt"]["item_category_counts"]["conversation"],
        1
    );
    assert_eq!(
        context_pack["receipt"]["item_origin_counts"]["session_message"],
        1
    );
    let receipts = state["metadata"]["context_pack_receipts"]
        .as_array()
        .expect("context pack receipts");
    assert_eq!(receipts, std::slice::from_ref(context_pack));
    assert!(state["metadata"].get("context_pack_shadow").is_none());
    let receipt_text = context_pack.to_string();
    assert!(!receipt_text.contains("test-key"));
    assert!(!receipt_text.contains("ask provider"));
    assert!(!receipt_text.contains("real provider answer"));
    let run_id = started["turn"]["id"].as_str().expect("turn id");
    let context_events = read_session_event_records(&session_root, &session_id, run_id)?
        .into_iter()
        .filter(|event| event["event"] == "context.pack_built")
        .collect::<Vec<_>>();
    assert_eq!(context_events.len(), 1);
    assert_eq!(context_events[0]["attributes"]["step"], 1);
    assert_eq!(context_events[0]["attributes"]["mode"], "active");
    assert_eq!(
        context_events[0]["attributes"]["receipt"],
        context_pack["receipt"]
    );
    let public_context = client.session_context(&session_id, Some(1))?;
    let trace = public_context["latest"]["trace"]
        .as_array()
        .expect("public context trace");
    assert!(trace.iter().all(|entry| {
        entry["taxonomy"]["schema_version"] == "openagent.context_item_taxonomy.v1"
    }));
    assert!(trace.iter().any(|entry| {
        entry["taxonomy"]["category"] == "conversation"
            && entry["taxonomy"]["origin"] == "session_message"
            && entry["taxonomy"]["compaction"] == "summarize"
    }));

    let _ = server.kill();
    let _ = provider_thread.join();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn active_context_budget_removes_oversized_history_from_provider_payload()
-> Result<(), Box<dyn Error>> {
    let old_answer = format!("OLD_CONTEXT_MARKER {}", "historical-output ".repeat(7_000));
    let first = serde_json::json!({
        "id": "resp_budget_history",
        "output_text": old_answer,
        "usage": {"input_tokens": 5, "output_tokens": 20}
    });
    let second = serde_json::json!({
        "id": "resp_budget_final",
        "output_text": "budgeted context answer",
        "usage": {"input_tokens": 8, "output_tokens": 3}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![first, second])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-context-budget")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let budget = serde_json::json!({
        "context_budget": {
            "context_window": 30_000,
            "reserve_output_tokens": 4_000,
            "input_safety_margin_tokens": 1_000,
            "bytes_per_token": 3
        }
    });
    let first_turn = client.start_turn(&session_id, "first request", budget.clone())?;
    assert_eq!(first_turn["status"], "completed");
    let second_turn = client.start_turn(&session_id, "LATEST_BUDGET_REQUEST", budget)?;
    assert_eq!(second_turn["status"], "completed");
    assert_eq!(
        second_turn["turn"]["final_answer"],
        "budgeted context answer"
    );

    let _ = server.kill();
    let _ = server.wait();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contains("LATEST_BUDGET_REQUEST"));
    assert!(!requests[1].contains("OLD_CONTEXT_MARKER"));
    assert!(!requests[1].contains("context_budget"));
    drop(requests);

    let state: Value = serde_json::from_str(&fs::read_to_string(
        session_root.join(&session_id).join("state.latest.json"),
    )?)?;
    let receipt = &state["metadata"]["context_pack"]["receipt"];
    assert_eq!(receipt["budget"]["enabled"], true);
    assert_eq!(receipt["budget"]["model_id"], "fake-model");
    assert_eq!(receipt["budget"]["context_window"], 30_000);
    assert_eq!(receipt["budget"]["reserved_output_tokens"], 4_000);
    assert_eq!(receipt["budget"]["input_limit_tokens"], 25_000);
    assert_eq!(receipt["drop_reason_counts"]["model_context_budget"], 1);
    assert!(
        receipt["estimated_input_tokens"]
            .as_u64()
            .is_some_and(|tokens| tokens <= 25_000)
    );

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn active_context_budget_fits_required_attachment_and_latest_user_before_provider_call()
-> Result<(), Box<dyn Error>> {
    let response = serde_json::json!({
        "id": "resp_required_context_fit",
        "output_text": "required context fitted",
        "usage": {"input_tokens": 120, "output_tokens": 4}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![response])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-required-context-fit")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(workspace.join(".openagent/agents"))?;
    fs::write(
        workspace.join(".openagent/agents/required-fit.md"),
        r#"---
id: required-fit
name: Required Fit
mode: primary
tools: ["read"]
---
PROFILE_REQUIRED_HEAD
Keep workspace instructions and the latest user request.
PROFILE_REQUIRED_TAIL
"#,
    )?;
    fs::write(
        workspace.join("OPENAGENT.md"),
        format!(
            "PROJECT_INSTRUCTION_HEAD\n{}\nPROJECT_INSTRUCTION_TAIL",
            "project-rule ".repeat(800)
        ),
    )?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let latest_user = format!(
        "LATEST_REQUIRED_HEAD\n{}UNRETAINED_USER_MIDDLE{}\nLATEST_REQUIRED_TAIL",
        "latest-left ".repeat(6_000),
        "latest-right ".repeat(6_000)
    );
    let attachment = format!(
        "ATTACHMENT_REQUIRED_HEAD\n{}UNRETAINED_ATTACHMENT_MIDDLE{}\nATTACHMENT_REQUIRED_TAIL",
        "attachment-left ".repeat(6_000),
        "attachment-right ".repeat(6_000)
    );
    let started = client.start_turn(
        &session_id,
        &latest_user,
        serde_json::json!({
            "agent": "required-fit",
            "attachments": [{
                "kind": "file",
                "path": "/workspace/large-required.md",
                "name": "large-required.md",
                "content_type": "text/markdown",
                "size_bytes": attachment.len(),
                "content": attachment
            }],
            "context_budget": {
                "strategy": "compact",
                "context_window": 12_000,
                "reserve_output_tokens": 1_000,
                "input_safety_margin_tokens": 500,
                "bytes_per_token": 3
            }
        }),
    )?;
    assert_eq!(started["status"], "completed");
    assert_eq!(started["turn"]["final_answer"], "required context fitted");

    let _ = server.kill();
    let _ = server.wait();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    for marker in [
        "PROJECT_INSTRUCTION_HEAD",
        "PROJECT_INSTRUCTION_TAIL",
        "LATEST_REQUIRED_HEAD",
        "LATEST_REQUIRED_TAIL",
        "ATTACHMENT_REQUIRED_HEAD",
        "ATTACHMENT_REQUIRED_TAIL",
        "context truncated to fit model budget",
    ] {
        assert!(request.contains(marker), "missing provider marker {marker}");
    }
    assert!(!request.contains("UNRETAINED_USER_MIDDLE"));
    assert!(!request.contains("UNRETAINED_ATTACHMENT_MIDDLE"));
    assert!(request.len() < 120_000);
    drop(requests);

    let state: Value = serde_json::from_str(&fs::read_to_string(
        session_root.join(&session_id).join("state.latest.json"),
    )?)?;
    let receipt = &state["metadata"]["context_pack"]["receipt"];
    assert_eq!(receipt["budget"]["overflowed"], false);
    assert!(
        receipt["truncated_item_count"]
            .as_u64()
            .is_some_and(|count| count >= 2)
    );
    assert!(
        receipt["truncation_reason_counts"]["required_context_budget"]
            .as_u64()
            .is_some_and(|count| count >= 2)
    );
    assert!(
        receipt["truncation_strategy_counts"]["attachment_header_head_tail"]
            .as_u64()
            .is_some_and(|count| count >= 1)
    );
    let run_id = started["turn"]["id"].as_str().expect("turn id");
    let context_events = read_session_event_records(&session_root, &session_id, run_id)?;
    assert_eq!(
        context_events
            .iter()
            .filter(|event| event["event"] == "context.auto_compacted")
            .count(),
        0
    );

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn context_budget_auto_compacts_and_rebuilds_across_restart() -> Result<(), Box<dyn Error>> {
    let responses = vec![
        serde_json::json!({
            "id": "resp_compact_history_1",
            "output_text": format!("HISTORY_ONE {}", "alpha-history ".repeat(1_500)),
            "usage": {"input_tokens": 5, "output_tokens": 2_000}
        }),
        serde_json::json!({
            "id": "resp_compact_history_2",
            "output_text": format!("HISTORY_TWO {}", "beta-history ".repeat(1_500)),
            "usage": {"input_tokens": 5, "output_tokens": 2_000}
        }),
        serde_json::json!({
            "id": "resp_compact_history_3",
            "output_text": format!("HISTORY_THREE {}", "gamma-history ".repeat(1_500)),
            "usage": {"input_tokens": 5, "output_tokens": 2_000}
        }),
        serde_json::json!({
            "id": "resp_compact_final",
            "output_text": "automatic compaction completed",
            "usage": {"input_tokens": 80, "output_tokens": 4}
        }),
        serde_json::json!({
            "id": "resp_compact_restarted",
            "output_text": "automatic compaction recovered",
            "usage": {"input_tokens": 84, "output_tokens": 4}
        }),
    ];
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(responses)?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-auto-context-compaction")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let provider_env = [
        ("OPENAI_API_KEY", "test-key"),
        ("OPENAI_BASE_URL", provider_base_url.as_str()),
        ("OPENAI_WIRE_API", "responses"),
        ("OPENAI_MODEL", "fake-model"),
    ];
    let mut server = spawn_runtime_with_env(port, &workspace, &session_root, &provider_env)?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    for turn in 1..=3 {
        let started = client.start_turn(
            &session_id,
            &format!("history request {turn}"),
            serde_json::json!({}),
        )?;
        assert_eq!(started["status"], "completed");
    }
    let compact_budget = serde_json::json!({
        "context_budget": {
            "bytes_per_token": 3,
            "compact_refresh_min_new_messages": 2,
            "compact_summary_max_output_tokens": 128,
            "context_window": 8_000,
            "input_safety_margin_tokens": 500,
            "prune_keep_recent_user_turns": 1,
            "reserve_output_tokens": 1_000,
            "strategy": "compact"
        }
    });
    let compacted = client.start_turn(
        &session_id,
        "AUTO_COMPACT_LATEST_REQUEST",
        compact_budget.clone(),
    )?;
    assert_eq!(compacted["status"], "completed");
    assert_eq!(
        compacted["turn"]["final_answer"],
        "automatic compaction completed"
    );

    let state: Value = serde_json::from_str(&fs::read_to_string(
        session_root.join(&session_id).join("state.latest.json"),
    )?)?;
    let rebuild = &state["metadata"]["context_pack"]["rebuild"];
    assert_eq!(rebuild["reason"], "history_budget_pressure");
    assert!(
        rebuild["before_receipt"]["drop_reason_counts"]["model_context_budget"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    assert_eq!(rebuild["after_receipt"]["budget"]["overflowed"], false);
    assert_eq!(rebuild["compaction"]["trigger"], "automatic");
    assert_eq!(
        state["metadata"]["compact"]["format"],
        "structured_work_state"
    );
    assert_eq!(
        state["metadata"]["compact"]["schema_version"],
        "openagent.context_epoch.v1"
    );
    assert_eq!(state["metadata"]["compact"]["trigger"], "automatic");
    let compact_run_id = compacted["turn"]["id"].as_str().expect("compact turn id");
    let context_events = read_session_event_records(&session_root, &session_id, compact_run_id)?;
    assert_eq!(
        context_events
            .iter()
            .filter(|event| event["event"] == "context.auto_compacted")
            .count(),
        1
    );
    assert_eq!(
        context_events
            .iter()
            .filter(|event| event["event"] == "context.epoch_created")
            .count(),
        1
    );
    assert_eq!(
        context_events
            .iter()
            .filter(|event| event["event"] == "context.pack_built")
            .count(),
        1
    );

    let _ = server.kill();
    let _ = server.wait();
    let mut restarted = spawn_runtime_with_env(port, &workspace, &session_root, &provider_env)?;
    wait_for_server(port)?;
    let recovered = client.start_turn(
        &session_id,
        "AFTER_COMPACTION_RESTART_REQUEST",
        compact_budget,
    )?;
    assert_eq!(recovered["status"], "completed");
    assert_eq!(
        recovered["turn"]["final_answer"],
        "automatic compaction recovered"
    );

    let _ = restarted.kill();
    let _ = restarted.wait();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 5);
    assert!(requests[3].contains("AUTO_COMPACT_LATEST_REQUEST"));
    assert!(requests[3].contains("[Structured work state]"));
    assert!(requests[3].len() < 40_000);
    assert!(requests[4].contains("AFTER_COMPACTION_RESTART_REQUEST"));
    assert!(requests[4].contains("[Structured work state]"));
    drop(requests);

    let transcript = fs::read_to_string(session_root.join(&session_id).join("transcript.jsonl"))?;
    let message_ids = transcript
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|record| record["schema_version"] == "openagent.message.v2")
        .filter_map(|record| record["info"]["id"].as_str().map(ToString::to_string))
        .collect::<Vec<_>>();
    assert_eq!(
        message_ids.len(),
        message_ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    );

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn remote_runtime_client_retries_retryable_provider_503_same_model() -> Result<(), Box<dyn Error>> {
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_http_sequence(vec![
            (
                503,
                serde_json::json!({
                    "error": {
                        "message": "Service temporarily unavailable",
                        "type": "api_error"
                    }
                }),
            ),
            (
                200,
                serde_json::json!({
                    "id": "resp_retry",
                    "output_text": "retried provider answer",
                    "usage": {"input_tokens": 3, "output_tokens": 2}
                }),
            ),
        ])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-provider-retry")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
            ("OPENAGENT_PROVIDER_RETRIES", "1"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "ask provider",
        serde_json::json!({
            "attachments": [{
                "kind": "file",
                "path": "/workspace/retry-evidence.md",
                "name": "retry-evidence.md",
                "content_type": "text/markdown",
                "size_bytes": 20,
                "content": "typed retry evidence"
            }]
        }),
    )?;

    assert_eq!(started["status"], "completed");
    assert_eq!(started["turn"]["final_answer"], "retried provider answer");
    assert!(
        started["events"]
            .as_array()
            .is_some_and(|events| events.iter().any(|event| {
                event["method"] == "turn/retrying"
                    && event["params"]["attempt"] == 2
                    && event["params"]["max_attempts"] == 2
                    && event["params"]["model"] == "fake-model"
            }))
    );
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 2);
    let first: Value = serde_json::from_str(&requests[0])?;
    let second: Value = serde_json::from_str(&requests[1])?;
    assert_eq!(first["model"], "fake-model");
    assert_eq!(second["model"], "fake-model");
    assert_eq!(first, second);
    let input = first["input"].as_array().expect("responses input");
    assert!(
        input
            .iter()
            .any(|item| { item["role"] == "user" && item["content"] == "ask provider" })
    );
    let attachment_message = input
        .iter()
        .find(|item| {
            item["role"] == "user"
                && item["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("typed retry evidence"))
        })
        .expect("typed attachment provider message");
    assert!(
        attachment_message["content"]
            .as_str()
            .is_some_and(|content| content.contains("kind=file"))
    );
    drop(requests);
    let state: Value = serde_json::from_str(&fs::read_to_string(
        session_root.join(&session_id).join("state.latest.json"),
    )?)?;
    let receipts = state["metadata"]["context_pack_receipts"]
        .as_array()
        .expect("context pack receipts");
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0]["run_id"], started["turn"]["id"]);
    assert_eq!(receipts[0]["step"], 1);
    assert_eq!(receipts[0]["prefix_cache"]["status"], "miss");
    assert_eq!(receipts[0]["prefix_cache"]["retry_reuses_pack"], true);
    assert!(
        receipts[0]["receipt"]["stable_prefix"]["hash"]
            .as_str()
            .is_some_and(|hash| hash.starts_with("sha1:"))
    );
    assert_eq!(
        receipts[0]["receipt"]["item_kind_counts"]["attachment_file"],
        1
    );
    let run_id = started["turn"]["id"].as_str().expect("turn id");
    let context_event_count = read_session_event_records(&session_root, &session_id, run_id)?
        .iter()
        .filter(|event| event["event"] == "context.pack_built")
        .count();
    assert_eq!(context_event_count, 1);

    let _ = server.kill();
    let _ = provider_thread.join();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn remote_runtime_client_falls_back_from_retryable_provider_502() -> Result<(), Box<dyn Error>> {
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_http_sequence(vec![
            (
                502,
                serde_json::json!({
                    "error": {
                        "message": "Upstream service temporarily unavailable",
                        "type": "upstream_error"
                    }
                }),
            ),
            (
                200,
                serde_json::json!({
                    "id": "resp_fallback",
                    "output_text": "fallback provider answer",
                    "usage": {"input_tokens": 3, "output_tokens": 2}
                }),
            ),
        ])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-provider-fallback")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "gpt-5.5"),
            ("OPENAGENT_PROVIDER_RETRIES", "0"),
            ("OPENAGENT_PROVIDER_FALLBACK_MODELS", "gpt-5.4,gpt-5.3"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(&session_id, "ask provider", serde_json::json!({}))?;

    assert_eq!(started["status"], "completed");
    assert_eq!(started["turn"]["final_answer"], "fallback provider answer");
    assert!(
        started["events"]
            .as_array()
            .is_some_and(|events| events.iter().any(|event| {
                event["method"] == "turn/fallback"
                    && event["params"]["from_model"] == "gpt-5.5"
                    && event["params"]["to_model"] == "gpt-5.4"
            }))
    );
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 2);
    let first: Value = serde_json::from_str(&requests[0])?;
    let second: Value = serde_json::from_str(&requests[1])?;
    assert_eq!(first["model"], "gpt-5.5");
    assert_eq!(second["model"], "gpt-5.4");
    assert!(!requests.iter().any(|request| request.contains("gpt-5.3")));

    let _ = server.kill();
    let _ = provider_thread.join();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn failed_async_provider_turn_can_be_retried_from_persisted_payload() -> Result<(), Box<dyn Error>>
{
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_http_sequence(vec![
            (
                502,
                serde_json::json!({
                    "error": {
                        "message": "Upstream service temporarily unavailable",
                        "type": "upstream_error"
                    }
                }),
            ),
            (
                200,
                serde_json::json!({
                    "id": "resp_manual_retry",
                    "output_text": "manual retry answer",
                    "usage": {"input_tokens": 3, "output_tokens": 2}
                }),
            ),
        ])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-provider-manual-retry")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "manual-retry-model"),
            ("OPENAGENT_PROVIDER_RETRIES", "0"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let accepted = json_body(&authorized_request(
        port,
        "POST",
        &format!("/api/sessions/{session_id}/turns"),
        &serde_json::json!({
            "input": "retry this provider request",
            "async": true,
            "stream": true,
        })
        .to_string(),
        false,
    )?)?;
    let failed_turn_id = accepted["turn_id"].as_str().expect("failed turn id");
    let mut failed = false;
    for _ in 0..40 {
        if client.turn_status(failed_turn_id)?["status"] == "failed" {
            failed = true;
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    assert!(failed, "original async turn should fail");
    let failed_events = client.turn_events_live(failed_turn_id, 0, Duration::from_millis(50))?;
    assert!(failed_events.iter().any(|event| {
        event["method"] == "turn/failed"
            && event["params"]["retryable"] == true
            && event["params"]["resumable"] == true
    }));

    let retried = json_body(&authorized_request(
        port,
        "POST",
        &format!("/api/turns/{failed_turn_id}/retry"),
        "",
        false,
    )?)?;
    assert_eq!(retried["accepted"], true);
    assert_eq!(retried["retry_of_turn_id"], failed_turn_id);
    let retried_turn_id = retried["turn_id"].as_str().expect("retried turn id");
    assert_ne!(retried_turn_id, failed_turn_id);

    let mut completed = false;
    for _ in 0..40 {
        if client.turn_status(retried_turn_id)?["status"] == "completed" {
            completed = true;
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    assert!(completed, "retried async turn should complete");
    let retried_events = client.turn_events_live(retried_turn_id, 0, Duration::from_millis(50))?;
    assert!(retried_events.iter().any(|event| {
        event["method"] == "turn/completed"
            && event["params"]["final_answer"] == "manual retry answer"
    }));
    assert_eq!(
        provider_requests.lock().expect("provider requests").len(),
        2
    );

    let _ = server.kill();
    let _ = provider_thread.join();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn remote_runtime_client_continues_provider_after_tool_call() -> Result<(), Box<dyn Error>> {
    let first = serde_json::json!({
        "id": "resp_tool_call",
        "output": [{
            "type": "function_call",
            "call_id": "call_read_notes",
            "name": "read",
            "arguments": "{\"file_path\":\"notes.txt\"}"
        }],
        "usage": {"input_tokens": 5, "output_tokens": 1}
    });
    let second = serde_json::json!({
        "id": "resp_final",
        "output_text": "tool result says alpha",
        "usage": {"input_tokens": 9, "output_tokens": 4}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![first, second])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-provider-tool-loop")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join("notes.txt"), "alpha\n")?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(&session_id, "read notes", serde_json::json!({}))?;

    assert_eq!(started["status"], "completed");
    assert_eq!(started["turn"]["final_answer"], "tool result says alpha");
    assert_eq!(started["turn"]["usage"]["input_tokens"], 14);
    assert_eq!(started["turn"]["usage"]["output_tokens"], 5);
    assert_eq!(started["turn"]["usage"]["tool_calls"], 1);
    let events = started["events"].as_array().expect("events");
    assert!(events.iter().any(|event| {
        event["method"] == "item/toolCall/completed"
            && event["params"]["call_id"] == "call_read_notes"
            && event["params"]["output"]
                .as_str()
                .is_some_and(|value| value.contains("alpha"))
    }));
    assert!(events.iter().any(|event| {
        event["method"] == "item/agentMessage/delta"
            && event["params"]["delta"] == "tool result says alpha"
    }));
    let context = client.session_context(&session_id, Some(4))?;
    assert_eq!(context["status"], "ready");
    assert!(
        context["latest"]["receipt"]["tool_manifest_count"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    let global_events = client.global_events(0)?;
    assert!(global_events.iter().any(|event| {
        event["method"] == "context/updated"
            && event["params"]["session_id"] == session_id
            && event["params"]["diagnostics"]["receipt"]["pack_hash"].is_string()
    }));

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contains("function_call_output"));
    assert!(requests[1].contains("alpha"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn context_pack_representative_repository_exploration_meets_quality_baseline()
-> Result<(), Box<dyn Error>> {
    let prepare_todo = serde_json::json!({
        "id": "resp_quality_prepare_todo",
        "output": [{
            "type": "function_call",
            "call_id": "call_quality_todo",
            "name": "todowrite",
            "arguments": serde_json::json!({
                "todos": [{
                    "id": "todo-context-audit",
                    "content": "Trace ContextPackBuilder through the provider boundary",
                    "status": "in_progress",
                    "priority": "high"
                }]
            }).to_string()
        }],
        "usage": {"input_tokens": 5, "output_tokens": 2}
    });
    let prepare_final = serde_json::json!({
        "id": "resp_quality_prepare_final",
        "output_text": "audit context prepared",
        "usage": {"input_tokens": 8, "output_tokens": 3}
    });
    let explore_tools = serde_json::json!({
        "id": "resp_quality_explore_tools",
        "output": [
            {
                "type": "function_call",
                "call_id": "call_quality_ls",
                "name": "ls",
                "arguments": "{\"path\":\".\"}"
            },
            {
                "type": "function_call",
                "call_id": "call_quality_grep",
                "name": "grep",
                "arguments": "{\"pattern\":\"ContextPackBuilder\",\"path\":\".\",\"include\":\"*.rs\"}"
            },
            {
                "type": "function_call",
                "call_id": "call_quality_read_readme",
                "name": "read",
                "arguments": "{\"file_path\":\"README.md\"}"
            },
            {
                "type": "function_call",
                "call_id": "call_quality_read_manifest",
                "name": "read",
                "arguments": "{\"file_path\":\"Cargo.toml\"}"
            },
            {
                "type": "function_call",
                "call_id": "call_quality_read_core",
                "name": "read",
                "arguments": "{\"file_path\":\"src/core.rs\"}"
            },
            {
                "type": "function_call",
                "call_id": "call_quality_read_http",
                "name": "read",
                "arguments": "{\"file_path\":\"runtime/http/src/http_runtime.rs\"}"
            }
        ],
        "usage": {"input_tokens": 20, "output_tokens": 8}
    });
    let explore_final = serde_json::json!({
        "id": "resp_quality_explore_final",
        "output_text": concat!(
            "Evidence from README.md and Cargo.toml identifies the Rust workspace. ",
            "src/core.rs owns ContextPackBuilder, while runtime/http/src/http_runtime.rs ",
            "enforces the provider boundary. The project instruction, attachment, ",
            "preloaded skill, todo, and checkpoint were all available to the audit."
        ),
        "usage": {"input_tokens": 35, "output_tokens": 18}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![
            prepare_todo,
            prepare_final,
            explore_tools,
            explore_final,
        ])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-context-quality")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(workspace.join(".openagent/agents"))?;
    fs::create_dir_all(workspace.join(".openagent/skills/repo-audit"))?;
    fs::create_dir_all(workspace.join("src"))?;
    fs::create_dir_all(workspace.join("runtime/http/src"))?;
    fs::write(
        workspace.join(".openagent/agents/context-explorer.md"),
        r#"---
id: context-explorer
name: Context Explorer
mode: primary
permission: FULL
skills: ["repo-audit"]
tools: ["ls", "grep", "read", "todoread", "todowrite"]
steps: 8
---
Inspect repository evidence before answering and cite the owning files.
"#,
    )?;
    fs::write(
        workspace.join(".openagent/skills/repo-audit/SKILL.md"),
        r#"---
name: repo-audit
description: Evidence-first repository architecture audit
---
PRELOADED_REPO_AUDIT_SKILL: inspect manifests, entrypoints, and ownership boundaries.
"#,
    )?;
    fs::write(
        workspace.join("OPENAGENT.md"),
        concat!(
            "PROJECT_CONTEXT_RULE: read README.md, Cargo.toml, src/core.rs, and ",
            "runtime/http/src/http_runtime.rs before answering. Cite evidence."
        ),
    )?;
    fs::write(
        workspace.join("README.md"),
        "# Representative OpenHarness\nRust Agent Runtime quality fixture.\n",
    )?;
    fs::write(
        workspace.join("Cargo.toml"),
        "[workspace]\nmembers = [\"src\", \"runtime/http\"]\n",
    )?;
    fs::write(
        workspace.join("src/core.rs"),
        "pub struct ContextPackBuilder;\n// Owns context assembly.\n",
    )?;
    fs::write(
        workspace.join("runtime/http/src/http_runtime.rs"),
        "fn provider_boundary() { /* accepts a verified ContextPack */ }\n",
    )?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let prepared = client.start_turn(
        &session_id,
        "Prepare the architecture audit.",
        serde_json::json!({
            "agent": "context-explorer",
            "permission": "FULL"
        }),
    )?;
    assert_eq!(prepared["status"], "completed");
    let explored = client.start_turn(
        &session_id,
        "Map the ContextPackBuilder ownership and provider path with evidence.",
        serde_json::json!({
            "agent": "context-explorer",
            "permission": "FULL",
            "attachments": [{
                "kind": "file",
                "path": "audit-focus.md",
                "name": "audit-focus.md",
                "content_type": "text/markdown",
                "size_bytes": 72,
                "content": "ATTACHMENT_FOCUS: verify context assembly and provider boundary ownership."
            }]
        }),
    )?;
    assert_eq!(explored["status"], "completed");
    let context = client.session_context(&session_id, Some(8))?;
    let observation = exploration_observation_from_bridge_turn(
        "context-pack-repository-audit",
        &explored,
        &context,
    );
    let rubric = ExplorationQualityRubric {
        case_id: "context-pack-repository-audit".to_string(),
        required_context_kinds: [
            "attachment_file",
            "checkpoint",
            "message",
            "skill_preloaded",
            "todo",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        required_available_tools: ["grep", "ls", "read", "todoread", "todowrite"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        required_files: [
            "Cargo.toml",
            "README.md",
            "runtime/http/src/http_runtime.rs",
            "src/core.rs",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        required_tools_used: ["grep", "ls", "read"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        required_answer_terms: [
            "attachment",
            "cargo.toml",
            "checkpoint",
            "contextpackbuilder",
            "project instruction",
            "provider boundary",
            "runtime/http/src/http_runtime.rs",
            "src/core.rs",
            "todo",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        forbidden_tools: ["bash", "edit", "write"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        max_failed_tool_calls: 0,
        max_duplicate_tool_calls: 0,
        minimum_score: 100.0,
    };
    let quality = score_exploration_quality(&rubric, &observation);
    assert!(
        quality.passed,
        "quality gate failed: {:?}",
        quality.failure_reasons
    );
    assert_eq!(quality.score, 100.0);
    let baseline: ExplorationQualityResult = serde_json::from_str(&fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/golden/rust_rewrite/context_pack_exploration_quality.json"),
    )?)?;
    let comparison = compare_exploration_quality(&baseline, &quality, 0.0);
    assert!(
        comparison.passed,
        "quality baseline regressed: {:?}",
        comparison.regressions
    );

    let _ = server.kill();
    let _ = server.wait();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 4);
    for marker in [
        "ATTACHMENT_FOCUS",
        "PRELOADED_REPO_AUDIT_SKILL",
        "PROJECT_CONTEXT_RULE",
        "[Checkpoint id=",
        "[Todo id=todo-context-audit",
    ] {
        assert!(
            requests[2].contains(marker),
            "representative provider request missing {marker}"
        );
    }
    for marker in [
        "Owns context assembly",
        "Representative OpenHarness",
        "accepts a verified ContextPack",
        "members",
    ] {
        assert!(
            requests[3].contains(marker),
            "tool result context missing {marker}"
        );
    }
    drop(requests);
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn context_pack_recovers_todo_checkpoint_and_work_state_after_restart() -> Result<(), Box<dyn Error>>
{
    let first = serde_json::json!({
        "id": "resp_context_todo",
        "output": [{
            "type": "function_call",
            "call_id": "call_context_todo",
            "name": "todowrite",
            "arguments": serde_json::json!({
                "todos": [{
                    "id": "todo-context",
                    "content": "Keep typed context across restart",
                    "status": "in_progress",
                    "priority": "high"
                }]
            }).to_string()
        }],
        "usage": {"input_tokens": 5, "output_tokens": 1}
    });
    let second = serde_json::json!({
        "id": "resp_context_first_final",
        "output_text": "typed context captured",
        "usage": {"input_tokens": 9, "output_tokens": 4}
    });
    let third = serde_json::json!({
        "id": "resp_context_recovered",
        "output_text": "typed context recovered",
        "usage": {"input_tokens": 8, "output_tokens": 3}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![first, second, third])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-context-recovery")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join("tracked.txt"), "context\n")?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let provider_env = [
        ("OPENAI_API_KEY", "test-key"),
        ("OPENAI_BASE_URL", provider_base_url.as_str()),
        ("OPENAI_WIRE_API", "responses"),
        ("OPENAI_MODEL", "fake-model"),
    ];
    let mut server = spawn_runtime_with_env(port, &workspace, &session_root, &provider_env)?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let first_turn = client.start_turn(&session_id, "capture context", serde_json::json!({}))?;
    assert_eq!(first_turn["status"], "completed");
    assert_eq!(first_turn["turn"]["final_answer"], "typed context captured");

    let state_before_compact: Value = serde_json::from_str(&fs::read_to_string(
        session_root.join(&session_id).join("state.latest.json"),
    )?)?;
    assert_eq!(
        state_before_compact["todos"][0]["id"],
        serde_json::json!("todo-context")
    );
    assert_eq!(
        state_before_compact["todos"][0]["status"],
        serde_json::json!("in_progress")
    );
    let initial_receipts = state_before_compact["metadata"]["context_pack_receipts"]
        .as_array()
        .expect("initial context receipts");
    assert_eq!(initial_receipts.len(), 2);
    assert_eq!(
        initial_receipts[0]["receipt"]["stable_prefix"]["hash"],
        initial_receipts[1]["receipt"]["stable_prefix"]["hash"]
    );
    assert_eq!(initial_receipts[0]["prefix_cache"]["status"], "miss");
    assert_eq!(initial_receipts[1]["prefix_cache"]["status"], "reused");
    assert_eq!(
        initial_receipts[1]["prefix_cache"]["reused_from"]["run_id"],
        first_turn["turn"]["id"]
    );
    assert_eq!(
        initial_receipts[1]["prefix_cache"]["reused_from"]["step"],
        1
    );
    {
        let requests = provider_requests.lock().expect("provider requests");
        assert_eq!(requests.len(), 2);
        assert!(requests[1].contains("[Todo id=todo-context"));
        assert!(requests[1].contains("[Checkpoint id="));
    }

    let compacted = client.compact_session(&session_id)?;
    assert_eq!(compacted["status"], "compacted");
    assert_eq!(
        compacted["summary"]["schema_version"],
        "openagent.context_epoch.v1"
    );
    assert_eq!(compacted["summary"]["trigger"], "manual");
    assert!(
        compacted["summary"]["boundary_message_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    let epochs = FileSessionStore::new(&session_root)
        .list_context_epochs(&session_id)
        .expect("context epochs load after manual compaction");
    assert_eq!(epochs.len(), 1);
    assert_eq!(epochs[0].epoch_id, compacted["summary"]["epoch_id"]);
    assert_eq!(epochs[0].compacted_message_count, 4);

    let _ = server.kill();
    let _ = server.wait();
    let mut restarted = spawn_runtime_with_env(port, &workspace, &session_root, &provider_env)?;
    wait_for_server(port)?;
    let second_turn =
        client.start_turn(&session_id, "continue after restart", serde_json::json!({}))?;
    assert_eq!(second_turn["status"], "completed");
    assert_eq!(
        second_turn["turn"]["final_answer"],
        "typed context recovered"
    );

    let _ = restarted.kill();
    let _ = restarted.wait();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 3);
    assert!(requests[2].contains("[Work state]"));
    assert!(requests[2].contains("[Todo id=todo-context"));
    assert!(requests[2].contains("[Checkpoint id="));
    drop(requests);

    let recovered: Value = serde_json::from_str(&fs::read_to_string(
        session_root.join(&session_id).join("state.latest.json"),
    )?)?;
    let receipt = &recovered["metadata"]["context_pack"]["receipt"];
    assert_eq!(receipt["item_kind_counts"]["work_state"], 1);
    assert_eq!(receipt["item_kind_counts"]["todo"], 1);
    assert_eq!(receipt["item_kind_counts"]["checkpoint"], 1);
    let receipt_text = receipt.to_string();
    assert!(!receipt_text.contains("Keep typed context across restart"));
    assert!(!receipt_text.contains("typed context captured"));
    assert!(!receipt_text.contains("test-key"));
    let recovered_receipts = recovered["metadata"]["context_pack_receipts"]
        .as_array()
        .expect("recovered context receipts");
    assert_eq!(recovered_receipts.len(), 3);
    let prefix_hashes = recovered_receipts
        .iter()
        .filter_map(|item| {
            item["receipt"]["stable_prefix"]["hash"]
                .as_str()
                .map(ToString::to_string)
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(prefix_hashes.len(), 1);
    assert_eq!(recovered_receipts[2]["prefix_cache"]["status"], "reused");
    assert_eq!(
        recovered_receipts[2]["prefix_cache"]["reused_from"]["run_id"],
        first_turn["turn"]["id"]
    );
    assert_eq!(
        recovered_receipts[2]["prefix_cache"]["reused_from"]["step"],
        2
    );

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn remote_runtime_client_provider_loop_executes_mcp_tool() -> Result<(), Box<dyn Error>> {
    let (mcp_port, mcp_thread, mcp_requests) = spawn_fake_mcp_server()?;
    let mcp_tool = "mcp_tool_remote_tools_echo";
    let first = serde_json::json!({
        "id": "resp_mcp_tool_call",
        "output": [{
            "type": "function_call",
            "call_id": "call_mcp_echo",
            "name": mcp_tool,
            "arguments": "{\"text\":\"from-provider\"}"
        }],
        "usage": {"input_tokens": 5, "output_tokens": 1}
    });
    let second = serde_json::json!({
        "id": "resp_mcp_final",
        "output_text": "mcp final answer",
        "usage": {"input_tokens": 8, "output_tokens": 4}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![first, second])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-provider-mcp-tool-loop")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mcp_config = serde_json::json!({
        "mcp": {
            "servers": {
                "remote-tools": {
                    "url": format!("http://127.0.0.1:{mcp_port}/mcp"),
                    "transport": "http",
                    "timeout_ms": 2000,
                    "enabled": true
                }
            }
        }
    })
    .to_string();
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
            ("OPENAGENT_MCP_CONFIG", mcp_config.as_str()),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "use remote MCP echo",
        serde_json::json!({"dangerously_skip_permissions": true}),
    )?;

    assert_eq!(started["status"], "completed");
    assert_eq!(started["turn"]["final_answer"], "mcp final answer");
    assert_eq!(started["turn"]["usage"]["tool_calls"], 1);
    let events = started["events"].as_array().expect("events");
    assert!(events.iter().any(|event| {
        event["method"] == "item/toolCall/completed"
            && event["params"]["call_id"] == "call_mcp_echo"
            && event["params"]["output"]
                .as_str()
                .is_some_and(|value| value.contains("mcp echo: from-provider"))
    }));
    let state: Value = serde_json::from_str(&fs::read_to_string(
        session_root.join(&session_id).join("state.latest.json"),
    )?)?;
    let context_receipts = state["metadata"]["context_pack_receipts"]
        .as_array()
        .expect("context pack receipts")
        .clone();
    assert_eq!(context_receipts.len(), 2);
    for receipt in &context_receipts {
        assert_eq!(
            receipt["receipt"]["item_kind_counts"]["mcp_tool_manifest"],
            1
        );
        assert_eq!(
            receipt["receipt"]["item_delivery_counts"]["tool_manifest"],
            1
        );
    }

    let _ = server.kill();
    let _ = provider_thread.join();
    let _ = mcp_thread.join();
    let provider_requests = provider_requests.lock().expect("provider requests");
    assert_eq!(provider_requests.len(), 2);
    assert!(provider_requests[0].contains(mcp_tool));
    assert!(provider_requests[1].contains("function_call_output"));
    assert!(provider_requests[1].contains("mcp echo: from-provider"));
    for (index, request) in provider_requests.iter().enumerate() {
        let request: Value = serde_json::from_str(request)?;
        let mut request_tool_names = request["tools"]
            .as_array()
            .expect("provider tools")
            .iter()
            .filter_map(|tool| {
                tool.get("name")
                    .or_else(|| tool.pointer("/function/name"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .collect::<Vec<_>>();
        request_tool_names.sort();
        let receipt_tool_names = context_receipts[index]["receipt"]["tool_names"]
            .as_array()
            .expect("receipt tool names")
            .iter()
            .filter_map(Value::as_str)
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(receipt_tool_names, request_tool_names);
        assert!(receipt_tool_names.iter().any(|name| name == mcp_tool));
    }
    let mcp_requests = mcp_requests.lock().expect("mcp requests");
    assert_eq!(mcp_requests.len(), 2);
    assert!(mcp_requests[0].contains("\"method\":\"tools/list\""));
    assert!(mcp_requests[1].contains("\"method\":\"tools/call\""));
    assert!(mcp_requests[1].contains("from-provider"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn remote_runtime_client_resumes_provider_after_mcp_approval_allow() -> Result<(), Box<dyn Error>> {
    let (mcp_port, mcp_thread, mcp_requests) = spawn_fake_mcp_server_with_limit(3)?;
    let mcp_tool = "mcp_tool_remote_tools_echo";
    let first = serde_json::json!({
        "id": "resp_mcp_approval",
        "output": [{
            "type": "function_call",
            "call_id": "call_mcp_echo_approval",
            "name": mcp_tool,
            "arguments": "{\"text\":\"approved-mcp\"}"
        }],
        "usage": {"input_tokens": 6, "output_tokens": 1}
    });
    let second = serde_json::json!({
        "id": "resp_mcp_approval_final",
        "output_text": "mcp approval flow completed",
        "usage": {"input_tokens": 10, "output_tokens": 4}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![first, second])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-provider-mcp-approval-resume")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mcp_config = serde_json::json!({
        "mcp": {
            "servers": {
                "remote-tools": {
                    "url": format!("http://127.0.0.1:{mcp_port}/mcp"),
                    "transport": "http",
                    "timeout_ms": 2000,
                    "enabled": true
                }
            }
        }
    })
    .to_string();
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
            ("OPENAGENT_MCP_CONFIG", mcp_config.as_str()),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "use remote MCP echo with approval",
        serde_json::json!({"permission": "PLAN_ONLY"}),
    )?;
    assert_eq!(started["status"], "waiting_approval");
    let approval = started["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| event["method"] == "turn/approval_requested")
        .and_then(|event| event["params"]["approval"].as_object())
        .cloned()
        .expect("approval event");
    assert_eq!(approval["tool_name"], serde_json::json!(mcp_tool));
    assert_eq!(
        approval["call_id"],
        serde_json::json!("call_mcp_echo_approval")
    );

    let mut response = Value::Object(approval);
    response["action"] = Value::String("allow".to_string());
    response["scope"] = Value::String("once".to_string());
    let resolved = client.respond_approval(&response)?;

    assert_eq!(resolved["status"], "completed");
    assert_eq!(
        resolved["turn"]["final_answer"],
        serde_json::json!("mcp approval flow completed")
    );
    assert_eq!(resolved["turn"]["usage"]["tool_calls"], 1);
    let events = resolved["events"].as_array().expect("resolved events");
    assert!(events.iter().any(|event| {
        event["method"] == "item/toolCall/completed"
            && event["params"]["call_id"] == "call_mcp_echo_approval"
            && event["params"]["name"] == mcp_tool
            && event["params"]["output"]
                .as_str()
                .is_some_and(|value| value.contains("mcp echo: approved-mcp"))
    }));
    assert!(events.iter().any(|event| {
        event["method"] == "turn/completed"
            && event["params"]["final_answer"] == "mcp approval flow completed"
    }));

    let session = client.get_session(&session_id)?;
    assert!(session["metadata"]["pending_approval"].is_null());
    assert!(session["metadata"]["pending_provider_turn"].is_null());
    let messages = client.session_messages(&session_id, Some(20))?;
    let approval_parts = message_parts_by_kind(&messages, "approval");
    assert!(
        approval_parts
            .iter()
            .any(|part| { part["status"] == "pending" && part["content"]["status"] == "pending" })
    );
    assert!(approval_parts.iter().any(|part| {
        part["status"] == "completed"
            && part["content"]["status"] == "allowed"
            && part["content"]["resolution"]["action"] == "allow"
    }));

    let _ = server.kill();
    let _ = provider_thread.join();
    let _ = mcp_thread.join();
    let provider_requests = provider_requests.lock().expect("provider requests");
    assert_eq!(provider_requests.len(), 2);
    assert!(provider_requests[0].contains(mcp_tool));
    assert!(provider_requests[1].contains("function_call_output"));
    assert!(provider_requests[1].contains("mcp echo: approved-mcp"));
    let mcp_requests = mcp_requests.lock().expect("mcp requests");
    assert_eq!(mcp_requests.len(), 3);
    assert!(mcp_requests[0].contains("\"method\":\"tools/list\""));
    assert!(mcp_requests[1].contains("\"method\":\"tools/list\""));
    assert!(mcp_requests[2].contains("\"method\":\"tools/call\""));
    assert!(mcp_requests[2].contains("approved-mcp"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn remote_runtime_client_executes_task_subagent_tool() -> Result<(), Box<dyn Error>> {
    let child_forbidden_tool = serde_json::json!({
        "id": "resp_child_tool_call",
        "output": [{
            "type": "function_call",
            "call_id": "call_write_forbidden",
            "name": "write",
            "arguments": "{\"file_path\":\"blocked.txt\",\"content\":\"nope\"}"
        }],
        "usage": {"input_tokens": 3, "output_tokens": 1}
    });
    let child_final = serde_json::json!({
        "id": "resp_child",
        "output_text": "runtime child answer",
        "usage": {"input_tokens": 4, "output_tokens": 2}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![child_forbidden_tool, child_final])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-task-subagent")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let agent_dir = workspace.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("deep-research.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "deep-research",
            "name": "Deep Research",
            "description": "Workspace-defined research subagent",
            "mode": "subagent",
            "permission": "READONLY",
            "prompt": "You are the Custom runtime researcher.",
            "tools": ["read"],
            "skills": ["runtime-brief"],
            "skill_roots": ["shared-skills"],
            "model": "custom-child-model",
            "max_steps": 3
        }))?,
    )?;
    let shared_skill = workspace.join("shared-skills/runtime-brief");
    fs::create_dir_all(&shared_skill)?;
    fs::write(
        shared_skill.join("SKILL.md"),
        r#"---
name: runtime-brief
description: Runtime preloaded subagent skill
---
Use runtime preloaded guidance.
"#,
    )?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let agents = client.agents()?;
    assert!(agents["agents"].as_array().is_some_and(|items| {
        items.iter().any(|agent| {
            agent["id"] == "deep-research"
                && agent["description"] == "Workspace-defined research subagent"
                && agent["model"] == "custom-child-model"
                && agent["tools"] == serde_json::json!(["read"])
                && agent["skills"] == serde_json::json!(["runtime-brief"])
                && agent["skill_roots"] == serde_json::json!(["shared-skills"])
        })
    }));
    let started = client.start_turn(
        &session_id,
        "delegate exploration",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_task",
                "name": "task",
                "input": {
                    "description": "Explore runtime fixture",
                    "prompt": "Summarize this runtime fixture.",
                    "subagent_type": "deep-research"
                }
            }
        }),
    )?;

    assert_eq!(started["status"], "completed");
    assert!(!serde_json::to_string(&started)?.contains("Use runtime preloaded guidance."));
    let events = started["events"].as_array().expect("events");
    let completed = events
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or("missing task completion")?;
    assert_eq!(
        completed["params"]["metadata"]["subagent_type"],
        "deep-research"
    );
    assert!(completed["params"]["output"].as_str().is_some_and(
        |output| output.contains("<task id=") && output.contains("runtime child answer")
    ));
    let child_session_id = completed["params"]["metadata"]["session_id"]
        .as_str()
        .ok_or("missing child session id")?;
    let child_state: Value = serde_json::from_str(&fs::read_to_string(
        session_root
            .join(child_session_id)
            .join("state.latest.json"),
    )?)?;
    assert_eq!(child_state["metadata"]["subagent"], true);
    assert_eq!(child_state["metadata"]["parent_session_id"], session_id);
    assert_eq!(child_state["metadata"]["parent_tool_call_id"], "call_task");
    assert_eq!(
        child_state["metadata"]["agent_profile"]["id"],
        "deep-research"
    );
    assert_eq!(child_state["metadata"]["model"], "custom-child-model");
    assert_eq!(child_state["metadata"]["permission"], "READONLY");
    assert_eq!(
        child_state["metadata"]["agent_profile"]["tools"],
        serde_json::json!(["read"])
    );
    assert_eq!(
        child_state["metadata"]["skills"],
        serde_json::json!(["runtime-brief"])
    );
    assert_eq!(
        child_state["metadata"]["preloaded_skills"],
        serde_json::json!(["runtime-brief"])
    );
    let child_context_receipt = child_state["metadata"]["context_pack_receipts"]
        .as_array()
        .and_then(|receipts| receipts.last())
        .ok_or("missing child context pack receipt")?;
    assert_eq!(
        child_context_receipt["receipt"]["item_kind_counts"]["skill_preloaded"],
        1
    );
    assert!(
        child_context_receipt["receipt"]["item_delivery_counts"]["trace_only"]
            .as_u64()
            .is_some_and(|count| count >= 1)
    );
    let tasks = client.tasks(&session_id)?;
    let task = tasks
        .iter()
        .find(|task| task["session_id"] == child_session_id)
        .ok_or("missing subagent task lifecycle summary")?;
    assert_eq!(task["status"], "completed");
    assert_eq!(task["title"], "Explore runtime fixture");
    assert_eq!(task["subagent_type"], "deep-research");
    assert_eq!(task["parent_tool_call_id"], "call_task");
    assert_eq!(task["agent_profile"]["id"], "deep-research");
    assert_eq!(task["run"]["status"], "completed");
    assert!(child_state["messages"].as_array().is_some_and(|messages| {
        !messages.iter().any(|message| {
            message["role"] == "system"
                && message["content"].as_str().is_some_and(|content| {
                    content.contains("Custom runtime researcher")
                        && content.contains("<preloaded_skills>")
                        && content.contains("Use runtime preloaded guidance.")
                })
        })
    }));

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 2);
    let first_request: Value = serde_json::from_str(&requests[0])?;
    let tool_names = first_request["tools"]
        .as_array()
        .ok_or("missing tools")?
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(tool_names, vec!["read"]);
    assert!(requests[0].contains("<preloaded_skills>"));
    assert!(requests[0].contains("Use runtime preloaded guidance."));
    assert!(requests[0].contains("Summarize this runtime fixture."));
    assert!(requests[0].contains("custom-child-model"));
    assert!(requests[1].contains("function_call_output"));
    assert!(requests[1].contains("not available to this agent profile"));
    assert!(!workspace.join("blocked.txt").exists());
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn task_subagent_scout_fetches_external_docs() -> Result<(), Box<dyn Error>> {
    let (docs_port, docs_thread) = spawn_fake_docs_server(
        "Scout external docs\nEvidence: reqwest blocking client keeps tests deterministic.\n",
    )?;
    let docs_url = format!("http://127.0.0.1:{docs_port}/guide");
    let child_fetch = serde_json::json!({
        "id": "resp_scout_fetch",
        "output": [{
            "type": "function_call",
            "call_id": "call_fetch_docs",
            "name": "web_fetch",
            "arguments": serde_json::json!({"url": docs_url, "max_bytes": 8192}).to_string()
        }],
        "usage": {"input_tokens": 6, "output_tokens": 1}
    });
    let child_final = serde_json::json!({
        "id": "resp_scout_final",
        "output_text": "Scout summary: docs confirmed reqwest blocking behavior.",
        "usage": {"input_tokens": 9, "output_tokens": 4}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![child_fetch, child_final])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-scout-subagent")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let agents = client.agents()?;
    assert!(agents["agents"].as_array().is_some_and(|items| {
        items.iter().any(|agent| {
            agent["id"] == "scout"
                && agent["permission"] == "READONLY"
                && agent["tools"]
                    .as_array()
                    .is_some_and(|tools| tools.iter().any(|tool| tool == "web_fetch"))
        })
    }));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "delegate scout fetch",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_task_scout",
                "name": "task",
                "input": {
                    "description": "Research dependency docs",
                    "prompt": "Fetch the local docs and summarize the dependency behavior.",
                    "subagent_type": "scout"
                }
            }
        }),
    )?;
    assert_eq!(started["status"], "completed");
    let completed = started["events"]
        .as_array()
        .ok_or("missing events")?
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or("missing scout task completion")?;
    assert_eq!(completed["params"]["metadata"]["subagent_type"], "scout");
    assert!(
        completed["params"]["output"]
            .as_str()
            .is_some_and(|output| {
                output.contains("<task id=")
                    && output.contains("Scout summary: docs confirmed reqwest blocking behavior.")
            })
    );
    let child_session_id = completed["params"]["metadata"]["session_id"]
        .as_str()
        .ok_or("missing child session id")?;
    let child_state: Value = serde_json::from_str(&fs::read_to_string(
        session_root
            .join(child_session_id)
            .join("state.latest.json"),
    )?)?;
    assert_eq!(child_state["metadata"]["agent_profile"]["id"], "scout");
    assert_eq!(child_state["metadata"]["permission"], "READONLY");
    assert_eq!(
        child_state["metadata"]["agent_profile"]["tools"],
        serde_json::json!([
            "web_fetch",
            "read",
            "glob",
            "grep",
            "ls",
            "lsp",
            "code_search",
            "skill",
            "todoread"
        ])
    );
    let tasks = client.tasks(&session_id)?;
    assert!(tasks.iter().any(|task| {
        task["session_id"] == child_session_id
            && task["status"] == "completed"
            && task["subagent_type"] == "scout"
    }));

    let _ = server.kill();
    let _ = provider_thread.join();
    docs_thread
        .join()
        .map_err(|_| "docs fixture server panicked")?;
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 2);
    let first_request: Value = serde_json::from_str(&requests[0])?;
    let tool_names = first_request["tools"]
        .as_array()
        .ok_or("missing tools")?
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        tool_names,
        vec![
            "code_search",
            "glob",
            "grep",
            "ls",
            "lsp",
            "read",
            "skill",
            "todoread",
            "web_fetch"
        ]
    );
    assert!(requests[1].contains("function_call_output"));
    assert!(requests[1].contains("Scout external docs"));
    assert!(requests[1].contains("reqwest blocking client"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn task_subagent_runs_in_isolated_workspace() -> Result<(), Box<dyn Error>> {
    let child_write = serde_json::json!({
        "id": "resp_isolated_write",
        "output": [{
            "type": "function_call",
            "call_id": "call_write_isolated",
            "name": "write",
            "arguments": "{\"file_path\":\"isolated.txt\",\"content\":\"child\\n\"}"
        }],
        "usage": {"input_tokens": 5, "output_tokens": 1}
    });
    let child_final = serde_json::json!({
        "id": "resp_isolated_final",
        "output_text": "isolated writer done",
        "usage": {"input_tokens": 8, "output_tokens": 3}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![child_write, child_final])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-isolated-subagent")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join("parent.txt"), "parent\n")?;
    let agent_dir = workspace.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("isolated-writer.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "isolated-writer",
            "name": "Isolated Writer",
            "description": "Write-capable subagent that runs in an isolated workspace.",
            "mode": "subagent",
            "permission": "FULL",
            "prompt": "You write only inside your assigned workspace.",
            "tools": ["write"],
            "workspace_isolation": true,
            "model": "isolated-child-model",
            "max_steps": 3
        }))?,
    )?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "delegate isolated write",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_task_isolated",
                "name": "task",
                "input": {
                    "description": "Isolated write",
                    "prompt": "Write isolated.txt.",
                    "subagent_type": "isolated-writer"
                }
            }
        }),
    )?;
    assert_eq!(started["status"], "completed");
    let completed = started["events"]
        .as_array()
        .ok_or("missing events")?
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or("missing isolated task completion")?;
    assert_eq!(
        completed["params"]["metadata"]["workspace_isolation"]["enabled"],
        true
    );
    assert_eq!(
        completed["params"]["metadata"]["workspace_isolation"]["method"],
        "directory_copy"
    );
    let child_workspace = PathBuf::from(
        completed["params"]["metadata"]["workspace_isolation"]["workspace"]
            .as_str()
            .ok_or("missing isolated workspace")?,
    );
    assert!(!workspace.join("isolated.txt").exists());
    assert_eq!(
        fs::read_to_string(child_workspace.join("isolated.txt"))?,
        "child\n"
    );
    let child_session_id = completed["params"]["metadata"]["session_id"]
        .as_str()
        .ok_or("missing child session id")?;
    let child_state: Value = serde_json::from_str(&fs::read_to_string(
        session_root
            .join(child_session_id)
            .join("state.latest.json"),
    )?)?;
    assert_eq!(
        child_state["workspace"],
        child_workspace.to_string_lossy().to_string()
    );
    assert_eq!(
        child_state["metadata"]["workspace_isolation"]["source_workspace"],
        completed["params"]["metadata"]["workspace_isolation"]["source_workspace"]
    );
    let tasks = client.tasks(&session_id)?;
    let task = tasks
        .iter()
        .find(|task| task["session_id"] == child_session_id)
        .ok_or("missing isolated task summary")?;
    assert_eq!(
        task["workspace_isolation"]["workspace"],
        child_workspace.to_string_lossy().to_string()
    );
    assert_eq!(
        task["workspace"],
        child_workspace.to_string_lossy().to_string()
    );

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contains("function_call_output"));
    assert!(requests[1].contains("Wrote 6 chars"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn task_subagent_task_id_resumes_existing_child_session() -> Result<(), Box<dyn Error>> {
    let child_first = serde_json::json!({
        "id": "resp_child_resume_first",
        "output_text": "first child answer",
        "usage": {"input_tokens": 4, "output_tokens": 2}
    });
    let child_second = serde_json::json!({
        "id": "resp_child_resume_second",
        "output_text": "second child answer",
        "usage": {"input_tokens": 5, "output_tokens": 2}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![child_first, child_second])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-task-subagent-resume")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let agent_dir = workspace.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("resume-worker.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "resume-worker",
            "name": "Resume Worker",
            "description": "Subagent used to verify task_id resume",
            "mode": "subagent",
            "permission": "READONLY",
            "prompt": "You are a resumable runtime subagent.",
            "tools": ["read"],
            "model": "resume-child-model",
            "max_steps": 2
        }))?,
    )?;
    fs::write(
        agent_dir.join("other-worker.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "other-worker",
            "name": "Other Worker",
            "description": "Subagent used to reject mismatched task resumes",
            "mode": "subagent",
            "permission": "READONLY",
            "prompt": "You are not the resumable worker.",
            "tools": ["read"],
            "model": "other-child-model",
            "max_steps": 2
        }))?,
    )?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let first = client.start_turn(
        &session_id,
        "delegate resumable work",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_task_first",
                "name": "task",
                "input": {
                    "description": "Initial resumable work",
                    "prompt": "First prompt for the resumable worker.",
                    "subagent_type": "resume-worker"
                }
            }
        }),
    )?;
    let first_completed = first["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or("missing first task completion")?;
    let child_session_id = first_completed["params"]["metadata"]["session_id"]
        .as_str()
        .ok_or("missing child session id")?
        .to_string();

    let wrong_agent = client.start_turn(
        &session_id,
        "try wrong subagent resume",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_task_wrong_agent",
                "name": "task",
                "input": {
                    "description": "Wrong agent resume",
                    "prompt": "This should not run.",
                    "subagent_type": "other-worker",
                    "task_id": child_session_id.clone()
                }
            }
        }),
    )?;
    let wrong_agent_failed = wrong_agent["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/failed" && event["params"]["name"] == "task"
        })
        .ok_or("missing wrong-agent task failure")?;
    assert!(
        wrong_agent_failed["params"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("belongs to subagent resume-worker"))
    );

    let other_session_id = client.create_session(&workspace, None)?;
    let wrong_parent = client.start_turn(
        &other_session_id,
        "try wrong parent resume",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_task_wrong_parent",
                "name": "task",
                "input": {
                    "description": "Wrong parent resume",
                    "prompt": "This should not run either.",
                    "subagent_type": "resume-worker",
                    "task_id": child_session_id.clone()
                }
            }
        }),
    )?;
    let wrong_parent_failed = wrong_parent["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/failed" && event["params"]["name"] == "task"
        })
        .ok_or("missing wrong-parent task failure")?;
    assert!(
        wrong_parent_failed["params"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("task does not belong to parent session"))
    );

    let resumed = client.start_turn(
        &session_id,
        "continue resumable work",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_task_resume",
                "name": "task",
                "input": {
                    "description": "Continue resumable work",
                    "prompt": "Second prompt for the same resumable worker.",
                    "subagent_type": "resume-worker",
                    "task_id": child_session_id.clone()
                }
            }
        }),
    )?;
    let resumed_completed = resumed["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or("missing resumed task completion")?;
    assert_eq!(
        resumed_completed["params"]["metadata"]["session_id"],
        child_session_id
    );
    assert_eq!(
        resumed_completed["params"]["metadata"]["status"],
        "completed"
    );
    assert!(
        resumed_completed["params"]["output"]
            .as_str()
            .is_some_and(
                |output| output.contains("<task id=") && output.contains("second child answer")
            )
    );

    let child_state: Value = serde_json::from_str(&fs::read_to_string(
        session_root
            .join(&child_session_id)
            .join("state.latest.json"),
    )?)?;
    assert_eq!(child_state["metadata"]["parent_session_id"], session_id);
    assert_eq!(
        child_state["metadata"]["parent_tool_call_id"],
        "call_task_resume"
    );
    assert_eq!(child_state["metadata"]["task_resume_count"], 1);
    assert!(
        child_state["metadata"]["task_resumed_at_ms"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
    let messages = child_state["messages"]
        .as_array()
        .ok_or("missing child messages")?;
    let system_count = messages
        .iter()
        .filter(|message| {
            message["role"] == "system" && message["metadata"]["agent_profile"] == "resume-worker"
        })
        .count();
    assert_eq!(system_count, 0);
    assert!(messages.iter().any(|message| {
        message["role"] == "user" && message["content"] == "First prompt for the resumable worker."
    }));
    assert!(messages.iter().any(|message| {
        message["role"] == "user"
            && message["content"] == "Second prompt for the same resumable worker."
    }));

    let tasks = client.tasks(&session_id)?;
    let task = tasks
        .iter()
        .find(|task| task["session_id"] == child_session_id)
        .ok_or("missing resumed task summary")?;
    assert_eq!(task["status"], "completed");
    assert_eq!(task["title"], "Continue resumable work");
    assert_eq!(task["subagent_type"], "resume-worker");

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("First prompt for the resumable worker."));
    assert!(requests[1].contains("First prompt for the resumable worker."));
    assert!(requests[1].contains("Second prompt for the same resumable worker."));
    assert!(requests[1].contains("resume-child-model"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn task_subagent_nested_tree_and_governance_guards() -> Result<(), Box<dyn Error>> {
    let outer_calls_inner = serde_json::json!({
        "id": "resp_outer_calls_inner",
        "output": [{
            "type": "function_call",
            "call_id": "call_nested_inner",
            "name": "task",
            "arguments": "{\"description\":\"Run nested inner\",\"prompt\":\"Inner should check nested guards.\",\"subagent_type\":\"inner\"}"
        }],
        "usage": {"input_tokens": 4, "output_tokens": 1}
    });
    let inner_calls_blocked_tasks = serde_json::json!({
        "id": "resp_inner_calls_blocked_tasks",
        "output": [
            {
                "type": "function_call",
                "call_id": "call_inner_self",
                "name": "task",
                "arguments": "{\"description\":\"Inner self recursion\",\"prompt\":\"This should be blocked by self-call guard.\",\"subagent_type\":\"inner\"}"
            },
            {
                "type": "function_call",
                "call_id": "call_too_deep",
                "name": "task",
                "arguments": "{\"description\":\"Too deep recursion\",\"prompt\":\"This should be blocked by depth guard.\",\"subagent_type\":\"third\"}"
            }
        ],
        "usage": {"input_tokens": 4, "output_tokens": 2}
    });
    let inner_final = serde_json::json!({
        "id": "resp_inner_final",
        "output_text": "inner handled guard failures",
        "usage": {"input_tokens": 5, "output_tokens": 2}
    });
    let outer_final = serde_json::json!({
        "id": "resp_outer_final",
        "output_text": "outer nested answer",
        "usage": {"input_tokens": 5, "output_tokens": 2}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![
            outer_calls_inner,
            inner_calls_blocked_tasks,
            inner_final,
            outer_final,
        ])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-task-subagent-nested")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let agent_dir = workspace.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    for id in ["outer", "inner", "third"] {
        fs::write(
            agent_dir.join(format!("{id}.json")),
            serde_json::to_string_pretty(&serde_json::json!({
                "id": id,
                "name": format!("{id} worker"),
                "description": format!("{id} nested subagent"),
                "mode": "subagent",
                "permission": "FULL",
                "prompt": format!("You are the {id} nested subagent."),
                "tools": ["task"],
                "model": "nested-child-model",
                "max_steps": 4
            }))?,
        )?;
    }
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
            ("OPENAGENT_MAX_SUBAGENT_DEPTH", "2"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "delegate nested work",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_task_outer",
                "name": "task",
                "input": {
                    "description": "Run outer nested work",
                    "prompt": "Outer should call inner.",
                    "subagent_type": "outer"
                }
            }
        }),
    )?;

    assert_eq!(started["status"], "completed");
    let completed = started["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or("missing outer task completion")?;
    assert_eq!(completed["params"]["metadata"]["subagent_type"], "outer");
    assert_eq!(completed["params"]["metadata"]["task_depth"], 1);
    assert!(completed["params"]["output"].as_str().is_some_and(
        |output| output.contains("<task id=") && output.contains("outer nested answer")
    ));
    let outer_session_id = completed["params"]["metadata"]["session_id"]
        .as_str()
        .ok_or("missing outer session id")?
        .to_string();

    let payload = json_body(&authorized_request(
        port,
        "GET",
        &format!("/api/sessions/{session_id}/tasks"),
        "",
        false,
    )?)?;
    assert_eq!(payload["tasks"].as_array().map(Vec::len), Some(1));
    assert_eq!(payload["tree"].as_array().map(Vec::len), Some(1));
    assert_eq!(payload["flat_tasks"].as_array().map(Vec::len), Some(2));
    let outer = payload["tree"][0].clone();
    assert_eq!(outer["session_id"], outer_session_id);
    assert_eq!(outer["subagent_type"], "outer");
    assert_eq!(outer["task_depth"], 1);
    assert_eq!(outer["task_root_session_id"], session_id);
    assert_eq!(
        outer["task_lineage_subagents"],
        serde_json::json!(["outer"])
    );
    let inner = outer["children"]
        .as_array()
        .and_then(|children| children.first())
        .ok_or("missing nested inner task")?;
    let inner_session_id = inner["session_id"]
        .as_str()
        .ok_or("missing inner session id")?;
    assert_eq!(inner["subagent_type"], "inner");
    assert_eq!(inner["parent_session_id"], outer_session_id);
    assert_eq!(inner["task_parent_session_id"], outer_session_id);
    assert_eq!(inner["task_root_session_id"], session_id);
    assert_eq!(inner["task_depth"], 2);
    assert_eq!(
        inner["task_lineage_subagents"],
        serde_json::json!(["outer", "inner"])
    );
    assert_eq!(inner["children"].as_array().map(Vec::len), Some(0));

    let inner_state: Value = serde_json::from_str(&fs::read_to_string(
        session_root
            .join(inner_session_id)
            .join("state.latest.json"),
    )?)?;
    assert_eq!(inner_state["metadata"]["task_depth"], 2);
    assert_eq!(
        inner_state["metadata"]["task_lineage_subagents"],
        serde_json::json!(["outer", "inner"])
    );
    assert!(
        !fs::read_dir(&session_root)?.flatten().any(|entry| {
            let state_path = entry.path().join("state.latest.json");
            let Ok(raw) = fs::read_to_string(state_path) else {
                return false;
            };
            let Ok(state) = serde_json::from_str::<Value>(&raw) else {
                return false;
            };
            state["metadata"]["task_description"] == "Inner self recursion"
                || state["metadata"]["task_description"] == "Too deep recursion"
        }),
        "blocked nested task calls must not create child sessions"
    );

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 4);
    assert!(requests[0].contains("Outer should call inner."));
    assert!(requests[1].contains("Inner should check nested guards."));
    assert!(requests[1].contains("Available subagents: none."));
    assert!(requests[2].contains("subagent inner cannot call itself"));
    assert!(requests[2].contains("exceeds max subagent depth 2"));
    assert!(requests[3].contains("inner handled guard failures"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn task_subagent_loads_opencode_markdown_agent_options() -> Result<(), Box<dyn Error>> {
    let child_final = serde_json::json!({
        "id": "resp_markdown_agent",
        "output_text": "markdown agent answer",
        "usage": {"input_tokens": 4, "output_tokens": 2}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![child_final])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-opencode-agent-md")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let agent_dir = workspace.join(".opencode/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("markdown-research.md"),
        r#"---
id: markdown-research
name: Markdown Research
description: OpenCode markdown research agent
mode: subagent
permission:
  ruleset: READONLY
  skill:
    alpha: deny
skills: ["alpha"]
skill_roots: ["shared-skills"]
skill_permissions:
  beta: allow
tools: ["read"]
model: markdown-child-model
steps: 2
temperature: 0.21
top_p: 0.82
reasoning_effort: medium
options:
  skill_roots: ["must-not-leak"]
  skill_permissions:
    leaked: deny
color: cyan
---
You are the Markdown research subagent.
"#,
    )?;
    fs::write(
        agent_dir.join("hidden-worker.md"),
        r#"---
id: hidden-worker
name: Hidden Worker
description: Hidden markdown agent
mode: subagent
hidden: true
---
Hidden prompt.
"#,
    )?;
    fs::write(
        agent_dir.join("disabled-worker.md"),
        r#"---
id: disabled-worker
name: Disabled Worker
description: Disabled markdown agent
mode: subagent
disable: true
---
Disabled prompt.
"#,
    )?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let agents = client.agents()?;
    let agent_items = agents["agents"].as_array().ok_or("missing agents")?;
    let markdown_agent = agent_items
        .iter()
        .find(|agent| agent["id"] == "markdown-research")
        .ok_or("missing markdown agent")?;
    assert_eq!(markdown_agent["name"], "Markdown Research");
    assert_eq!(
        markdown_agent["description"],
        "OpenCode markdown research agent"
    );
    assert_eq!(markdown_agent["steps"], 2);
    assert_eq!(markdown_agent["max_steps"], 2);
    assert_eq!(markdown_agent["temperature"], 0.21);
    assert_eq!(markdown_agent["top_p"], 0.82);
    assert_eq!(markdown_agent["color"], "cyan");
    assert_eq!(markdown_agent["permission"], "READONLY");
    assert_eq!(markdown_agent["skills"], serde_json::json!(["alpha"]));
    assert_eq!(
        markdown_agent["skill_roots"],
        serde_json::json!(["shared-skills"])
    );
    assert_eq!(markdown_agent["skill_permissions"][0]["pattern"], "alpha");
    assert_eq!(markdown_agent["skill_permissions"][0]["action"], "deny");
    assert_eq!(markdown_agent["skill_permissions"][1]["pattern"], "beta");
    assert_eq!(markdown_agent["skill_permissions"][1]["action"], "allow");
    assert_eq!(
        markdown_agent["model_options"]["reasoning_effort"],
        "medium"
    );
    assert!(markdown_agent["model_options"].get("skill_roots").is_none());
    assert!(
        markdown_agent["model_options"]
            .get("skill_permissions")
            .is_none()
    );
    assert!(
        !agent_items
            .iter()
            .any(|agent| agent["id"] == "hidden-worker")
    );
    assert!(
        !agent_items
            .iter()
            .any(|agent| agent["id"] == "disabled-worker")
    );

    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "delegate markdown agent",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_markdown_agent",
                "name": "task",
                "input": {
                    "description": "Run markdown agent",
                    "prompt": "Use the markdown agent prompt.",
                    "subagent_type": "markdown-research"
                }
            }
        }),
    )?;
    let completed = started["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or("missing markdown task completion")?;
    assert_eq!(
        completed["params"]["metadata"]["subagent_type"],
        "markdown-research"
    );
    assert_eq!(completed["params"]["metadata"]["max_steps"], 2);
    assert_eq!(
        completed["params"]["metadata"]["model_options"]["reasoning_effort"],
        "medium"
    );
    let child_session_id = completed["params"]["metadata"]["session_id"]
        .as_str()
        .ok_or("missing child session id")?;
    let child_state: Value = serde_json::from_str(&fs::read_to_string(
        session_root
            .join(child_session_id)
            .join("state.latest.json"),
    )?)?;
    assert!(child_state["messages"].as_array().is_some_and(|messages| {
        !messages.iter().any(|message| {
            message["role"] == "system"
                && message["metadata"]["agent_profile"] == "markdown-research"
        }) && messages.iter().any(|message| {
            message["role"] == "user" && message["content"] == "Use the markdown agent prompt."
        })
    }));
    assert_eq!(child_state["metadata"]["temperature"], 0.21);
    assert_eq!(child_state["metadata"]["top_p"], 0.82);
    assert_eq!(child_state["metadata"]["color"], "cyan");

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 1);
    let provider_request: Value = serde_json::from_str(&requests[0])?;
    assert_eq!(provider_request["model"], "markdown-child-model");
    assert_eq!(provider_request["temperature"], 0.21);
    assert_eq!(provider_request["top_p"], 0.82);
    assert_eq!(provider_request["reasoning_effort"], "medium");
    let provider_request_text = requests[0].as_str();
    assert!(provider_request_text.contains("You are the Markdown research subagent."));
    assert!(provider_request_text.contains("Use the markdown agent prompt."));
    assert!(!provider_request_text.contains("skill_roots"));
    assert!(!provider_request_text.contains("skill_permissions"));
    assert!(!provider_request_text.contains("must-not-leak"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn provider_loop_skill_tool_uses_profile_skill_roots() -> Result<(), Box<dyn Error>> {
    let skill_call = serde_json::json!({
        "id": "resp_skill_call",
        "output": [{
            "type": "function_call",
            "call_id": "call_skill_list",
            "name": "skill",
            "arguments": "{\"query\":\"root\"}"
        }, {
            "type": "function_call",
            "call_id": "call_skill_rooted",
            "name": "skill",
            "arguments": "{\"name\":\"rooted\"}"
        }],
        "usage": {"input_tokens": 5, "output_tokens": 1}
    });
    let final_answer = serde_json::json!({
        "id": "resp_skill_final",
        "output_text": "Loaded rooted skill.",
        "usage": {"input_tokens": 8, "output_tokens": 2}
    });
    let denied_skill_call = serde_json::json!({
        "id": "resp_skill_denied_call",
        "output": [{
            "type": "function_call",
            "call_id": "call_skill_alpha",
            "name": "skill",
            "arguments": "{\"name\":\"alpha\"}"
        }],
        "usage": {"input_tokens": 5, "output_tokens": 1}
    });
    let denied_final_answer = serde_json::json!({
        "id": "resp_skill_denied_final",
        "output_text": "Alpha skill was denied.",
        "usage": {"input_tokens": 8, "output_tokens": 2}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![
            skill_call,
            final_answer,
            denied_skill_call,
            denied_final_answer,
        ])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-skill-roots")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let agent_dir = workspace.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("skillful.md"),
        r#"---
id: skillful
name: Skillful
description: Skill root aware primary agent
mode: primary
permission:
  ruleset: FULL
  skill:
    alpha: deny
tools: ["skill"]
skill_roots: ["shared-skills"]
---
You are the skillful primary agent.
"#,
    )?;
    let shared_skill = workspace.join("shared-skills/rooted");
    fs::create_dir_all(&shared_skill)?;
    fs::write(
        shared_skill.join("SKILL.md"),
        r#"---
name: rooted
description: Rooted HTTP skill
---
Use the HTTP rooted skill guidance.
"#,
    )?;
    let denied_skill = workspace.join("shared-skills/alpha");
    fs::create_dir_all(&denied_skill)?;
    fs::write(
        denied_skill.join("SKILL.md"),
        r#"---
name: alpha
description: Alpha HTTP skill should be hidden
---
Do not expose HTTP alpha guidance.
"#,
    )?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "load rooted skill",
        serde_json::json!({
            "agent": "skillful",
            "permission": "FULL"
        }),
    )?;
    assert_eq!(started["status"], "completed");
    assert!(started["events"].as_array().is_some_and(|events| {
        events.iter().any(|event| {
            event["method"] == "item/toolCall/completed"
                && event["params"]["name"] == "skill"
                && event["params"]["output"]
                    .as_str()
                    .is_some_and(|output| output.contains("Use the HTTP rooted skill guidance."))
        })
    }));
    let run_id = started["turn_id"].as_str().ok_or("missing run id")?;
    let session_events = read_session_event_records(&session_root, &session_id, run_id)?;
    assert!(session_events.iter().any(|event| {
        event["event"] == "skill.discovered"
            && event["kind"] == "skill"
            && event["attributes"]["query"] == "root"
            && event["attributes"]["skill_count"]
                .as_u64()
                .unwrap_or_default()
                >= 1
    }));
    assert!(session_events.iter().any(|event| {
        event["event"] == "skill.loaded"
            && event["kind"] == "skill"
            && event["attributes"]["skill_name"] == "rooted"
            && event["attributes"]["skill_dir"]
                .as_str()
                .is_some_and(|dir| dir.ends_with("shared-skills/rooted"))
            && event["attributes"]["skill_files"].as_array().is_some()
    }));
    let denied_session_id = client.create_session(&workspace, None)?;
    let denied = client.start_turn(
        &denied_session_id,
        "load alpha skill",
        serde_json::json!({
            "agent": "skillful",
            "permission": "FULL"
        }),
    )?;
    assert_eq!(denied["status"], "completed");
    let denied_events = denied["events"].as_array().ok_or("missing denied events")?;
    let failed_skill = denied_events
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/failed" && event["params"]["name"] == "skill"
        })
        .ok_or("missing denied skill event")?;
    assert_eq!(
        failed_skill["params"]["metadata"]["permission_action"],
        "deny"
    );
    assert_eq!(
        failed_skill["params"]["metadata"]["error_kind"],
        "permission_denied"
    );

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 4);
    assert!(requests[0].contains("<available_skills>"));
    assert!(requests[0].contains("<name>rooted</name>"));
    assert!(requests[0].contains("Rooted HTTP skill"));
    assert!(!requests[0].contains("<name>alpha</name>"));
    assert!(!requests[0].contains("Alpha HTTP skill should be hidden"));
    assert!(requests[2].contains("<available_skills>"));
    assert!(!requests[2].contains("<name>alpha</name>"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn skill_fork_context_routes_to_subagent_foreground_and_background() -> Result<(), Box<dyn Error>> {
    let child_final = serde_json::json!({
        "id": "resp_fork_skill_child",
        "output_text": "fork worker answer",
        "usage": {"input_tokens": 4, "output_tokens": 2}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![child_final])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-skill-fork")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let agent_dir = workspace.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("fork-worker.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "fork-worker",
            "name": "Fork Worker",
            "description": "Runs fork skills",
            "mode": "subagent",
            "permission": "READONLY",
            "prompt": "You are the fork worker.",
            "tools": ["read"],
            "model": "fork-child-model",
            "max_steps": 2
        }))?,
    )?;
    let skill_dir = workspace.join(".openagent/skills/forker");
    fs::create_dir_all(&skill_dir)?;
    fs::write(
        skill_dir.join("SKILL.md"),
        r#"---
name: forker
description: Fork foreground skill
context: fork
agent: fork-worker
---
Use forked skill guidance for {{topic}}.
"#,
    )?;
    let background_skill_dir = workspace.join(".openagent/skills/forker-bg");
    fs::create_dir_all(&background_skill_dir)?;
    fs::write(
        background_skill_dir.join("SKILL.md"),
        r#"---
name: forker-bg
description: Fork background skill
context: fork
agent: fork-worker
background: true
---
Queue forked background guidance.
"#,
    )?;

    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let foreground = client.start_turn(
        &session_id,
        "run fork skill",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_fork_skill",
                "name": "skill",
                "input": {
                    "name": "forker",
                    "arguments": {"topic": "routing"}
                }
            }
        }),
    )?;
    assert_eq!(foreground["status"], "completed");
    let foreground_text = serde_json::to_string(&foreground)?;
    assert!(foreground_text.contains("fork worker answer"));
    assert!(!foreground_text.contains("Use forked skill guidance"));
    let completed = foreground["events"]
        .as_array()
        .ok_or("missing foreground events")?
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "skill"
        })
        .ok_or("missing fork skill completion")?;
    assert_eq!(completed["params"]["metadata"]["skill_context"], "fork");
    assert_eq!(completed["params"]["metadata"]["skill_name"], "forker");
    assert_eq!(
        completed["params"]["metadata"]["skill_agent"],
        "fork-worker"
    );
    let child_session_id = completed["params"]["metadata"]["session_id"]
        .as_str()
        .ok_or("missing fork child session id")?;
    let child_state: Value = serde_json::from_str(&fs::read_to_string(
        session_root
            .join(child_session_id)
            .join("state.latest.json"),
    )?)?;
    assert!(child_state["messages"].as_array().is_some_and(|messages| {
        messages.iter().any(|message| {
            message["role"] == "user"
                && message["content"].as_str().is_some_and(|content| {
                    content.contains("Use forked skill guidance for routing.")
                })
        })
    }));

    let background = client.start_turn(
        &session_id,
        "queue fork skill",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_fork_skill_bg",
                "name": "skill",
                "input": {"name": "forker-bg"}
            }
        }),
    )?;
    assert_eq!(background["status"], "completed");
    let queued = background["events"]
        .as_array()
        .ok_or("missing background events")?
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "skill"
        })
        .ok_or("missing background fork skill completion")?;
    assert_eq!(queued["params"]["metadata"]["skill_context"], "fork");
    assert_eq!(queued["params"]["metadata"]["status"], "queued");
    assert_eq!(queued["params"]["metadata"]["background"], true);
    let background_child_id = queued["params"]["metadata"]["session_id"]
        .as_str()
        .ok_or("missing background child session id")?;
    let background_state: Value = serde_json::from_str(&fs::read_to_string(
        session_root
            .join(background_child_id)
            .join("state.latest.json"),
    )?)?;
    assert!(
        background_state["metadata"]["task_status"]
            .as_str()
            .is_some_and(|status| matches!(status, "queued" | "running"))
    );
    assert_eq!(background_state["metadata"]["background"], true);

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("Use forked skill guidance for routing."));
    assert!(requests[0].contains("fork-child-model"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn task_tool_respects_agent_task_permissions() -> Result<(), Box<dyn Error>> {
    let forged_task = serde_json::json!({
        "id": "resp_parent_forged_task",
        "output": [{
            "type": "function_call",
            "call_id": "call_task_blocked",
            "name": "task",
            "arguments": "{\"description\":\"Blocked task\",\"prompt\":\"Should not run.\",\"subagent_type\":\"blocked-worker\"}"
        }],
        "usage": {"input_tokens": 5, "output_tokens": 2}
    });
    let parent_final = serde_json::json!({
        "id": "resp_parent_final_after_denial",
        "output_text": "parent saw task denial",
        "usage": {"input_tokens": 6, "output_tokens": 3}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![forged_task, parent_final])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-task-permissions")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let agent_dir = workspace.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("limited-primary.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "limited-primary",
            "name": "Limited Primary",
            "description": "Primary agent that may only launch allowed-worker.",
            "mode": "primary",
            "permission": {
                "ruleset": "FULL",
                "task": {
                    "*": "deny",
                    "allowed-worker": "allow"
                }
            },
            "tools": ["task"]
        }))?,
    )?;
    for id in ["allowed-worker", "blocked-worker"] {
        fs::write(
            agent_dir.join(format!("{id}.json")),
            serde_json::to_string_pretty(&serde_json::json!({
                "id": id,
                "name": id,
                "description": format!("{id} subagent"),
                "mode": "subagent",
                "permission": "READONLY",
                "prompt": format!("You are {id}."),
                "tools": ["read"],
                "max_steps": 2
            }))?,
        )?;
    }
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "try a blocked subagent",
        serde_json::json!({
            "permission": "FULL",
            "agent": "limited-primary",
            "max_steps": 2
        }),
    )?;

    assert_eq!(started["status"], "completed");
    assert_eq!(started["turn"]["final_answer"], "parent saw task denial");
    let events = started["events"].as_array().expect("events");
    let failed = events
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/failed" && event["params"]["name"] == "task"
        })
        .ok_or("missing denied task event")?;
    assert_eq!(failed["params"]["metadata"]["permission_action"], "deny");
    assert_eq!(
        failed["params"]["metadata"]["permission_pattern"],
        "blocked-worker"
    );
    let tasks = client.tasks(&session_id)?;
    assert!(
        tasks.is_empty(),
        "denied task should not create a child task"
    );

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 2);
    let first_request: Value = serde_json::from_str(&requests[0])?;
    let task_tool = first_request["tools"]
        .as_array()
        .ok_or("missing tools")?
        .iter()
        .find(|tool| tool["name"] == "task" || tool["function"]["name"] == "task")
        .ok_or("missing task tool")?;
    let task_description = task_tool
        .get("description")
        .or_else(|| task_tool.pointer("/function/description"))
        .and_then(Value::as_str)
        .ok_or("missing task tool description")?;
    assert!(task_description.contains("allowed-worker"));
    assert!(!task_description.contains("blocked-worker"));
    assert!(requests[1].contains("Permission denied"));
    assert!(requests[1].contains("blocked-worker"));

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn start_turn_invokes_subagent_with_at_mention() -> Result<(), Box<dyn Error>> {
    let child_final = serde_json::json!({
        "id": "resp_manual_child",
        "output_text": "manual http child answer",
        "usage": {"input_tokens": 5, "output_tokens": 2}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![child_final])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-at-subagent")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let agent_dir = workspace.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("limited-primary.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "limited-primary",
            "name": "Limited Primary",
            "description": "Primary agent that may only launch allowed-worker.",
            "mode": "primary",
            "permission": {
                "ruleset": "FULL",
                "task": {
                    "*": "deny",
                    "allowed-worker": "allow"
                }
            },
            "tools": ["task"]
        }))?,
    )?;
    for id in ["allowed-worker", "blocked-worker"] {
        fs::write(
            agent_dir.join(format!("{id}.json")),
            serde_json::to_string_pretty(&serde_json::json!({
                "id": id,
                "name": id,
                "description": format!("{id} subagent"),
                "mode": "subagent",
                "permission": "READONLY",
                "prompt": format!("You are {id}."),
                "tools": ["read"],
                "model": "manual-child-model",
                "max_steps": 2
            }))?,
        )?;
    }
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let allowed_session_id = client.create_session(&workspace, None)?;
    let allowed = client.start_turn(
        &allowed_session_id,
        "@allowed-worker Handle this directly.",
        serde_json::json!({
            "permission": "FULL",
            "agent": "limited-primary"
        }),
    )?;
    assert_eq!(allowed["status"], "completed");
    let allowed_events = allowed["events"].as_array().expect("events");
    let completed = allowed_events
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or("missing manual task completion")?;
    assert_eq!(
        completed["params"]["metadata"]["subagent_type"],
        "allowed-worker"
    );
    assert!(
        completed["params"]["output"]
            .as_str()
            .is_some_and(|output| output.contains("manual http child answer"))
    );
    let allowed_tasks = client.tasks(&allowed_session_id)?;
    assert_eq!(allowed_tasks.len(), 1);
    assert_eq!(allowed_tasks[0]["subagent_type"], "allowed-worker");

    let denied_session_id = client.create_session(&workspace, None)?;
    let denied = client.start_turn(
        &denied_session_id,
        "@blocked-worker Should not run.",
        serde_json::json!({
            "permission": "FULL",
            "agent": "limited-primary"
        }),
    )?;
    assert_eq!(denied["status"], "completed");
    let denied_events = denied["events"].as_array().expect("events");
    let failed = denied_events
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/failed" && event["params"]["name"] == "task"
        })
        .ok_or("missing denied manual task failure")?;
    assert_eq!(failed["params"]["metadata"]["permission_action"], "deny");
    assert_eq!(
        failed["params"]["metadata"]["permission_pattern"],
        "blocked-worker"
    );
    assert!(client.tasks(&denied_session_id)?.is_empty());

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("Handle this directly."));
    assert!(requests[0].contains("manual-child-model"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn start_turn_auto_routes_prompt_to_matching_subagent_description() -> Result<(), Box<dyn Error>> {
    let child_final = serde_json::json!({
        "id": "resp_auto_scout_child",
        "output_text": "auto http scout answer",
        "usage": {"input_tokens": 5, "output_tokens": 2}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![child_final])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-auto-subagent")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "Research external dependency docs before coding.",
        serde_json::json!({"permission": "FULL"}),
    )?;

    assert_eq!(started["status"], "completed");
    let events = started["events"].as_array().expect("events");
    let completed = events
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or("missing auto task completion")?;
    assert_eq!(completed["params"]["call_id"], "auto_task_scout");
    assert_eq!(completed["params"]["metadata"]["subagent_type"], "scout");
    assert!(
        completed["params"]["output"]
            .as_str()
            .is_some_and(|output| output.contains("auto http scout answer"))
    );
    let child_session_id = completed["params"]["metadata"]["session_id"]
        .as_str()
        .ok_or("missing child session id")?;
    let child_state: Value = serde_json::from_str(&fs::read_to_string(
        session_root
            .join(child_session_id)
            .join("state.latest.json"),
    )?)?;
    assert_eq!(child_state["metadata"]["agent_profile"]["id"], "scout");
    assert_eq!(
        child_state["metadata"]["parent_tool_call_id"],
        "auto_task_scout"
    );
    assert_eq!(
        child_state["metadata"]["task_description"],
        "Auto-routed to scout"
    );
    assert!(child_state["messages"].as_array().is_some_and(|messages| {
        !messages.iter().any(|message| {
            message["role"] == "system"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("OpenAgent ScoutAgent"))
        })
    }));
    let parent_state: Value = serde_json::from_str(&fs::read_to_string(
        session_root.join(&session_id).join("state.latest.json"),
    )?)?;
    assert_eq!(
        parent_state["metadata"]["auto_subagent_route"]["subagent_type"],
        "scout"
    );

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("OpenAgent ScoutAgent"));
    assert!(requests[0].contains("Research external dependency docs before coding."));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn task_subagent_profile_max_steps_failure_propagates_to_parent() -> Result<(), Box<dyn Error>> {
    let child_tool_call = serde_json::json!({
        "id": "resp_child_tool_call",
        "output": [{
            "type": "function_call",
            "call_id": "call_read_notes",
            "name": "read",
            "arguments": "{\"file_path\":\"notes.txt\"}"
        }],
        "usage": {"input_tokens": 3, "output_tokens": 1}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![child_tool_call])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-task-subagent-max-steps")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join("notes.txt"), "alpha\n")?;
    let agent_dir = workspace.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("one-step.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "one-step",
            "name": "One Step",
            "description": "Subagent with a single provider step",
            "mode": "subagent",
            "permission": "READONLY",
            "prompt": "You are a constrained one-step reader.",
            "tools": ["read"],
            "max_steps": 1
        }))?,
    )?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "delegate one-step",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_task",
                "name": "task",
                "input": {
                    "description": "One step read",
                    "prompt": "Read notes and report back.",
                    "subagent_type": "one-step"
                }
            }
        }),
    )?;

    assert_eq!(started["status"], "completed");
    let events = started["events"].as_array().expect("events");
    let failed = events
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/failed" && event["params"]["name"] == "task"
        })
        .ok_or("missing failed task event")?;
    assert_eq!(failed["params"]["metadata"]["status"], "failed");
    assert_eq!(failed["params"]["metadata"]["max_steps"], 1);
    assert!(
        failed["params"]["error"]
            .as_str()
            .is_some_and(|value| value.contains("finished with status failed"))
    );
    let child_session_id = failed["params"]["metadata"]["session_id"]
        .as_str()
        .ok_or("missing child session id")?;
    let tasks = client.tasks(&session_id)?;
    let task = tasks
        .iter()
        .find(|task| task["session_id"] == child_session_id)
        .ok_or("missing failed subagent task lifecycle summary")?;
    assert_eq!(task["status"], "failed");
    assert_eq!(task["title"], "One step read");
    assert_eq!(task["subagent_type"], "one-step");
    assert_eq!(task["parent_tool_call_id"], "call_task");
    assert_eq!(task["max_steps"], 1);
    assert_eq!(task["run"]["status"], "failed");
    let child_state: Value = serde_json::from_str(&fs::read_to_string(
        session_root
            .join(child_session_id)
            .join("state.latest.json"),
    )?)?;
    assert_eq!(child_state["metadata"]["subagent"], true);
    assert_eq!(child_state["metadata"]["agent_profile"]["id"], "one-step");
    assert_eq!(child_state["metadata"]["max_steps"], 1);
    assert!(child_state["messages"].as_array().is_some_and(|messages| {
        messages.iter().any(|message| {
            message["role"] == "tool"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("alpha"))
        })
    }));

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("Read notes and report back."));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn task_subagent_background_true_queues_queryable_task() -> Result<(), Box<dyn Error>> {
    let child_final = serde_json::json!({
        "id": "resp_child_background",
        "output_text": "background child answer",
        "usage": {"input_tokens": 5, "output_tokens": 2}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![child_final])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-task-subagent-background")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let agent_dir = workspace.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("background-research.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "background-research",
            "name": "Background Research",
            "description": "Queued background research subagent",
            "mode": "subagent",
            "permission": "READONLY",
            "prompt": "You are a queued background researcher.",
            "tools": ["read"],
            "model": "background-child-model",
            "max_steps": 2
        }))?,
    )?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
            ("OPENAGENT_BACKGROUND_WORKER", "0"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "queue research",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_task_background",
                "name": "task",
                "input": {
                    "description": "Queue background research",
                    "prompt": "Research this in the background.",
                    "subagent_type": "background-research",
                    "background": true
                }
            }
        }),
    )?;

    assert_eq!(started["status"], "completed");
    let events = started["events"].as_array().expect("events");
    let completed = events
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or("missing queued task completion")?;
    assert_eq!(completed["params"]["metadata"]["status"], "queued");
    assert_eq!(completed["params"]["metadata"]["background"], true);
    assert!(
        completed["params"]["output"]
            .as_str()
            .is_some_and(|output| output.contains("state=\"queued\""))
    );
    let child_session_id = completed["params"]["metadata"]["session_id"]
        .as_str()
        .ok_or("missing child session id")?;
    let tasks = client.tasks(&session_id)?;
    let task = tasks
        .iter()
        .find(|task| task["session_id"] == child_session_id)
        .ok_or("missing queued background task lifecycle summary")?;
    assert_eq!(task["status"], "queued");
    assert_eq!(task["background"], true);
    assert_eq!(task["title"], "Queue background research");
    assert_eq!(task["subagent_type"], "background-research");
    assert_eq!(task["parent_tool_call_id"], "call_task_background");
    assert_eq!(task["max_steps"], 2);
    assert_eq!(task["run_status"], Value::Null);
    let child_state: Value = serde_json::from_str(&fs::read_to_string(
        session_root
            .join(child_session_id)
            .join("state.latest.json"),
    )?)?;
    assert_eq!(child_state["status"], "idle");
    assert_eq!(child_state["metadata"]["task_status"], "queued");
    assert_eq!(child_state["metadata"]["background"], true);
    assert_eq!(
        child_state["metadata"]["agent_profile"]["id"],
        "background-research"
    );
    assert!(child_state["messages"].as_array().is_some_and(|messages| {
        messages.iter().any(|message| {
            message["role"] == "user"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("Research this in the background."))
        })
    }));

    assert_eq!(
        provider_requests.lock().expect("provider requests").len(),
        0
    );
    let ran = client.run_task(&session_id, child_session_id, serde_json::json!({}))?;
    assert_eq!(ran["status"], "completed");
    assert_eq!(ran["task"]["status"], "completed");
    assert_eq!(ran["task"]["run_status"], "completed");
    assert_eq!(ran["task"]["background"], true);
    assert_eq!(
        ran["result"]["turn"]["final_answer"],
        "background child answer"
    );
    let tasks = client.tasks(&session_id)?;
    let task = tasks
        .iter()
        .find(|task| task["session_id"] == child_session_id)
        .ok_or("missing completed background task lifecycle summary")?;
    assert_eq!(task["status"], "completed");
    assert_eq!(task["run_status"], "completed");
    let child_state: Value = serde_json::from_str(&fs::read_to_string(
        session_root
            .join(child_session_id)
            .join("state.latest.json"),
    )?)?;
    assert_eq!(child_state["metadata"]["task_status"], "completed");

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("Research this in the background."));
    assert!(requests[0].contains("background-child-model"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn task_subagent_background_worker_auto_runs_queued_task() -> Result<(), Box<dyn Error>> {
    let child_final = serde_json::json!({
        "id": "resp_child_background_worker",
        "output_text": "background worker answer",
        "usage": {"input_tokens": 5, "output_tokens": 2}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![child_final])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-task-subagent-background-worker")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let agent_dir = workspace.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("worker-research.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "worker-research",
            "name": "Worker Research",
            "description": "Background worker research subagent",
            "mode": "subagent",
            "permission": "READONLY",
            "prompt": "You are an automatically scheduled background researcher.",
            "tools": ["read"],
            "model": "worker-child-model",
            "max_steps": 2
        }))?,
    )?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
            ("OPENAGENT_BACKGROUND_WORKER_POLL_MS", "20"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "queue worker research",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_task_background_worker",
                "name": "task",
                "input": {
                    "description": "Queue worker research",
                    "prompt": "Research this via the worker.",
                    "subagent_type": "worker-research",
                    "background": true
                }
            }
        }),
    )?;

    assert_eq!(started["status"], "completed");
    let completed = started["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or("missing queued task completion")?;
    assert_eq!(completed["params"]["metadata"]["status"], "queued");
    let child_session_id = completed["params"]["metadata"]["session_id"]
        .as_str()
        .ok_or("missing child session id")?;

    let task = wait_for_task_status(&client, &session_id, child_session_id, "completed")?;
    assert_eq!(task["run_status"], "completed");
    assert_eq!(task["background"], true);
    let child_state: Value = serde_json::from_str(&fs::read_to_string(
        session_root
            .join(child_session_id)
            .join("state.latest.json"),
    )?)?;
    assert_eq!(child_state["metadata"]["task_status"], "completed");
    assert_eq!(
        child_state["metadata"]["run_started_by"],
        "background_worker"
    );

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("Research this via the worker."));
    assert!(requests[0].contains("worker-child-model"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn task_subagent_run_rejects_duplicate_consumer() -> Result<(), Box<dyn Error>> {
    let child_final = serde_json::json!({
        "id": "resp_child_duplicate",
        "output_text": "duplicate guarded answer",
        "usage": {"input_tokens": 5, "output_tokens": 2}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence_with_delays(vec![(child_final, 900)])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-task-subagent-duplicate-run")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let agent_dir = workspace.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("single-consumer.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "single-consumer",
            "name": "Single Consumer",
            "description": "Queued subagent used for duplicate run tests",
            "mode": "subagent",
            "permission": "READONLY",
            "prompt": "You are a single-consumer background subagent.",
            "tools": ["read"],
            "model": "single-consumer-model",
            "max_steps": 2
        }))?,
    )?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
            ("OPENAGENT_BACKGROUND_WORKER", "0"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "queue duplicate guarded task",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_task_duplicate",
                "name": "task",
                "input": {
                    "description": "Duplicate guarded task",
                    "prompt": "Run once only.",
                    "subagent_type": "single-consumer",
                    "background": true
                }
            }
        }),
    )?;
    let completed = started["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or("missing queued task completion")?;
    let child_session_id = completed["params"]["metadata"]["session_id"]
        .as_str()
        .ok_or("missing child session id")?
        .to_string();

    let first_client = client.clone();
    let first_session_id = session_id.clone();
    let first_task_id = child_session_id.clone();
    let first = thread::spawn(move || {
        first_client.run_task(&first_session_id, &first_task_id, serde_json::json!({}))
    });
    thread::sleep(Duration::from_millis(150));
    let duplicate_error = client
        .run_task(&session_id, &child_session_id, serde_json::json!({}))
        .expect_err("duplicate task run should fail");
    assert!(
        duplicate_error.contains("task is already running")
            || duplicate_error.contains("task is not queued: running")
    );
    let first_result = first
        .join()
        .map_err(|_| "first task run thread panicked".to_string())?
        .map_err(|error| format!("first task run failed: {error}"))?;
    assert_eq!(first_result["status"], "completed");
    assert_eq!(
        first_result["result"]["turn"]["final_answer"],
        "duplicate guarded answer"
    );
    let tasks = client.tasks(&session_id)?;
    let task = tasks
        .iter()
        .find(|task| task["session_id"] == child_session_id)
        .ok_or("missing completed duplicate-guarded task")?;
    assert_eq!(task["status"], "completed");
    assert_eq!(task["run_status"], "completed");

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("Run once only."));
    assert!(requests[0].contains("single-consumer-model"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn task_subagent_run_recovers_stale_lock() -> Result<(), Box<dyn Error>> {
    let child_final = serde_json::json!({
        "id": "resp_child_stale_lock",
        "output_text": "stale lock recovered answer",
        "usage": {"input_tokens": 5, "output_tokens": 2}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![child_final])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-task-subagent-stale-lock")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let agent_dir = workspace.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("stale-lock-runner.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "stale-lock-runner",
            "name": "Stale Lock Runner",
            "description": "Queued subagent used for stale lock recovery tests",
            "mode": "subagent",
            "permission": "READONLY",
            "prompt": "You are a stale-lock recovery background subagent.",
            "tools": ["read"],
            "model": "stale-lock-model",
            "max_steps": 2
        }))?,
    )?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
            ("OPENAGENT_BACKGROUND_WORKER", "0"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "queue stale lock recoverable task",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_task_stale_lock",
                "name": "task",
                "input": {
                    "description": "Stale lock recoverable task",
                    "prompt": "Recover from an abandoned lock.",
                    "subagent_type": "stale-lock-runner",
                    "background": true
                }
            }
        }),
    )?;
    let completed = started["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or("missing queued task completion")?;
    let child_session_id = completed["params"]["metadata"]["session_id"]
        .as_str()
        .ok_or("missing child session id")?
        .to_string();
    let stale_lock_path = session_root.join(&child_session_id).join("task.run.lock");
    fs::write(
        &stale_lock_path,
        serde_json::to_string(&serde_json::json!({
            "task_id": child_session_id,
            "claimed_at_ms": 0
        }))?,
    )?;

    let ran = client.run_task(&session_id, &child_session_id, serde_json::json!({}))?;
    assert_eq!(ran["status"], "completed");
    assert_eq!(ran["task"]["status"], "completed");
    assert_eq!(
        ran["result"]["turn"]["final_answer"],
        "stale lock recovered answer"
    );
    assert!(!stale_lock_path.exists());
    let tasks = client.tasks(&session_id)?;
    let task = tasks
        .iter()
        .find(|task| task["session_id"] == child_session_id)
        .ok_or("missing recovered stale-lock task")?;
    assert_eq!(task["status"], "completed");
    assert_eq!(task["run_status"], "completed");

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("Recover from an abandoned lock."));
    assert!(requests[0].contains("stale-lock-model"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn task_subagent_cancel_rejects_later_run() -> Result<(), Box<dyn Error>> {
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-task-subagent-cancel")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let agent_dir = workspace.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("cancel-me.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "cancel-me",
            "name": "Cancel Me",
            "description": "Queued subagent used for cancel tests",
            "mode": "subagent",
            "permission": "READONLY",
            "prompt": "You are a queued subagent that should be canceled.",
            "tools": ["read"],
            "max_steps": 2
        }))?,
    )?;
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAGENT_BACKGROUND_WORKER", "0"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "queue cancelable task",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_task_cancel",
                "name": "task",
                "input": {
                    "description": "Cancelable task",
                    "prompt": "Do not actually run.",
                    "subagent_type": "cancel-me",
                    "background": true
                }
            }
        }),
    )?;
    let completed = started["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or("missing queued task completion")?;
    let child_session_id = completed["params"]["metadata"]["session_id"]
        .as_str()
        .ok_or("missing child session id")?;
    let stale_lock_path = session_root.join(child_session_id).join("task.run.lock");
    fs::write(
        &stale_lock_path,
        serde_json::to_string(&serde_json::json!({
            "task_id": child_session_id,
            "claimed_at_ms": 0
        }))?,
    )?;

    let canceled = client.cancel_task(&session_id, child_session_id)?;
    assert_eq!(canceled["status"], "canceled");
    assert_eq!(canceled["task"]["status"], "canceled");
    assert_eq!(canceled["task"]["background"], true);
    assert!(!stale_lock_path.exists());
    let tasks = client.tasks(&session_id)?;
    let task = tasks
        .iter()
        .find(|task| task["session_id"] == child_session_id)
        .ok_or("missing canceled task lifecycle summary")?;
    assert_eq!(task["status"], "canceled");
    assert_eq!(task["title"], "Cancelable task");
    let run_error = client
        .run_task(&session_id, child_session_id, serde_json::json!({}))
        .expect_err("canceled task run should fail");
    assert!(run_error.contains("task is not queued: canceled"));
    let child_state: Value = serde_json::from_str(&fs::read_to_string(
        session_root
            .join(child_session_id)
            .join("state.latest.json"),
    )?)?;
    assert_eq!(child_state["metadata"]["task_status"], "canceled");
    assert!(child_state["metadata"]["canceled_at_ms"].as_u64().is_some());

    let _ = server.kill();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn remote_runtime_client_resumes_provider_after_question_reply() -> Result<(), Box<dyn Error>> {
    let first = serde_json::json!({
        "id": "resp_question",
        "output": [{
            "type": "function_call",
            "call_id": "call_question",
            "name": "question",
            "arguments": "{\"questions\":[{\"header\":\"Confirm\",\"question\":\"Proceed?\",\"multiple\":false,\"options\":[{\"label\":\"yes\",\"description\":\"Continue\"},{\"label\":\"no\",\"description\":\"Stop\"}]}]}"
        }],
        "usage": {"input_tokens": 4, "output_tokens": 1}
    });
    let second = serde_json::json!({
        "id": "resp_final",
        "output_text": "continuing after yes",
        "usage": {"input_tokens": 8, "output_tokens": 3}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![first, second])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-provider-question-resume")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(&session_id, "ask a question", serde_json::json!({}))?;
    assert_eq!(started["status"], "waiting_question");
    let question = started["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| event["method"] == "item/question/requested")
        .and_then(|event| event["params"]["event"].as_object())
        .cloned()
        .expect("question event");
    let mut response = Value::Object(question);
    response["answers"] = serde_json::json!([["yes"]]);

    let resolved = client.respond_question(&response)?;
    assert_eq!(resolved["status"], "completed");
    assert_eq!(
        resolved["turn"]["final_answer"],
        serde_json::json!("continuing after yes")
    );
    let events = resolved["events"].as_array().expect("resolved events");
    assert!(events.iter().any(|event| {
        event["method"] == "item/toolCall/completed"
            && event["params"]["name"] == "question"
            && event["params"]["output"]
                .as_str()
                .is_some_and(|value| value.contains("yes"))
    }));
    assert!(events.iter().any(|event| {
        event["method"] == "turn/completed"
            && event["params"]["final_answer"] == "continuing after yes"
    }));
    let session = client.get_session(&session_id)?;
    assert!(session["metadata"]["pending_question"].is_null());
    assert!(session["metadata"]["pending_provider_turn"].is_null());
    let messages = client.session_messages(&session_id, Some(20))?;
    let question_parts = message_parts_by_kind(&messages, "question");
    assert!(
        question_parts
            .iter()
            .any(|part| { part["status"] == "pending" && part["content"]["status"] == "pending" })
    );
    assert!(question_parts.iter().any(|part| {
        part["status"] == "completed"
            && part["content"]["status"] == "answered"
            && part["content"]["resolution"]["answers"]
                .as_array()
                .is_some_and(|answers| !answers.is_empty())
    }));

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contains("function_call_output"));
    assert!(requests[1].contains("yes"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn remote_runtime_client_resumes_provider_after_approval_allow() -> Result<(), Box<dyn Error>> {
    let first = serde_json::json!({
        "id": "resp_approval",
        "output": [{
            "type": "function_call",
            "call_id": "call_bash",
            "name": "bash",
            "arguments": "{\"command\":\"printf approved\"}"
        }],
        "usage": {"input_tokens": 6, "output_tokens": 1}
    });
    let second = serde_json::json!({
        "id": "resp_final",
        "output_text": "approval flow completed",
        "usage": {"input_tokens": 10, "output_tokens": 4}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![first, second])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-provider-approval-resume")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "run command",
        serde_json::json!({"permission": "PLAN_ONLY"}),
    )?;
    assert_eq!(started["status"], "waiting_approval");
    let approval = started["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| event["method"] == "turn/approval_requested")
        .and_then(|event| event["params"]["approval"].as_object())
        .cloned()
        .expect("approval event");
    let mut response = Value::Object(approval);
    response["action"] = Value::String("allow".to_string());
    response["scope"] = Value::String("once".to_string());

    let resolved = client.respond_approval(&response)?;
    assert_eq!(resolved["status"], "completed");
    assert_eq!(
        resolved["turn"]["final_answer"],
        serde_json::json!("approval flow completed")
    );
    let events = resolved["events"].as_array().expect("resolved events");
    assert!(events.iter().any(|event| {
        event["method"] == "item/toolCall/completed"
            && event["params"]["name"] == "bash"
            && event["params"]["output"] == "approved"
    }));
    assert!(events.iter().any(|event| {
        event["method"] == "turn/completed"
            && event["params"]["final_answer"] == "approval flow completed"
    }));
    let session = client.get_session(&session_id)?;
    assert!(session["metadata"]["pending_approval"].is_null());
    assert!(session["metadata"]["pending_provider_turn"].is_null());
    let messages = client.session_messages(&session_id, Some(20))?;
    let approval_parts = message_parts_by_kind(&messages, "approval");
    assert!(
        approval_parts
            .iter()
            .any(|part| { part["status"] == "pending" && part["content"]["status"] == "pending" })
    );
    assert!(approval_parts.iter().any(|part| {
        part["status"] == "completed"
            && part["content"]["status"] == "allowed"
            && part["content"]["resolution"]["action"] == "allow"
    }));

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contains("function_call_output"));
    assert!(requests[1].contains("approved"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn remote_runtime_client_stops_provider_after_approval_deny() -> Result<(), Box<dyn Error>> {
    let first = serde_json::json!({
        "id": "resp_approval_deny",
        "output": [{
            "type": "function_call",
            "call_id": "call_bash_deny",
            "name": "bash",
            "arguments": "{\"command\":\"printf should-not-run > denied.txt\"}"
        }],
        "usage": {"input_tokens": 6, "output_tokens": 1}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![first])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-provider-approval-deny")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "run denied command",
        serde_json::json!({"permission": "PLAN_ONLY"}),
    )?;
    assert_eq!(started["status"], "waiting_approval");
    let approval = started["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| event["method"] == "turn/approval_requested")
        .and_then(|event| event["params"]["approval"].as_object())
        .cloned()
        .expect("approval event");
    let mut response = Value::Object(approval);
    response["action"] = Value::String("deny".to_string());
    response["note"] = Value::String("test denied".to_string());

    let denied = client.respond_approval(&response)?;
    assert_eq!(denied["status"], "failed");
    assert_eq!(denied["approval"]["action"], "deny");
    let events = denied["events"].as_array().expect("denied events");
    assert!(events.iter().any(|event| {
        event["method"] == "turn/approval_resolved" && event["params"]["status"] == "denied"
    }));
    assert!(events.iter().any(|event| {
        event["method"] == "turn/failed" && event["params"]["error"] == "approval denied"
    }));
    assert!(!workspace.join("denied.txt").exists());

    let session = client.get_session(&session_id)?;
    assert_eq!(session["status"], "idle");
    assert!(session["metadata"]["pending_approval"].is_null());
    assert!(session["metadata"]["pending_provider_turn"].is_null());
    let messages = client.session_messages(&session_id, Some(20))?;
    let approval_parts = message_parts_by_kind(&messages, "approval");
    assert!(
        approval_parts
            .iter()
            .any(|part| { part["status"] == "pending" && part["content"]["status"] == "pending" })
    );
    assert!(approval_parts.iter().any(|part| {
        part["status"] == "error"
            && part["content"]["status"] == "denied"
            && part["content"]["resolution"]["action"] == "deny"
            && part["content"]["resolution"]["note"] == "test denied"
    }));

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 1);
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn remote_runtime_client_stops_provider_after_question_dismiss() -> Result<(), Box<dyn Error>> {
    let first = serde_json::json!({
        "id": "resp_question_dismiss",
        "output": [{
            "type": "function_call",
            "call_id": "call_question_dismiss",
            "name": "question",
            "arguments": "{\"questions\":[{\"header\":\"Confirm\",\"question\":\"Proceed?\",\"multiple\":false,\"options\":[{\"label\":\"yes\",\"description\":\"Continue\"},{\"label\":\"no\",\"description\":\"Stop\"}]}]}"
        }],
        "usage": {"input_tokens": 4, "output_tokens": 1}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence(vec![first])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-provider-question-dismiss")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = client.start_turn(
        &session_id,
        "ask dismissable question",
        serde_json::json!({}),
    )?;
    assert_eq!(started["status"], "waiting_question");
    let question = started["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| event["method"] == "item/question/requested")
        .and_then(|event| event["params"]["event"].as_object())
        .cloned()
        .expect("question event");
    let mut response = Value::Object(question);
    response["dismissed"] = Value::Bool(true);
    response["note"] = Value::String("test dismissed".to_string());

    let dismissed = client.respond_question(&response)?;
    assert_eq!(dismissed["status"], "failed");
    assert_eq!(dismissed["question"]["dismissed"], true);
    let events = dismissed["events"].as_array().expect("dismissed events");
    assert!(events.iter().any(|event| {
        event["method"] == "item/question/resolved" && event["params"]["status"] == "dismissed"
    }));
    assert!(events.iter().any(|event| {
        event["method"] == "turn/failed" && event["params"]["error"] == "test dismissed"
    }));

    let session = client.get_session(&session_id)?;
    assert_eq!(session["status"], "idle");
    assert!(session["metadata"]["pending_question"].is_null());
    assert!(session["metadata"]["pending_provider_turn"].is_null());
    let messages = client.session_messages(&session_id, Some(20))?;
    let question_parts = message_parts_by_kind(&messages, "question");
    assert!(
        question_parts
            .iter()
            .any(|part| { part["status"] == "pending" && part["content"]["status"] == "pending" })
    );
    assert!(question_parts.iter().any(|part| {
        part["status"] == "error"
            && part["content"]["status"] == "dismissed"
            && part["content"]["resolution"]["dismissed"] == true
            && part["content"]["resolution"]["note"] == "test dismissed"
    }));

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 1);
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn live_sse_tails_interaction_resolved_events_before_provider_final() -> Result<(), Box<dyn Error>>
{
    run_live_interaction_resume_case("question")?;
    run_live_interaction_resume_case("approval")?;
    Ok(())
}

#[test]
fn global_sse_live_tails_provider_stream_delta_before_completion() -> Result<(), Box<dyn Error>> {
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_streaming_provider()?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-provider-live-stream")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let live = thread::spawn(move || {
        http_request(
            port,
            "GET",
            "/api/events?last_event_id=0&live_timeout_ms=700",
            &[
                ("Authorization", "Bearer secret"),
                ("Accept", "text/event-stream"),
            ],
            "",
        )
        .map_err(|error| error.to_string())
    });
    thread::sleep(Duration::from_millis(150));

    let turn_session_id = session_id.clone();
    let turn = thread::spawn(move || {
        let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
            .with_auth(RemoteAuth::bearer("secret"));
        client
            .start_turn(
                &turn_session_id,
                "stream from provider",
                serde_json::json!({}),
            )
            .map_err(|error| error.to_string())
    });

    let live_response = live
        .join()
        .map_err(|_| "live sse thread panicked".to_string())?
        .map_err(|error| format!("live sse request failed: {error}"))?;
    assert!(live_response.contains("event: item/agentMessage/delta"));
    assert!(live_response.contains("streamed "));
    assert!(!live_response.contains("event: turn/completed"));

    let started = turn
        .join()
        .map_err(|_| "turn thread panicked".to_string())?
        .map_err(|error| format!("turn failed: {error}"))?;
    assert_eq!(started["status"], "completed");
    assert_eq!(started["turn"]["final_answer"], "streamed answer");
    assert_eq!(started["turn"]["usage"]["input_tokens"], 11);
    assert_eq!(started["turn"]["usage"]["output_tokens"], 2);

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("accept: text/event-stream")
    );
    assert!(requests[0].contains("\"stream\":true"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn async_turn_returns_accepted_before_provider_completion_and_streams() -> Result<(), Box<dyn Error>>
{
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_streaming_provider()?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-async-turn-stream")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started_at = Instant::now();
    let accepted_response = authorized_request(
        port,
        "POST",
        &format!("/api/sessions/{session_id}/turns"),
        &serde_json::json!({
            "input": "stream asynchronously",
            "async": true,
            "stream": true,
        })
        .to_string(),
        true,
    )?;
    assert!(accepted_response.starts_with("HTTP/1.1 202"));
    assert!(
        started_at.elapsed() < Duration::from_millis(900),
        "async turn should return before delayed provider completion"
    );
    let accepted = json_body(&accepted_response)?;
    assert_eq!(accepted["status"], "running");
    assert_eq!(accepted["accepted"], true);
    assert_eq!(accepted["async"], true);
    let turn_id = accepted["turn_id"].as_str().expect("turn id");

    let events = client.turn_events_live(turn_id, 0, Duration::from_secs(4))?;
    let methods = events
        .iter()
        .filter_map(|event| event.get("method").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(methods.contains(&"turn/started"));
    assert!(methods.contains(&"item/agentMessage/delta"));
    assert!(methods.contains(&"turn/completed"));
    let final_event = events
        .iter()
        .find(|event| event.get("method").and_then(Value::as_str) == Some("turn/completed"))
        .expect("turn completed event");
    assert_eq!(final_event["params"]["final_answer"], "streamed answer");

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("\"stream\":true"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn async_turn_queues_second_active_turn_for_same_session() -> Result<(), Box<dyn Error>> {
    let first_response = serde_json::json!({
        "id": "resp_first_async_queue",
        "output_text": "first queued answer",
        "usage": {"input_tokens": 9, "output_tokens": 3}
    });
    let second_response = serde_json::json!({
        "id": "resp_second_async_queue",
        "output_text": "second queued answer",
        "usage": {"input_tokens": 10, "output_tokens": 3}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence_with_delays(vec![
            (first_response, 900),
            (second_response, 0),
        ])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-async-turn-queue")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let first_response = authorized_request(
        port,
        "POST",
        &format!("/api/sessions/{session_id}/turns"),
        &serde_json::json!({
            "input": "first async turn",
            "async": true,
        })
        .to_string(),
        true,
    )?;
    assert!(first_response.starts_with("HTTP/1.1 202"));
    let first = json_body(&first_response)?;
    let first_turn_id = first["turn_id"].as_str().expect("first turn id");
    assert_eq!(first["accepted"], true);

    let second_response = authorized_request(
        port,
        "POST",
        &format!("/api/sessions/{session_id}/turns"),
        &serde_json::json!({
            "input": "second async turn should run after first",
            "async": true,
        })
        .to_string(),
        true,
    )?;
    assert!(
        second_response.starts_with("HTTP/1.1 202"),
        "expected queued turn accepted, got {second_response}"
    );
    let second = json_body(&second_response)?;
    let second_turn_id = second["turn_id"].as_str().expect("second turn id");
    assert_ne!(second_turn_id, first_turn_id);
    assert_eq!(second["accepted"], true);
    assert_eq!(second["queued"], true);
    assert_eq!(second["status"], "queued");
    assert_eq!(second["queue_position"].as_u64(), Some(1));
    assert_eq!(
        second["scheduler"]["max_queued_turns_per_session"].as_u64(),
        Some(8)
    );
    assert_eq!(second["turn"]["status"], "queued");
    assert_eq!(second["turn"]["queue_position"].as_u64(), Some(1));

    let listed = client.turns_for_session(&session_id)?;
    assert_eq!(listed["running_count"].as_u64(), Some(1));
    assert_eq!(listed["queued_count"].as_u64(), Some(1));
    assert_eq!(listed["active_count"].as_u64(), Some(2));
    assert_eq!(listed["count"].as_u64(), Some(2));
    assert_eq!(
        listed["scheduler"]["max_queued_turns_per_session"].as_u64(),
        Some(8)
    );
    let listed_turns = listed["turns"].as_array().expect("listed turns");
    assert!(
        listed_turns
            .iter()
            .any(|turn| turn["turn_id"] == first_turn_id && turn["status"] == "running")
    );
    assert!(
        listed_turns
            .iter()
            .any(|turn| turn["turn_id"] == second_turn_id
                && turn["status"] == "queued"
                && turn["queue_position"].as_u64() == Some(1))
    );

    let first_events = client.turn_events_live(first_turn_id, 0, Duration::from_secs(4))?;
    assert!(
        first_events
            .iter()
            .any(|event| event.get("method").and_then(Value::as_str) == Some("turn/completed"))
    );
    let second_events = client.turn_events_live(second_turn_id, 0, Duration::from_secs(4))?;
    assert!(
        second_events
            .iter()
            .any(|event| event.get("method").and_then(Value::as_str) == Some("turn/completed"))
    );
    let second_status = client.turn_status(second_turn_id)?;
    assert_eq!(second_status["status"], "completed");

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(
        requests.len(),
        2,
        "queued async turn should start after the active turn completes"
    );
    assert!(requests[0].contains("first async turn"));
    assert!(requests[1].contains("second async turn should run after first"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn async_turn_queues_second_session_when_global_worker_quota_full() -> Result<(), Box<dyn Error>> {
    let first_response = serde_json::json!({
        "id": "resp_first_async_global_quota",
        "output_text": "first global quota answer",
        "usage": {"input_tokens": 9, "output_tokens": 3}
    });
    let second_response = serde_json::json!({
        "id": "resp_second_async_global_quota",
        "output_text": "second global quota answer",
        "usage": {"input_tokens": 10, "output_tokens": 3}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence_with_delays(vec![
            (first_response, 900),
            (second_response, 0),
        ])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-async-turn-global-quota")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
            ("OPENAGENT_MAX_RUNNING_TURN_WORKERS", "1"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let first_session_id = client.create_session(&workspace, Some("quota first"))?;
    let second_session_id = client.create_session(&workspace, Some("quota second"))?;
    let first_response = authorized_request(
        port,
        "POST",
        &format!("/api/sessions/{first_session_id}/turns"),
        &serde_json::json!({
            "input": "first async turn occupies global worker",
            "async": true,
        })
        .to_string(),
        true,
    )?;
    assert!(first_response.starts_with("HTTP/1.1 202"));
    let first = json_body(&first_response)?;
    let first_turn_id = first["turn_id"].as_str().expect("first turn id");
    assert_eq!(first["status"], "running");
    for _ in 0..40 {
        if !provider_requests
            .lock()
            .expect("provider requests")
            .is_empty()
        {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        provider_requests.lock().expect("provider requests").len(),
        1,
        "first turn should be the only provider request while quota is full"
    );

    let second_response = authorized_request(
        port,
        "POST",
        &format!("/api/sessions/{second_session_id}/turns"),
        &serde_json::json!({
            "input": "second session waits for global worker quota",
            "async": true,
        })
        .to_string(),
        true,
    )?;
    assert!(
        second_response.starts_with("HTTP/1.1 202"),
        "expected second session to queue under global quota, got {second_response}"
    );
    let second = json_body(&second_response)?;
    let second_turn_id = second["turn_id"].as_str().expect("second turn id");
    assert_eq!(second["accepted"], true);
    assert_eq!(second["queued"], true);
    assert_eq!(second["status"], "queued");
    assert_eq!(second["queue_reason"], "global_worker_quota");
    assert_eq!(second["queue_position"].as_u64(), Some(1));
    assert_eq!(
        second["scheduler"]["max_running_turn_workers"].as_u64(),
        Some(1)
    );
    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        provider_requests.lock().expect("provider requests").len(),
        1,
        "queued second-session turn must not start until the running worker finishes"
    );

    let listed = client.turns()?;
    assert_eq!(listed["running_count"].as_u64(), Some(1));
    assert_eq!(listed["queued_count"].as_u64(), Some(1));
    assert_eq!(listed["active_count"].as_u64(), Some(2));
    assert_eq!(
        listed["scheduler"]["running_turn_workers"].as_u64(),
        Some(1)
    );
    assert_eq!(
        listed["scheduler"]["max_running_turn_workers"].as_u64(),
        Some(1)
    );

    let first_events = client.turn_events_live(first_turn_id, 0, Duration::from_secs(4))?;
    assert!(
        first_events
            .iter()
            .any(|event| event.get("method").and_then(Value::as_str) == Some("turn/completed"))
    );
    let second_events = client.turn_events_live(second_turn_id, 0, Duration::from_secs(4))?;
    assert!(
        second_events
            .iter()
            .any(|event| event.get("method").and_then(Value::as_str) == Some("turn/completed"))
    );
    let second_status = client.turn_status(second_turn_id)?;
    assert_eq!(second_status["status"], "completed");

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(
        requests.len(),
        2,
        "global quota queued turn should start exactly once after the first worker completes"
    );
    assert!(requests[0].contains("first async turn occupies global worker"));
    assert!(requests[1].contains("second session waits for global worker quota"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn async_turn_expires_queued_turn_after_timeout() -> Result<(), Box<dyn Error>> {
    let first_response = serde_json::json!({
        "id": "resp_first_async_queue_expiry",
        "output_text": "first queue expiry answer",
        "usage": {"input_tokens": 9, "output_tokens": 3}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence_with_delays(vec![(first_response, 900)])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-async-turn-queue-expiry")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
            ("OPENAGENT_TURN_QUEUE_TIMEOUT_MS", "150"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let first_response = authorized_request(
        port,
        "POST",
        &format!("/api/sessions/{session_id}/turns"),
        &serde_json::json!({
            "input": "first async turn before queue expiry",
            "async": true,
        })
        .to_string(),
        true,
    )?;
    assert!(first_response.starts_with("HTTP/1.1 202"));
    let first = json_body(&first_response)?;
    let first_turn_id = first["turn_id"].as_str().expect("first turn id");

    let second_response = authorized_request(
        port,
        "POST",
        &format!("/api/sessions/{session_id}/turns"),
        &serde_json::json!({
            "input": "second async turn should expire in queue",
            "async": true,
        })
        .to_string(),
        true,
    )?;
    assert!(second_response.starts_with("HTTP/1.1 202"));
    let second = json_body(&second_response)?;
    let second_turn_id = second["turn_id"].as_str().expect("second turn id");
    assert_eq!(second["status"], "queued");
    assert_eq!(
        second["scheduler"]["turn_queue_timeout_ms"].as_u64(),
        Some(150)
    );
    let queued_payload_path = session_root
        .join(".openagent-runtime")
        .join("turn_queue")
        .join(format!("{second_turn_id}.json"));
    assert!(queued_payload_path.exists());

    thread::sleep(Duration::from_millis(250));
    let listed = client.turns_for_session(&session_id)?;
    assert_eq!(listed["running_count"].as_u64(), Some(1));
    assert_eq!(listed["queued_count"].as_u64(), Some(0));
    assert_eq!(listed["terminal_count"].as_u64(), Some(1));
    assert_eq!(
        listed["scheduler"]["turn_queue_timeout_ms"].as_u64(),
        Some(150)
    );
    assert_eq!(
        listed["scheduler"]["expired_queued_turns"].as_u64(),
        Some(1)
    );
    let expired_turn = listed["turns"]
        .as_array()
        .expect("turns")
        .iter()
        .find(|turn| turn["turn_id"] == second_turn_id)
        .expect("expired queued turn");
    assert_eq!(expired_turn["status"], "expired");
    assert!(
        !queued_payload_path.exists(),
        "expired queued turn payload should be removed"
    );
    let second_status = client.turn_status(second_turn_id)?;
    assert_eq!(second_status["status"], "expired");

    let first_events = client.turn_events_live(first_turn_id, 0, Duration::from_secs(4))?;
    assert!(
        first_events
            .iter()
            .any(|event| event.get("method").and_then(Value::as_str) == Some("turn/completed"))
    );
    thread::sleep(Duration::from_millis(150));

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(
        requests.len(),
        1,
        "expired queued turn must not start or call the provider"
    );
    assert!(requests[0].contains("first async turn before queue expiry"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn async_turn_rejects_when_session_queue_is_full() -> Result<(), Box<dyn Error>> {
    let first_response = serde_json::json!({
        "id": "resp_first_async_queue_limit",
        "output_text": "first queue limit answer",
        "usage": {"input_tokens": 9, "output_tokens": 3}
    });
    let second_response = serde_json::json!({
        "id": "resp_second_async_queue_limit",
        "output_text": "second queue limit answer",
        "usage": {"input_tokens": 10, "output_tokens": 3}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence_with_delays(vec![
            (first_response, 900),
            (second_response, 0),
        ])?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-async-turn-queue-limit")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
            ("OPENAGENT_MAX_QUEUED_TURNS_PER_SESSION", "1"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let first_response = authorized_request(
        port,
        "POST",
        &format!("/api/sessions/{session_id}/turns"),
        &serde_json::json!({
            "input": "first async turn before queue limit",
            "async": true,
        })
        .to_string(),
        true,
    )?;
    assert!(first_response.starts_with("HTTP/1.1 202"));
    let first = json_body(&first_response)?;
    let first_turn_id = first["turn_id"].as_str().expect("first turn id");

    let second_response = authorized_request(
        port,
        "POST",
        &format!("/api/sessions/{session_id}/turns"),
        &serde_json::json!({
            "input": "second async turn fills queue",
            "async": true,
        })
        .to_string(),
        true,
    )?;
    assert!(second_response.starts_with("HTTP/1.1 202"));
    let second = json_body(&second_response)?;
    let second_turn_id = second["turn_id"].as_str().expect("second turn id");
    assert_eq!(second["status"], "queued");
    assert_eq!(second["queue_position"].as_u64(), Some(1));

    let third_response = authorized_request(
        port,
        "POST",
        &format!("/api/sessions/{session_id}/turns"),
        &serde_json::json!({
            "input": "third async turn should be rejected",
            "async": true,
        })
        .to_string(),
        true,
    )?;
    assert!(
        third_response.starts_with("HTTP/1.1 429"),
        "expected queue full 429, got {third_response}"
    );
    let third = json_body(&third_response)?;
    assert_eq!(third["accepted"], false);
    assert_eq!(third["queued"], false);
    assert_eq!(third["status"], "rejected");
    assert_eq!(third["error_code"], "turn_queue_full");
    assert_eq!(third["queued_count"].as_u64(), Some(1));
    assert_eq!(third["max_queued_turns_per_session"].as_u64(), Some(1));
    assert_eq!(
        third["scheduler"]["max_queued_turns_per_session"].as_u64(),
        Some(1)
    );

    let listed = client.turns_for_session(&session_id)?;
    assert_eq!(listed["running_count"].as_u64(), Some(1));
    assert_eq!(listed["queued_count"].as_u64(), Some(1));
    assert_eq!(listed["active_count"].as_u64(), Some(2));
    assert_eq!(listed["count"].as_u64(), Some(2));

    let first_events = client.turn_events_live(first_turn_id, 0, Duration::from_secs(4))?;
    assert!(
        first_events
            .iter()
            .any(|event| event.get("method").and_then(Value::as_str) == Some("turn/completed"))
    );
    let second_events = client.turn_events_live(second_turn_id, 0, Duration::from_secs(4))?;
    assert!(
        second_events
            .iter()
            .any(|event| event.get("method").and_then(Value::as_str) == Some("turn/completed"))
    );

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(
        requests.len(),
        2,
        "queue-full rejected turn must not call the provider"
    );
    assert!(requests[0].contains("first async turn before queue limit"));
    assert!(requests[1].contains("second async turn fills queue"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn async_turn_recovers_persisted_queued_turn_after_runtime_restart() -> Result<(), Box<dyn Error>> {
    let first_response = serde_json::json!({
        "id": "resp_first_async_queue_recovery",
        "output_text": "first queue recovery answer",
        "usage": {"input_tokens": 9, "output_tokens": 3}
    });
    let second_response = serde_json::json!({
        "id": "resp_second_async_queue_recovery",
        "output_text": "second queue recovery answer",
        "usage": {"input_tokens": 10, "output_tokens": 3}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence_with_delays(vec![
            (first_response, 1200),
            (second_response, 0),
        ])?;
    let first_port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-async-turn-queue-recovery")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let provider_env = [
        ("OPENAI_API_KEY", "test-key"),
        ("OPENAI_BASE_URL", provider_base_url.as_str()),
        ("OPENAI_WIRE_API", "responses"),
        ("OPENAI_MODEL", "fake-model"),
        ("OPENAGENT_TURN_QUEUE_LEASE_STALE_MS", "1"),
    ];
    let mut first_server =
        spawn_runtime_with_env(first_port, &workspace, &session_root, &provider_env)?;
    wait_for_server(first_port)?;

    let first_client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{first_port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = first_client.create_session(&workspace, None)?;
    let first_response = authorized_request(
        first_port,
        "POST",
        &format!("/api/sessions/{session_id}/turns"),
        &serde_json::json!({
            "input": "first async turn before restart",
            "async": true,
        })
        .to_string(),
        true,
    )?;
    assert!(first_response.starts_with("HTTP/1.1 202"));
    let first = json_body(&first_response)?;
    let first_turn_id = first["turn_id"].as_str().expect("first turn id");
    for _ in 0..40 {
        if !provider_requests
            .lock()
            .expect("provider requests")
            .is_empty()
        {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        provider_requests.lock().expect("provider requests").len(),
        1,
        "first runtime should have sent the active turn to the provider before restart"
    );

    let second_response = authorized_request(
        first_port,
        "POST",
        &format!("/api/sessions/{session_id}/turns"),
        &serde_json::json!({
            "input": "second async turn should recover after restart",
            "async": true,
        })
        .to_string(),
        true,
    )?;
    assert!(second_response.starts_with("HTTP/1.1 202"));
    let second = json_body(&second_response)?;
    let second_turn_id = second["turn_id"].as_str().expect("second turn id");
    assert_eq!(second["status"], "queued");
    assert_eq!(second["queue_position"].as_u64(), Some(1));

    let queued_before_restart = first_client.turns_for_session(&session_id)?;
    let queued_turn = queued_before_restart["turns"]
        .as_array()
        .expect("turns")
        .iter()
        .find(|turn| turn["turn_id"] == second_turn_id)
        .expect("queued turn before restart");
    assert_eq!(queued_turn["status"], "queued");
    assert_eq!(queued_turn["payload_persisted"], true);

    let _ = first_server.kill();
    let _ = first_server.wait();
    thread::sleep(Duration::from_millis(10));

    let second_port = free_port()?;
    let mut second_server =
        spawn_runtime_with_env(second_port, &workspace, &session_root, &provider_env)?;
    wait_for_server(second_port)?;
    let second_client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{second_port}"))
        .with_auth(RemoteAuth::bearer("secret"));

    let second_events =
        second_client.turn_events_live(second_turn_id, 0, Duration::from_secs(6))?;
    assert!(
        second_events
            .iter()
            .any(|event| event.get("method").and_then(Value::as_str) == Some("turn/completed")),
        "recovered queued turn should complete after restart"
    );
    let second_status = second_client.turn_status(second_turn_id)?;
    assert_eq!(second_status["status"], "completed");
    let listed_after_restart = second_client.turns_for_session(&session_id)?;
    let listed_turns = listed_after_restart["turns"].as_array().expect("turns");
    assert!(listed_turns.iter().any(|turn| {
        turn["turn_id"] == first_turn_id
            && turn["status"] == "interrupted"
            && turn["cancel_requested"] == true
    }));
    assert!(
        listed_turns
            .iter()
            .any(|turn| { turn["turn_id"] == second_turn_id && turn["status"] == "completed" })
    );

    let _ = second_server.kill();
    let _ = second_server.wait();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(
        requests.len(),
        2,
        "restarted runtime should recover exactly the queued turn"
    );
    assert!(requests[0].contains("first async turn before restart"));
    assert!(requests[1].contains("second async turn should recover after restart"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn async_turn_recovery_respects_live_queue_lease_owner() -> Result<(), Box<dyn Error>> {
    let first_response = serde_json::json!({
        "id": "resp_first_async_queue_live_lease",
        "output_text": "first live lease answer",
        "usage": {"input_tokens": 9, "output_tokens": 3}
    });
    let second_response = serde_json::json!({
        "id": "resp_second_async_queue_live_lease",
        "output_text": "second live lease answer",
        "usage": {"input_tokens": 10, "output_tokens": 3}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence_with_delays(vec![
            (first_response, 900),
            (second_response, 0),
        ])?;
    let first_port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-async-turn-queue-live-lease")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let provider_env = [
        ("OPENAI_API_KEY", "test-key"),
        ("OPENAI_BASE_URL", provider_base_url.as_str()),
        ("OPENAI_WIRE_API", "responses"),
        ("OPENAI_MODEL", "fake-model"),
        ("OPENAGENT_TURN_QUEUE_LEASE_STALE_MS", "30_000"),
    ];
    let mut first_server =
        spawn_runtime_with_env(first_port, &workspace, &session_root, &provider_env)?;
    wait_for_server(first_port)?;

    let first_client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{first_port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = first_client.create_session(&workspace, None)?;
    let first_response = authorized_request(
        first_port,
        "POST",
        &format!("/api/sessions/{session_id}/turns"),
        &serde_json::json!({
            "input": "first async turn with live lease owner",
            "async": true,
        })
        .to_string(),
        true,
    )?;
    assert!(first_response.starts_with("HTTP/1.1 202"));
    let first = json_body(&first_response)?;
    let first_turn_id = first["turn_id"].as_str().expect("first turn id");
    for _ in 0..40 {
        if !provider_requests
            .lock()
            .expect("provider requests")
            .is_empty()
        {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        provider_requests.lock().expect("provider requests").len(),
        1
    );

    let second_response = authorized_request(
        first_port,
        "POST",
        &format!("/api/sessions/{session_id}/turns"),
        &serde_json::json!({
            "input": "second async turn should stay with original owner",
            "async": true,
        })
        .to_string(),
        true,
    )?;
    assert!(second_response.starts_with("HTTP/1.1 202"));
    let second = json_body(&second_response)?;
    let second_turn_id = second["turn_id"].as_str().expect("second turn id");
    assert_eq!(second["status"], "queued");
    assert_eq!(second["queue_position"].as_u64(), Some(1));

    let second_port = free_port()?;
    let mut second_server =
        spawn_runtime_with_env(second_port, &workspace, &session_root, &provider_env)?;
    wait_for_server(second_port)?;
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        provider_requests.lock().expect("provider requests").len(),
        1,
        "second runtime must not recover a queued turn while the original lease is live"
    );
    let _ = second_server.kill();
    let _ = second_server.wait();

    let first_events = first_client.turn_events_live(first_turn_id, 0, Duration::from_secs(4))?;
    assert!(
        first_events
            .iter()
            .any(|event| event.get("method").and_then(Value::as_str) == Some("turn/completed"))
    );
    let second_events = first_client.turn_events_live(second_turn_id, 0, Duration::from_secs(4))?;
    assert!(
        second_events
            .iter()
            .any(|event| event.get("method").and_then(Value::as_str) == Some("turn/completed"))
    );

    let _ = first_server.kill();
    let _ = first_server.wait();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(
        requests.len(),
        2,
        "live lease owner should be the only runtime to execute the queued turn"
    );
    assert!(requests[0].contains("first async turn with live lease owner"));
    assert!(requests[1].contains("second async turn should stay with original owner"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn async_turn_interrupt_cancels_provider_stream_before_completion() -> Result<(), Box<dyn Error>> {
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_streaming_provider()?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-async-turn-interrupt")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let accepted = json_body(&authorized_request(
        port,
        "POST",
        &format!("/api/sessions/{session_id}/turns"),
        &serde_json::json!({
            "input": "stream until interrupted",
            "async": true,
            "stream": true,
        })
        .to_string(),
        false,
    )?)?;
    assert_eq!(accepted["accepted"], true);
    let turn_id = accepted["turn_id"].as_str().expect("turn id");

    let running = client.turn_status(turn_id)?;
    assert_eq!(running["status"], "running");
    let running_turns = client.turns_for_session(&session_id)?;
    assert_eq!(running_turns["source"], "runtime_job_registry");
    assert_eq!(running_turns["count"].as_u64(), Some(1));
    assert_eq!(running_turns["running_count"].as_u64(), Some(1));
    let running_turn = running_turns["turns"]
        .as_array()
        .and_then(|turns| {
            turns
                .iter()
                .find(|turn| turn.get("turn_id").and_then(Value::as_str) == Some(turn_id))
        })
        .expect("running turn listed");
    assert_eq!(running_turn["status"], "running");
    assert_eq!(running_turn["session_id"], session_id);

    let mut saw_delta = false;
    for _ in 0..30 {
        let events = client.turn_events_live(turn_id, 0, Duration::from_millis(100))?;
        saw_delta = events.iter().any(|event| {
            event.get("method").and_then(Value::as_str) == Some("item/agentMessage/delta")
        });
        if saw_delta {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(saw_delta, "expected provider delta before interrupt");

    let interrupted = json_body(&authorized_request(
        port,
        "POST",
        &format!("/api/turns/{turn_id}/interrupt"),
        "",
        false,
    )?)?;
    assert_eq!(interrupted["status"], "interrupted");

    let _ = provider_thread.join();
    thread::sleep(Duration::from_millis(100));

    let status = client.turn_status(turn_id)?;
    assert_eq!(status["status"], "interrupted");
    let interrupted_turns = client.turns_for_session(&session_id)?;
    assert_eq!(interrupted_turns["count"].as_u64(), Some(1));
    assert_eq!(interrupted_turns["running_count"].as_u64(), Some(0));
    assert_eq!(interrupted_turns["terminal_count"].as_u64(), Some(1));
    let interrupted_turn = interrupted_turns["turns"]
        .as_array()
        .and_then(|turns| {
            turns
                .iter()
                .find(|turn| turn.get("turn_id").and_then(Value::as_str) == Some(turn_id))
        })
        .expect("interrupted turn listed");
    assert_eq!(interrupted_turn["status"], "interrupted");
    assert_eq!(interrupted_turn["cancel_requested"], true);

    let index_path = session_root
        .join(".openagent-runtime")
        .join("turn_jobs.json");
    let mut index = serde_json::from_str::<Value>(&fs::read_to_string(&index_path)?)?;
    index["turns"]
        .as_array_mut()
        .expect("turn job index array")
        .push(serde_json::json!({
            "session_id": session_id,
            "turn_id": "turn_old_terminal_for_prune",
            "status": "completed",
            "started_at_ms": 1,
            "updated_at_ms": 1,
            "cancel_requested": false,
            "cancel_requested_at_ms": null,
        }));
    fs::write(&index_path, serde_json::to_string(&index)?)?;

    let _ = server.kill();
    let _ = server.wait();
    let restart_port = free_port()?;
    let mut restarted = spawn_runtime_with_env(
        restart_port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(restart_port)?;
    let restarted_client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{restart_port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let restored_status = restarted_client.turn_status(turn_id)?;
    assert_eq!(restored_status["source"], "runtime_job_index");
    assert_eq!(restored_status["status"], "interrupted");
    let restored_turns = restarted_client.turns_for_session(&session_id)?;
    let restored_ids = restored_turns["turns"]
        .as_array()
        .expect("restored turns")
        .iter()
        .filter_map(|turn| turn.get("turn_id").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(restored_ids.contains(&turn_id));
    assert!(
        !restored_ids.contains(&"turn_old_terminal_for_prune"),
        "old terminal jobs should be pruned from persistent index"
    );

    let events = restarted_client.turn_events_live(turn_id, 0, Duration::from_millis(250))?;
    let methods = events
        .iter()
        .filter_map(|event| event.get("method").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(methods.contains(&"item/agentMessage/delta"));
    assert!(methods.contains(&"turn/interrupted"));
    assert!(
        !methods.contains(&"turn/completed"),
        "interrupted turn must not complete after cancellation"
    );
    let _ = restarted.kill();
    let _ = restarted.wait();

    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("\"stream\":true"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn global_sse_live_tails_provider_tool_events_before_final_answer() -> Result<(), Box<dyn Error>> {
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_streaming_tool_then_delayed_final_provider()?;
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-provider-tool-live-stream")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join("notes.txt"), "alpha\n")?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let live = thread::spawn(move || {
        http_request(
            port,
            "GET",
            "/api/events?last_event_id=0&live_timeout_ms=800",
            &[
                ("Authorization", "Bearer secret"),
                ("Accept", "text/event-stream"),
            ],
            "",
        )
        .map_err(|error| error.to_string())
    });
    thread::sleep(Duration::from_millis(150));

    let turn_session_id = session_id.clone();
    let turn = thread::spawn(move || {
        let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
            .with_auth(RemoteAuth::bearer("secret"));
        client
            .start_turn(
                &turn_session_id,
                "read notes with live tool events",
                serde_json::json!({}),
            )
            .map_err(|error| error.to_string())
    });

    let live_response = live
        .join()
        .map_err(|_| "live sse thread panicked".to_string())?
        .map_err(|error| format!("live sse request failed: {error}"))?;
    assert!(live_response.contains("event: item/toolCall/started"));
    assert!(live_response.contains("event: item/toolCall/completed"));
    assert!(live_response.contains("call_live_read"));
    assert!(live_response.contains("alpha"));
    assert!(!live_response.contains("event: turn/completed"));

    let started = turn
        .join()
        .map_err(|_| "turn thread panicked".to_string())?
        .map_err(|error| format!("turn failed: {error}"))?;
    assert_eq!(started["status"], "completed");
    assert_eq!(started["turn"]["final_answer"], "tool final answer");

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contains("function_call_output"));
    assert!(requests[1].contains("alpha"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn global_sse_live_tails_events_after_connection() -> Result<(), Box<dyn Error>> {
    let port = free_port()?;
    let temp = temp_dir("openagent-http-runtime-live-sse")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let mut server = spawn_runtime(port, &workspace, &session_root)?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let live = thread::spawn(move || {
        http_request(
            port,
            "GET",
            "/api/events?last_event_id=0&live_timeout_ms=5000",
            &[
                ("Authorization", "Bearer secret"),
                ("Accept", "text/event-stream"),
            ],
            "",
        )
        .map_err(|error| error.to_string())
    });
    thread::sleep(Duration::from_millis(150));

    let started = client.start_turn(
        &session_id,
        "write notes",
        serde_json::json!({
            "permission": "FULL",
            "tool_call": {
                "call_id": "call_live_write",
                "name": "write",
                "input": {"file_path": "live.txt", "content": "live\n"}
            }
        }),
    )?;
    assert_eq!(started["status"], "completed");

    let live_response = live
        .join()
        .map_err(|_| "live sse thread panicked".to_string())?
        .map_err(|error| format!("live sse request failed: {error}"))?;
    assert!(live_response.contains("content-type: text/event-stream"));
    assert!(
        !live_response
            .to_ascii_lowercase()
            .contains("content-length")
    );
    assert!(live_response.contains("event: item/toolCall/completed"));
    assert!(live_response.contains("event: turn/completed"));

    let _ = server.kill();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

fn read_fixture() -> Result<Value, Box<dyn Error>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/rust_rewrite/http_runtime.json");
    let raw = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

fn section(fixture: &Value, name: &str) -> Value {
    fixture.get(name).cloned().unwrap_or(Value::Null)
}

fn run_live_interaction_resume_case(kind: &str) -> Result<(), Box<dyn Error>> {
    let first = match kind {
        "question" => serde_json::json!({
            "id": "resp_question_live",
            "output": [{
                "type": "function_call",
                "call_id": "call_question_live",
                "name": "question",
                "arguments": "{\"questions\":[{\"header\":\"Confirm\",\"question\":\"Proceed?\",\"multiple\":false,\"options\":[{\"label\":\"yes\",\"description\":\"Continue\"},{\"label\":\"no\",\"description\":\"Stop\"}]}]}"
            }],
            "usage": {"input_tokens": 4, "output_tokens": 1}
        }),
        "approval" => serde_json::json!({
            "id": "resp_approval_live",
            "output": [{
                "type": "function_call",
                "call_id": "call_bash_live",
                "name": "bash",
                "arguments": "{\"command\":\"printf approved\"}"
            }],
            "usage": {"input_tokens": 6, "output_tokens": 1}
        }),
        other => return Err(format!("unsupported interaction case: {other}").into()),
    };
    let final_answer = format!("{kind} final answer");
    let second = serde_json::json!({
        "id": format!("resp_{kind}_final"),
        "output_text": final_answer.clone(),
        "usage": {"input_tokens": 9, "output_tokens": 3}
    });
    let (provider_port, provider_thread, provider_requests) =
        spawn_fake_openai_responses_provider_sequence_with_delays(vec![
            (first, 0),
            (second, 1500),
        ])?;
    let port = free_port()?;
    let temp = temp_dir(&format!("openagent-http-runtime-live-{kind}-resume"))?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    let provider_base_url = format!("http://127.0.0.1:{provider_port}/v1");
    let mut server = spawn_runtime_with_env(
        port,
        &workspace,
        &session_root,
        &[
            ("OPENAI_API_KEY", "test-key"),
            ("OPENAI_BASE_URL", provider_base_url.as_str()),
            ("OPENAI_WIRE_API", "responses"),
            ("OPENAI_MODEL", "fake-model"),
        ],
    )?;
    wait_for_server(port)?;

    let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
        .with_auth(RemoteAuth::bearer("secret"));
    let session_id = client.create_session(&workspace, None)?;
    let started = if kind == "approval" {
        client.start_turn(
            &session_id,
            "run command with approval",
            serde_json::json!({"permission": "PLAN_ONLY"}),
        )?
    } else {
        client.start_turn(&session_id, "ask a question", serde_json::json!({}))?
    };
    assert_eq!(
        started["status"],
        if kind == "approval" {
            "waiting_approval"
        } else {
            "waiting_question"
        }
    );
    let mut response = if kind == "approval" {
        Value::Object(
            started["events"]
                .as_array()
                .expect("events")
                .iter()
                .find(|event| event["method"] == "turn/approval_requested")
                .and_then(|event| event["params"]["approval"].as_object())
                .cloned()
                .expect("approval event"),
        )
    } else {
        Value::Object(
            started["events"]
                .as_array()
                .expect("events")
                .iter()
                .find(|event| event["method"] == "item/question/requested")
                .and_then(|event| event["params"]["event"].as_object())
                .cloned()
                .expect("question event"),
        )
    };
    if kind == "approval" {
        response["action"] = Value::String("allow".to_string());
        response["scope"] = Value::String("once".to_string());
    } else {
        response["answers"] = serde_json::json!([["yes"]]);
    }
    let request_id = response["request_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let live = thread::spawn(move || {
        http_request(
            port,
            "GET",
            "/api/events?last_event_id=0&live_timeout_ms=800",
            &[
                ("Authorization", "Bearer secret"),
                ("Accept", "text/event-stream"),
            ],
            "",
        )
        .map_err(|error| error.to_string())
    });
    thread::sleep(Duration::from_millis(150));

    let response_for_thread = response.clone();
    let kind_for_thread = kind.to_string();
    let reply = thread::spawn(move || {
        let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
            .with_auth(RemoteAuth::bearer("secret"));
        if kind_for_thread == "approval" {
            client
                .respond_approval(&response_for_thread)
                .map_err(|error| error.to_string())
        } else {
            client
                .respond_question(&response_for_thread)
                .map_err(|error| error.to_string())
        }
    });

    let live_response = live
        .join()
        .map_err(|_| "live sse thread panicked".to_string())?
        .map_err(|error| format!("live sse request failed: {error}"))?;
    if kind == "approval" {
        assert!(live_response.contains("event: turn/approval_resolved"));
        assert!(live_response.contains("running"));
    } else {
        assert!(live_response.contains("event: item/question/resolved"));
        assert!(live_response.contains("answered"));
    }
    assert!(live_response.contains(&request_id));
    assert!(live_response.contains(&session_id));
    assert!(!live_response.contains("event: turn/completed"));

    let resolved = reply
        .join()
        .map_err(|_| "interaction reply thread panicked".to_string())?
        .map_err(|error| format!("interaction reply failed: {error}"))?;
    assert_eq!(resolved["status"], "completed");
    assert_eq!(resolved["turn"]["final_answer"], final_answer);

    let _ = server.kill();
    let _ = provider_thread.join();
    let requests = provider_requests.lock().expect("provider requests");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contains("function_call_output"));
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

fn free_port() -> Result<u16, Box<dyn Error>> {
    static NEXT_PORT: AtomicU16 = AtomicU16::new(0);
    if NEXT_PORT.load(Ordering::Relaxed) == 0 {
        let seed = 20_000 + (std::process::id() % 20_000) as u16;
        let _ = NEXT_PORT.compare_exchange(0, seed, Ordering::Relaxed, Ordering::Relaxed);
    }
    for _ in 0..10_000 {
        let port = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

fn temp_dir(prefix: &str) -> Result<PathBuf, Box<dyn Error>> {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_nanos()
        .to_string();
    let path = std::env::temp_dir().join(format!("{prefix}-{suffix}"));
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn read_session_event_records(
    session_root: &std::path::Path,
    session_id: &str,
    run_id: &str,
) -> Result<Vec<Value>, Box<dyn Error>> {
    let path = session_root
        .join(session_id)
        .join("runs")
        .join(run_id)
        .join("events.jsonl");
    let raw = fs::read_to_string(path)?;
    raw.lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn spawn_runtime(
    port: u16,
    workspace: &std::path::Path,
    session_root: &std::path::Path,
) -> Result<Child, Box<dyn Error>> {
    spawn_runtime_with_env(port, workspace, session_root, &[])
}

fn spawn_runtime_with_env(
    port: u16,
    workspace: &std::path::Path,
    session_root: &std::path::Path,
    envs: &[(&str, &str)],
) -> Result<Child, Box<dyn Error>> {
    let port = port.to_string();
    let mut command = Command::new(env!("CARGO_BIN_EXE_openagent-http-runtime"));
    command
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            &port,
            "--workspace",
            workspace.to_str().unwrap_or("."),
            "--session-root",
            session_root.to_str().unwrap_or("."),
            "--auth-token",
            "secret",
            "--username",
            "openagent",
            "--password",
            "pass",
            "--cors-origin",
            "http://client.test",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for key in [
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "OPENAI_MODEL",
        "OPENAI_WIRE_API",
        "OPENAGENT_API_KEY",
        "OPENAGENT_BASE_URL",
        "OPENAGENT_MODEL",
        "OPENAGENT_WIRE_API",
        "OPENAGENT_PROVIDER",
        "OPENAGENT_ACTIVE_PROVIDER",
        "OPENAGENT_AUTH_FILE",
        "OPENAGENT_MCP_CONFIG",
        "OPENAGENT_MAX_QUEUED_TURNS_PER_SESSION",
        "OPENAGENT_MAX_RUNNING_TURN_WORKERS",
        "OPENAGENT_TURN_QUEUE_LEASE_STALE_MS",
        "OPENAGENT_TURN_QUEUE_TIMEOUT_MS",
        "OPENAGENT_PROVIDER_RETRIES",
        "OPENAGENT_PROVIDER_FALLBACK_MODELS",
    ] {
        command.env_remove(key);
    }
    command.env(
        "OPENAGENT_AUTH_FILE",
        session_root
            .join("missing-auth.json")
            .to_str()
            .unwrap_or(""),
    );
    for (key, value) in envs {
        command.env(key, value);
    }
    Ok(command.spawn()?)
}

fn spawn_fake_openai_responses_provider() -> Result<(u16, thread::JoinHandle<()>), Box<dyn Error>> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    Ok((
        port,
        thread::spawn(move || {
            if let Ok((mut stream, _addr)) = listener.accept() {
                let mut buffer = [0_u8; 8192];
                let _ = stream.read(&mut buffer);
                let body = serde_json::json!({
                    "id": "resp_fake",
                    "output_text": "real provider answer",
                    "usage": {"input_tokens": 7, "output_tokens": 3}
                })
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        }),
    ))
}

fn spawn_fake_openai_responses_provider_sequence(
    responses: Vec<Value>,
) -> Result<FakeProviderServer, Box<dyn Error>> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    Ok((
        port,
        thread::spawn(move || {
            for body_value in responses {
                let Ok((mut stream, _addr)) = listener.accept() else {
                    break;
                };
                if let Ok(body) = read_http_request_body(&mut stream)
                    && let Ok(mut items) = captured.lock()
                {
                    items.push(body);
                }
                let body = body_value.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        }),
        requests,
    ))
}

fn spawn_fake_openai_responses_provider_http_sequence(
    responses: Vec<(u16, Value)>,
) -> Result<FakeProviderServer, Box<dyn Error>> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    Ok((
        port,
        thread::spawn(move || {
            for (status, body_value) in responses {
                let Ok((mut stream, _addr)) = listener.accept() else {
                    break;
                };
                if let Ok(body) = read_http_request_body(&mut stream)
                    && let Ok(mut items) = captured.lock()
                {
                    items.push(body);
                }
                let reason = if status == 200 { "OK" } else { "ERROR" };
                let body = body_value.to_string();
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        }),
        requests,
    ))
}

fn read_http_request_body(stream: &mut TcpStream) -> Result<String, Box<dyn Error>> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 8192];
    let (header_end, content_length) = loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Err("request ended before headers were complete".into());
        }
        request.extend_from_slice(&buffer[..read]);
        let Some(header_end) = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|position| position + 4)
        else {
            continue;
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or_default();
        break (header_end, content_length);
    };
    while request.len() < header_end.saturating_add(content_length) {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
    }
    let body_end = header_end.saturating_add(content_length).min(request.len());
    Ok(String::from_utf8_lossy(&request[header_end..body_end]).into_owned())
}

fn spawn_fake_mcp_server() -> Result<FakeMcpServer, Box<dyn Error>> {
    spawn_fake_mcp_server_with_limit(2)
}

fn spawn_fake_mcp_server_with_limit(max_requests: u8) -> Result<FakeMcpServer, Box<dyn Error>> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    Ok((
        port,
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut handled = 0_u8;
            while handled < max_requests && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _addr)) => {
                        handled = handled.saturating_add(1);
                        let mut buffer = [0_u8; 16384];
                        let read = stream.read(&mut buffer).unwrap_or_default();
                        let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                        let body = request
                            .split_once("\r\n\r\n")
                            .map(|(_, body)| body.to_string())
                            .unwrap_or_default();
                        if let Ok(mut items) = captured.lock() {
                            items.push(body.clone());
                        }
                        let request_json = serde_json::from_str::<Value>(&body)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        let id = request_json.get("id").cloned().unwrap_or(Value::Null);
                        let method = request_json
                            .get("method")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let result = if method == "tools/list" {
                            serde_json::json!({
                                "tools": [{
                                    "name": "echo",
                                    "title": "Echo",
                                    "description": "Echo input",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "text": {"type": "string"}
                                        }
                                    }
                                }]
                            })
                        } else {
                            let text = request_json["params"]["arguments"]["text"]
                                .as_str()
                                .unwrap_or_default();
                            serde_json::json!({
                                "content": [{
                                    "type": "text",
                                    "text": format!("mcp echo: {text}")
                                }]
                            })
                        };
                        let body = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": result,
                        })
                        .to_string();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        }),
        requests,
    ))
}

fn spawn_fake_docs_server(body: &str) -> Result<FakeDocsServer, Box<dyn Error>> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let body = body.to_string();
    Ok((
        port,
        thread::spawn(move || {
            let Ok((mut stream, _addr)) = listener.accept() else {
                return;
            };
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }),
    ))
}

fn spawn_fake_openai_responses_provider_sequence_with_delays(
    responses: Vec<(Value, u64)>,
) -> Result<FakeProviderServer, Box<dyn Error>> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    Ok((
        port,
        thread::spawn(move || {
            for (body_value, delay_ms) in responses {
                let Ok((mut stream, _addr)) = listener.accept() else {
                    break;
                };
                let mut buffer = [0_u8; 16384];
                let read = stream.read(&mut buffer).unwrap_or_default();
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                if let Some((_, body)) = request.split_once("\r\n\r\n") {
                    if let Ok(mut items) = captured.lock() {
                        items.push(body.to_string());
                    }
                }
                if delay_ms > 0 {
                    thread::sleep(Duration::from_millis(delay_ms));
                }
                let body = body_value.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        }),
        requests,
    ))
}

fn spawn_fake_openai_responses_streaming_provider() -> Result<FakeProviderServer, Box<dyn Error>> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    Ok((
        port,
        thread::spawn(move || {
            if let Ok((mut stream, _addr)) = listener.accept() {
                let mut buffer = [0_u8; 16384];
                let read = stream.read(&mut buffer).unwrap_or_default();
                if let Ok(mut items) = captured.lock() {
                    items.push(String::from_utf8_lossy(&buffer[..read]).to_string());
                }
                let headers = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream; charset=utf-8\r\nconnection: close\r\n\r\n";
                let _ = stream.write_all(headers.as_bytes());
                let _ = stream.write_all(
                    b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"streamed \"}\n\n",
                );
                let _ = stream.flush();
                thread::sleep(Duration::from_millis(1500));
                let _ = stream.write_all(
                    b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"answer\"}\n\n",
                );
                let _ = stream.write_all(
                    b"data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":11,\"output_tokens\":2}}}\n\n",
                );
                let _ = stream.write_all(b"data: [DONE]\n\n");
                let _ = stream.flush();
            }
        }),
        requests,
    ))
}

fn spawn_fake_openai_responses_streaming_tool_then_delayed_final_provider()
-> Result<FakeProviderServer, Box<dyn Error>> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    Ok((
        port,
        thread::spawn(move || {
            if let Ok((mut stream, _addr)) = listener.accept() {
                let mut buffer = [0_u8; 16384];
                let read = stream.read(&mut buffer).unwrap_or_default();
                if let Ok(mut items) = captured.lock() {
                    items.push(String::from_utf8_lossy(&buffer[..read]).to_string());
                }
                let headers = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream; charset=utf-8\r\nconnection: close\r\n\r\n";
                let tool_event = serde_json::json!({
                    "type": "response.output_item.done",
                    "item": {
                        "type": "function_call",
                        "call_id": "call_live_read",
                        "name": "read",
                        "arguments": "{\"file_path\":\"notes.txt\"}",
                    }
                })
                .to_string();
                let usage_event = serde_json::json!({
                    "type": "response.completed",
                    "response": {"usage": {"input_tokens": 5, "output_tokens": 1}}
                })
                .to_string();
                let _ = stream.write_all(headers.as_bytes());
                let _ = stream.write_all(format!("data: {tool_event}\n\n").as_bytes());
                let _ = stream.write_all(format!("data: {usage_event}\n\n").as_bytes());
                let _ = stream.write_all(b"data: [DONE]\n\n");
                let _ = stream.flush();
            }

            if let Ok((mut stream, _addr)) = listener.accept() {
                let mut buffer = [0_u8; 16384];
                let read = stream.read(&mut buffer).unwrap_or_default();
                if let Ok(mut items) = captured.lock() {
                    items.push(String::from_utf8_lossy(&buffer[..read]).to_string());
                }
                thread::sleep(Duration::from_millis(1500));
                let body = serde_json::json!({
                    "id": "resp_final_after_tool",
                    "output_text": "tool final answer",
                    "usage": {"input_tokens": 12, "output_tokens": 3}
                })
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        }),
        requests,
    ))
}

fn wait_for_server(port: u16) -> Result<(), Box<dyn Error>> {
    for _ in 0..300 {
        if authorized_request(port, "GET", "/api/health", "", false).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err("server did not start".into())
}

fn wait_for_task_status(
    client: &RemoteRuntimeClient,
    session_id: &str,
    task_id: &str,
    expected: &str,
) -> Result<Value, Box<dyn Error>> {
    for _ in 0..100 {
        let tasks = client.tasks(session_id)?;
        if let Some(task) = tasks.iter().find(|task| task["session_id"] == task_id)
            && task["status"] == expected
        {
            return Ok(task.clone());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(format!("task {task_id} did not reach status {expected}").into())
}

fn message_parts_by_kind<'a>(messages: &'a Value, kind: &str) -> Vec<&'a Value> {
    messages
        .get("messages_v2")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|message| {
            message
                .get("parts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter(|part| part.get("kind").and_then(Value::as_str) == Some(kind))
        .collect()
}

fn authorized_request(
    port: u16,
    method: &str,
    path: &str,
    body: &str,
    raw: bool,
) -> Result<String, Box<dyn Error>> {
    let response = http_request(
        port,
        method,
        path,
        &[("Authorization", "Bearer secret")],
        body,
    )?;
    if raw || response.starts_with("HTTP/1.1 2") {
        return Ok(response);
    }
    Err(format!("request failed: {response}").into())
}

fn http_request(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Result<String, Box<dyn Error>> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if !body.is_empty() {
        request.push_str("Content-Type: application/json\r\n");
    }
    for (key, value) in headers {
        request.push_str(&format!("{key}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

fn json_body(response: &str) -> Result<Value, Box<dyn Error>> {
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or(response);
    Ok(serde_json::from_str(body)?)
}
