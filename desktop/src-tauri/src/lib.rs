use std::{
    env, fs,
    io::Write,
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
struct DiagnosticPath {
    source: String,
    path: String,
    exists: bool,
}

#[derive(Debug, Serialize)]
struct DesktopDiagnostics {
    runtime: String,
    app_version: String,
    os: String,
    arch: String,
    bridge_default_url: String,
    bridge_url_env: Option<String>,
    bridge_binary: Option<DiagnosticPath>,
    bridge_binary_candidates: Vec<DiagnosticPath>,
    workspace_default: String,
    workspace_default_source: String,
    session_root_default: String,
    warnings: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectPathRequest {
    path: String,
}

#[derive(Debug, Serialize)]
struct ProjectPathInfo {
    input: String,
    path: String,
    name: String,
    exists: bool,
    is_dir: bool,
    canonical: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct DesktopAuthToken {
    token: String,
    path: String,
    created: bool,
}

#[derive(Default)]
struct AppBridgeProcess {
    child: Mutex<Option<ManagedBridgeChild>>,
}

struct ManagedBridgeChild {
    child: Child,
    pid: u32,
    url: String,
    port: u16,
    workspace: String,
    session_root: String,
    binary: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppBridgeStartOptions {
    workspace: Option<String>,
    session_root: Option<String>,
    port: Option<u16>,
    auth_token: Option<String>,
}

#[derive(Debug, Serialize)]
struct AppBridgeStatus {
    running: bool,
    pid: Option<u32>,
    url: String,
    port: u16,
    workspace: String,
    session_root: String,
    binary: Option<String>,
    error: Option<String>,
}

impl AppBridgeProcess {
    fn status(&self) -> AppBridgeStatus {
        let mut guard = self
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            return stopped_bridge_status(None);
        }

        let exited_status = {
            let managed = guard.as_mut().expect("guard checked above");
            match managed.child.try_wait() {
                Ok(None) => return managed.to_status(true, None),
                Ok(Some(status)) => Some(managed.to_status(
                    false,
                    Some(format!("openagent-http-runtime exited with {status}")),
                )),
                Err(error) => {
                    return managed.to_status(
                        true,
                        Some(format!("failed to inspect bridge process: {error}")),
                    );
                }
            }
        };
        *guard = None;
        exited_status.unwrap_or_else(|| stopped_bridge_status(None))
    }

    fn start(&self, options: AppBridgeStartOptions) -> Result<AppBridgeStatus, String> {
        let mut guard = self
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_some() {
            let already_running = {
                let managed = guard.as_mut().expect("guard checked above");
                match managed.child.try_wait() {
                    Ok(None) => Some(managed.to_status(true, None)),
                    Ok(Some(_)) => None,
                    Err(error) => Some(managed.to_status(
                        true,
                        Some(format!("failed to inspect bridge process: {error}")),
                    )),
                }
            };
            if let Some(status) = already_running {
                return Ok(status);
            }
            *guard = None;
        }

        let binary = find_bridge_binary().ok_or_else(|| {
            "openagent-http-runtime not found. Build it with `cargo build -p openagent-http-runtime` or set OPENAGENT_HTTP_RUNTIME.".to_string()
        })?;
        let port = options.port.unwrap_or_else(default_bridge_port);
        if port == 0 {
            return Err("bridge port must be greater than 0".to_string());
        }
        let workspace = non_empty(options.workspace).unwrap_or_else(default_workspace);
        let session_root = non_empty(options.session_root)
            .unwrap_or_else(|| default_session_root().display().to_string());
        fs::create_dir_all(&session_root)
            .map_err(|error| format!("failed to create session root `{session_root}`: {error}"))?;

        let url = format!("http://127.0.0.1:{port}");
        let mut command = Command::new(&binary.path);
        command
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .arg("--workspace")
            .arg(&workspace)
            .arg("--session-root")
            .arg(&session_root)
            .arg("--headless")
            .arg("--cors-origin")
            .arg("*")
            .arg("--no-mdns")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(token) = non_empty(options.auth_token) {
            command.arg("--auth-token").arg(token);
        }
        let workspace_path = PathBuf::from(&workspace);
        if workspace_path.exists() {
            command.current_dir(workspace_path);
        }

        let mut child = command
            .spawn()
            .map_err(|error| format!("failed to start openagent-http-runtime: {error}"))?;
        wait_for_bridge_port(&mut child, port)?;

        let managed = ManagedBridgeChild {
            pid: child.id(),
            child,
            url,
            port,
            workspace,
            session_root,
            binary: binary.path,
        };
        let status = managed.to_status(true, None);
        *guard = Some(managed);
        Ok(status)
    }

    fn stop(&self) -> Result<AppBridgeStatus, String> {
        let mut guard = self
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(mut managed) = guard.take() else {
            return Ok(stopped_bridge_status(None));
        };
        let status = managed.to_status(false, None);
        let needs_kill = !matches!(managed.child.try_wait(), Ok(Some(_)));
        if needs_kill {
            let _ = managed.child.kill();
        }
        let _ = managed.child.wait();
        Ok(status)
    }

    fn restart(&self, options: AppBridgeStartOptions) -> Result<AppBridgeStatus, String> {
        let _ = self.stop()?;
        thread::sleep(Duration::from_millis(80));
        self.start(options)
    }
}

impl Drop for AppBridgeProcess {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut managed) = guard.take() {
                let _ = managed.child.kill();
                let _ = managed.child.wait();
            }
        }
    }
}

