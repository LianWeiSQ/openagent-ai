use std::{io::IsTerminal, path::PathBuf};

use openagent_bridge_server_client::RemoteAuth;

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        println!(
            "Usage: openagent-tui [--attach <url>] [--workspace <path>] [--session <id>] [--continue] [--fork] [--server-token <token>] [-u|--username <name>] [-p|--password <password>] [--permission <ruleset>] [--dangerously-skip-permissions]"
        );
        return;
    }
    if !std::io::stdin().is_terminal() {
        println!("{}", openagent_tui::command_name());
        return;
    }
    let server_url = value_for(&args, &["--attach", "--server-url"])
        .or_else(|| std::env::var("OPENAGENT_SERVER_URL").ok())
        .unwrap_or_else(|| "http://127.0.0.1:8787".to_string());
    let auth = RemoteAuth {
        token: value_for(&args, &["--server-token"])
            .or_else(|| std::env::var("OPENAGENT_SERVER_TOKEN").ok()),
        username: value_for(&args, &["--username", "-u"]),
        password: value_for(&args, &["--password", "-p"]),
    };
    let workspace = value_for(&args, &["--workspace", "--dir"])
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let options = openagent_tui::BridgeTerminalOptions {
        server_url: server_url.clone(),
        auth,
        workspace,
        session_id: value_for(&args, &["--session", "-s"]),
        continue_last: has_flag(&args, &["--continue", "-c"]),
        fork: has_flag(&args, &["--fork"]),
        permission: value_for(&args, &["--permission"]),
        dangerously_skip_permissions: has_flag(&args, &["--dangerously-skip-permissions"]),
    };
    let handler = match openagent_tui::BridgeTerminalHandler::connect(options) {
        Ok(handler) => handler,
        Err(error) => {
            eprintln!(
                "failed to connect to Bridge API at {server_url}: {error}\nstart one with: openagent serve --host 127.0.0.1 --port 8787"
            );
            std::process::exit(1);
        }
    };
    if let Err(error) = openagent_tui::run_terminal_ui(
        openagent_tui::TerminalUiOptions {
            title: format!("OpenAgent Bridge: {server_url}"),
            status: "connected".to_string(),
        },
        handler,
    ) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn has_flag(args: &[String], names: &[&str]) -> bool {
    args.iter().any(|arg| names.contains(&arg.as_str()))
}

fn value_for(args: &[String], names: &[&str]) -> Option<String> {
    for arg in args {
        if let Some((name, value)) = arg.split_once('=')
            && names.contains(&name)
        {
            return Some(value.to_string());
        }
    }
    args.windows(2)
        .find(|items| names.contains(&items[0].as_str()))
        .and_then(|items| items.get(1))
        .cloned()
}