impl ManagedBridgeChild {
    fn to_status(&self, running: bool, error: Option<String>) -> AppBridgeStatus {
        AppBridgeStatus {
            running,
            pid: Some(self.pid),
            url: self.url.clone(),
            port: self.port,
            workspace: self.workspace.clone(),
            session_root: self.session_root.clone(),
            binary: Some(self.binary.clone()),
            error,
        }
    }
}

#[tauri::command]
fn desktop_diagnostics() -> DesktopDiagnostics {
    let bridge_default_url = default_bridge_url();
    let candidates = bridge_binary_candidates();
    let bridge_binary = candidates
        .iter()
        .find(|candidate| candidate.exists)
        .cloned();
    let mut warnings = Vec::new();
    if bridge_binary.is_none() {
        warnings.push(
            "openagent-http-runtime not found in env, bundle, repo target, or PATH".to_string(),
        );
    }

    DesktopDiagnostics {
        runtime: "tauri".to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        os: env::consts::OS.to_string(),
        arch: env::consts::ARCH.to_string(),
        bridge_default_url,
        bridge_url_env: env::var("OPENAGENT_BRIDGE_URL")
            .or_else(|_| env::var("OPENAGENT_APP_BRIDGE_URL"))
            .ok(),
        bridge_binary,
        bridge_binary_candidates: candidates,
        workspace_default: default_workspace(),
        workspace_default_source: if env::var_os("OPENAGENT_WORKSPACE").is_some() {
            "env".to_string()
        } else {
            "cwd".to_string()
        },
        session_root_default: default_session_root().display().to_string(),
        warnings,
    }
}

#[tauri::command]
fn app_bridge_status(state: tauri::State<'_, AppBridgeProcess>) -> AppBridgeStatus {
    state.status()
}

#[tauri::command]
fn app_bridge_start(
    options: AppBridgeStartOptions,
    state: tauri::State<'_, AppBridgeProcess>,
) -> Result<AppBridgeStatus, String> {
    state.start(options)
}

#[tauri::command]
fn app_bridge_stop(state: tauri::State<'_, AppBridgeProcess>) -> Result<AppBridgeStatus, String> {
    state.stop()
}

#[tauri::command]
fn app_bridge_restart(
    options: AppBridgeStartOptions,
    state: tauri::State<'_, AppBridgeProcess>,
) -> Result<AppBridgeStatus, String> {
    state.restart(options)
}

#[tauri::command]
fn desktop_auth_token() -> Result<DesktopAuthToken, String> {
    let path = desktop_auth_token_path();
    if let Ok(existing) = fs::read_to_string(&path) {
        let token = existing.trim().to_string();
        if !token.is_empty() {
            return Ok(DesktopAuthToken {
                token,
                path: path.display().to_string(),
                created: false,
            });
        }
    }

    let token = generate_bridge_token()?;
    write_secret_file(&path, &token)?;
    Ok(DesktopAuthToken {
        token,
        path: path.display().to_string(),
        created: true,
    })
}

#[tauri::command]
fn project_path_info(request: ProjectPathRequest) -> ProjectPathInfo {
    project_path_info_for_input(request.path)
}

#[tauri::command]
fn choose_project_folder() -> Option<ProjectPathInfo> {
    rfd::FileDialog::new()
        .set_title("Choose OpenAgent project")
        .pick_folder()
        .map(|path| project_path_info_for_input(path.display().to_string()))
}

fn project_path_info_for_input(input: String) -> ProjectPathInfo {
    let trimmed = input.trim().to_string();
    if trimmed.is_empty() {
        return ProjectPathInfo {
            input: trimmed,
            path: String::new(),
            name: String::new(),
            exists: false,
            is_dir: false,
            canonical: None,
            error: Some("project path is required".to_string()),
        };
    }

    let path = expand_user_path(&trimmed);
    let metadata = fs::metadata(&path);
    let exists = metadata.is_ok();
    let is_dir = metadata.as_ref().is_ok_and(|metadata| metadata.is_dir());
    let canonical = fs::canonicalize(&path)
        .ok()
        .map(|path| path.display().to_string());
    let display_path = canonical
        .clone()
        .unwrap_or_else(|| path.display().to_string());
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| display_path.clone());
    let error = match metadata {
        Ok(metadata) if !metadata.is_dir() => Some("project path is not a directory".to_string()),
        Ok(_) => None,
        Err(error) => Some(error.to_string()),
    };

    ProjectPathInfo {
        input: trimmed,
        path: display_path,
        name,
        exists,
        is_dir,
        canonical,
        error,
    }
}

fn bridge_binary_candidates() -> Vec<DiagnosticPath> {
    let binary = binary_name("openagent-http-runtime");
    let mut candidates = Vec::new();
    if let Some(path) = env::var_os("OPENAGENT_HTTP_RUNTIME") {
        candidates.push(diagnostic_path("env", PathBuf::from(path)));
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(diagnostic_path("bundle-next-to-exe", dir.join(binary)));
            candidates.push(diagnostic_path(
                "bundle-resources",
                dir.join("../Resources").join(binary),
            ));
        }
    }
    if !development_runtime_fallback_enabled() {
        return candidates;
    }
    candidates.push(diagnostic_path(
        "repo-target-debug",
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("target")
            .join("debug")
            .join(binary),
    ));
    candidates.push(diagnostic_path(
        "repo-target-release",
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("target")
            .join("release")
            .join(binary),
    ));
    candidates.extend(path_candidates(binary));
    candidates
}

fn path_candidates(binary: &str) -> Vec<DiagnosticPath> {
    env::var_os("PATH")
        .map(|paths| {
            env::split_paths(&paths)
                .map(|path| diagnostic_path("path", path.join(binary)))
                .collect()
        })
        .unwrap_or_default()
}

fn diagnostic_path(source: &str, path: PathBuf) -> DiagnosticPath {
    DiagnosticPath {
        source: source.to_string(),
        exists: path.exists(),
        path: path.display().to_string(),
    }
}

fn find_bridge_binary() -> Option<DiagnosticPath> {
    bridge_binary_candidates()
        .into_iter()
        .find(|candidate| candidate.exists)
}

fn wait_for_bridge_port(child: &mut Child, port: u16) -> Result<(), String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(format!(
                    "openagent-http-runtime exited during startup with {status}"
                ));
            }
            Ok(None) => {}
            Err(error) => {
                return Err(format!("failed to inspect bridge startup: {error}"));
            }
        }

        if TcpStream::connect_timeout(&addr, Duration::from_millis(120)).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "openagent-http-runtime did not listen on 127.0.0.1:{port} within startup timeout"
            ));
        }
        thread::sleep(Duration::from_millis(60));
    }
}

fn stopped_bridge_status(error: Option<String>) -> AppBridgeStatus {
    AppBridgeStatus {
        running: false,
        pid: None,
        url: default_bridge_url(),
        port: default_bridge_port(),
        workspace: default_workspace(),
        session_root: default_session_root().display().to_string(),
        binary: find_bridge_binary().map(|binary| binary.path),
        error,
    }
}

fn default_bridge_url() -> String {
    env::var("OPENAGENT_BRIDGE_URL")
        .or_else(|_| env::var("OPENAGENT_APP_BRIDGE_URL"))
        .unwrap_or_else(|_| format!("http://127.0.0.1:{}", default_bridge_port()))
}

fn default_bridge_port() -> u16 {
    env::var("OPENAGENT_BRIDGE_PORT")
        .ok()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .filter(|port| *port > 0)
        .unwrap_or(8787)
}

fn development_runtime_fallback_enabled() -> bool {
    env::var("OPENAGENT_DESKTOP_DISABLE_DEV_RUNTIME_FALLBACK")
        .ok()
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(true)
}

fn default_workspace() -> String {
    env::var("OPENAGENT_WORKSPACE")
        .ok()
        .and_then(|value| non_empty(Some(value)))
        .unwrap_or_else(|| {
            env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .display()
                .to_string()
        })
}

fn desktop_auth_token_path() -> PathBuf {
    env::var_os("OPENAGENT_DESKTOP_AUTH_TOKEN_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| openagent_home().join("desktop").join("bridge-auth-token"))
}

fn openagent_home() -> PathBuf {
    env::var_os("OPENAGENT_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| Path::new(&home).join(".openagent")))
        .or_else(|| env::var_os("USERPROFILE").map(|home| Path::new(&home).join(".openagent")))
        .unwrap_or_else(|| PathBuf::from(".openagent"))
}

fn generate_bridge_token() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("failed to generate auth token: {error}"))?;
    let mut token = String::with_capacity("oa_desktop_".len() + bytes.len() * 2);
    token.push_str("oa_desktop_");
    for byte in bytes {
        token.push_str(&format!("{byte:02x}"));
    }
    Ok(token)
}

fn write_secret_file(path: &Path, token: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create auth token directory `{}`: {error}",
                parent.display()
            )
        })?;
    }

    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        format!(
            "failed to open auth token file `{}`: {error}",
            path.display()
        )
    })?;
    file.write_all(token.as_bytes())
        .and_then(|_| file.write_all(b"\n"))
        .map_err(|error| {
            format!(
                "failed to write auth token file `{}`: {error}",
                path.display()
            )
        })
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn expand_user_path(input: &str) -> PathBuf {
    if input == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    }
    if let Some(rest) = input
        .strip_prefix("~/")
        .or_else(|| input.strip_prefix("~\\"))
    {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(input)
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
}

fn binary_name(name: &'static str) -> &'static str {
    if cfg!(windows) {
        "openagent-http-runtime.exe"
    } else {
        name
    }
}

fn default_session_root() -> PathBuf {
    env::var_os("OPENAGENT_SESSION_ROOT")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME").map(|home| Path::new(&home).join(".openagent").join("sessions"))
        })
        .unwrap_or_else(|| PathBuf::from(".openagent").join("sessions"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppBridgeProcess::default())
        .invoke_handler(tauri::generate_handler![
            desktop_diagnostics,
            app_bridge_status,
            app_bridge_start,
            app_bridge_stop,
            app_bridge_restart,
            desktop_auth_token,
            project_path_info,
            choose_project_folder
        ])
        .run(tauri::generate_context!())
        .expect("failed to run OpenAgent Desktop");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Read,
        net::TcpListener,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn temp_root(name: &str) -> PathBuf {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis();
        env::temp_dir().join(format!("{name}-{}-{millis}", std::process::id()))
    }

    fn free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind free port");
        listener.local_addr().expect("addr").port()
    }

    fn http_get(port: u16, path: &str, token: Option<&str>) -> std::io::Result<String> {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let auth = token
            .map(|value| format!("Authorization: Bearer {value}\r\n"))
            .unwrap_or_default();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\n{auth}Connection: close\r\n\r\n"
        )?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        Ok(response)
    }

    #[test]
    fn desktop_auth_token_persists_with_override() {
        let root = env::temp_dir().join(format!(
            "openagent-desktop-auth-token-{}",
            std::process::id()
        ));
        let path = root.join("bridge-auth-token");
        let _ = fs::remove_dir_all(&root);
        env::set_var("OPENAGENT_DESKTOP_AUTH_TOKEN_PATH", &path);

        let first = desktop_auth_token().expect("first token");
        let second = desktop_auth_token().expect("second token");

        assert!(first.created);
        assert!(!second.created);
        assert_eq!(first.path, path.display().to_string());
        assert_eq!(first.token, second.token);
        assert!(first.token.starts_with("oa_desktop_"));
        assert!(first.token.len() >= 32);

        env::remove_var("OPENAGENT_DESKTOP_AUTH_TOKEN_PATH");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn managed_bridge_starts_restarts_and_stops_runtime() {
        let root = temp_root("openagent-desktop-managed-bridge");
        let workspace_a = root.join("workspace-a");
        let workspace_b = root.join("workspace-b");
        let session_root = root.join("sessions");
        fs::create_dir_all(&workspace_a).expect("workspace a");
        fs::create_dir_all(&workspace_b).expect("workspace b");
        let port = free_port();
        let token = "oa_desktop_test_managed_bridge";
        let process = AppBridgeProcess::default();

        let started = process
            .start(AppBridgeStartOptions {
                workspace: Some(workspace_a.display().to_string()),
                session_root: Some(session_root.display().to_string()),
                port: Some(port),
                auth_token: Some(token.to_string()),
            })
            .expect("start bridge");
        assert!(started.running);
        assert_eq!(started.port, port);
        assert_eq!(started.workspace, workspace_a.display().to_string());
        assert!(started.pid.is_some());

        let unauthorized = http_get(port, "/api/health", None).expect("unauthorized health");
        assert!(unauthorized.contains("401 Unauthorized"));
        let authorized = http_get(port, "/api/health", Some(token)).expect("authorized health");
        assert!(authorized.contains("200 OK"));
        assert!(authorized.contains("\"ok\""));
        assert!(authorized.contains("true"));

        let status = process.status();
        assert!(status.running);
        assert_eq!(status.workspace, workspace_a.display().to_string());

        let restarted = process
            .restart(AppBridgeStartOptions {
                workspace: Some(workspace_b.display().to_string()),
                session_root: Some(session_root.display().to_string()),
                port: Some(port),
                auth_token: Some(token.to_string()),
            })
            .expect("restart bridge");
        assert!(restarted.running);
        assert_eq!(restarted.workspace, workspace_b.display().to_string());
        let authorized_after_restart =
            http_get(port, "/api/health", Some(token)).expect("authorized health after restart");
        assert!(authorized_after_restart.contains("200 OK"));
        assert!(authorized_after_restart.contains("\"ok\""));
        assert!(authorized_after_restart.contains("true"));

        let stopped = process.stop().expect("stop bridge");
        assert!(!stopped.running);
        let final_status = process.status();
        assert!(!final_status.running);
        assert!(final_status.pid.is_none());
        assert!(http_get(port, "/api/health", Some(token)).is_err());

        let _ = fs::remove_dir_all(&root);
    }
}
