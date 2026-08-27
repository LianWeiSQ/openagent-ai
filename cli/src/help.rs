use super::*;

pub(super) struct HelpSection<'a> {
    pub(super) title: &'a str,
    pub(super) rows: &'a [(&'a str, &'a str)],
}

pub(super) fn render_help_screen(
    title: &str,
    summary: &str,
    usage: &str,
    sections: &[HelpSection<'_>],
    examples: &[&str],
) -> String {
    let mut output = String::new();
    output.push_str(title);
    output.push('\n');
    if !summary.is_empty() {
        output.push_str("  ");
        output.push_str(summary);
        output.push('\n');
    }
    output.push('\n');
    output.push_str("Usage\n");
    output.push_str("  ");
    output.push_str(usage);
    output.push('\n');
    for section in sections {
        output.push('\n');
        output.push_str(section.title);
        output.push('\n');
        push_aligned_rows(&mut output, section.rows);
    }
    if !examples.is_empty() {
        output.push('\n');
        output.push_str("Examples\n");
        for example in examples {
            output.push_str("  ");
            output.push_str(example);
            output.push('\n');
        }
    }
    output.trim_end().to_string()
}

pub(super) fn push_aligned_rows(output: &mut String, rows: &[(&str, &str)]) {
    let width = rows
        .iter()
        .map(|(label, _)| label.len())
        .max()
        .unwrap_or(0)
        .clamp(12, 34);
    for (label, description) in rows {
        output.push_str("  ");
        if label.len() > width {
            output.push_str(label);
            output.push('\n');
            output.push_str("  ");
            output.push_str(&" ".repeat(width));
            output.push_str("  ");
            output.push_str(description);
            output.push('\n');
        } else {
            output.push_str(&format!("{label:<width$}  {description}"));
            output.push('\n');
        }
    }
}

pub(super) fn render_key_values(title: &str, rows: &[(&str, String)]) -> String {
    let mut output = String::new();
    output.push_str(title);
    output.push('\n');
    let width = rows
        .iter()
        .map(|(label, _)| label.len())
        .max()
        .unwrap_or(0)
        .clamp(8, 24);
    for (label, value) in rows {
        output.push_str("  ");
        output.push_str(&format!("{label:<width$}  {value}"));
        output.push('\n');
    }
    output.trim_end().to_string()
}

pub(super) fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let mut widths = headers
        .iter()
        .map(|header| header.len())
        .collect::<Vec<_>>();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            if let Some(width) = widths.get_mut(index) {
                *width = (*width).max(cell.len());
            }
        }
    }
    let mut output = String::new();
    push_table_row(&mut output, headers, &widths);
    let separators = widths
        .iter()
        .map(|width| "-".repeat(*width))
        .collect::<Vec<_>>();
    let separator_refs = separators.iter().map(String::as_str).collect::<Vec<_>>();
    push_table_row(&mut output, &separator_refs, &widths);
    for row in rows {
        let row_refs = row.iter().map(String::as_str).collect::<Vec<_>>();
        push_table_row(&mut output, &row_refs, &widths);
    }
    output.trim_end().to_string()
}

pub(super) fn push_table_row(output: &mut String, cells: &[&str], widths: &[usize]) {
    output.push_str("  ");
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            output.push_str("  ");
        }
        let width = widths.get(index).copied().unwrap_or(cell.len());
        output.push_str(&format!("{cell:<width$}"));
    }
    while output.ends_with(' ') {
        output.pop();
    }
    output.push('\n');
}

pub(super) fn compact_text_value(value: &Value) -> String {
    match value {
        Value::Null => "-".to_string(),
        Value::String(value) if value.is_empty() => "-".to_string(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => {
            if *value {
                "yes".to_string()
            } else {
                "no".to_string()
            }
        }
        Value::Number(value) => value.to_string(),
        _ => stable_json_dumps(value),
    }
}

pub(super) fn has_flag(args: &[String], names: &[&str]) -> bool {
    args.iter().any(|arg| names.contains(&arg.as_str()))
}

pub(super) fn value_for(args: &[String], names: &[&str]) -> Option<String> {
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

pub(super) fn values_for(args: &[String], names: &[&str]) -> Vec<String> {
    let mut values = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if names.contains(&args[index].as_str()) && index + 1 < args.len() {
            if let Some(value) = args.get(index + 1) {
                values.push(value.clone());
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    values
}

pub(super) fn positional_args(args: &[String], value_flags: &[&str]) -> Vec<String> {
    let mut values = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            values.extend(args.iter().skip(index + 1).cloned());
            break;
        }
        if arg.starts_with('-') {
            index += if value_flags.contains(&arg.as_str()) && index + 1 < args.len() {
                2
            } else {
                1
            };
            continue;
        }
        values.push(arg.clone());
        index += 1;
    }
    values
}

#[allow(dead_code)]
pub(super) fn simple_command(
    name: &str,
    args: &[String],
    help: &'static str,
    non_help_message: &'static str,
) -> CliRunResult {
    if args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text(help);
    }
    CliRunResult {
        exit_code: 0,
        stdout: format!("{non_help_message}\n"),
        stderr: if name == "tui" {
            String::new()
        } else {
            "Use --help for flags; live long-running server/UI execution is validated by the dedicated Rust binary crates.\n".to_string()
        },
    }
}

pub(super) fn root_help() -> String {
    let core = [
        ("run", "run an agent prompt without opening the TUI"),
        ("tui", "start the terminal UI"),
        ("attach", "attach to a running Bridge server"),
        (
            "terminal",
            "run a workspace-scoped command through Bridge API",
        ),
        ("serve", "start the local Bridge API HTTP server"),
        ("client", "send a prompt to a running Bridge server"),
    ];
    let workspace = [
        (
            "session",
            "list, export, import, share, and delete sessions",
        ),
        ("task", "inspect and control durable local background tasks"),
        ("models", "inspect provider and model metadata"),
        ("stats", "show local session usage statistics"),
        ("command", "manage custom prompt commands"),
        ("config", "inspect and initialize CLI configuration"),
        ("doctor", "check provider and gateway configuration"),
        ("lsp", "inspect and query Language Server Protocol servers"),
    ];
    let integrations = [
        ("auth", "manage provider credentials"),
        ("providers", "provider credential alias for auth"),
        ("mcp", "manage local and remote MCP servers"),
        ("approval", "inspect or answer queued approval requests"),
        ("question", "inspect or answer queued question requests"),
    ];
    let parity = [
        ("agent, plugin", "agent profiles and plugin command surface"),
        ("github, pr", "GitHub issue, workflow, and PR helpers"),
        (
            "debug, db, acp",
            "diagnostics, local database, and ACP helpers",
        ),
        (
            "upgrade, uninstall, import, export, generate, console",
            "OpenCode parity lifecycle and utility commands",
        ),
    ];
    render_help_screen(
        "OpenAgent CLI",
        "Agent workflows, sessions, tools, and model routing from one terminal.",
        "openagent <command> [options]",
        &[
            HelpSection {
                title: "Core Commands",
                rows: &core,
            },
            HelpSection {
                title: "Workspace",
                rows: &workspace,
            },
            HelpSection {
                title: "Integrations",
                rows: &integrations,
            },
            HelpSection {
                title: "OpenCode Parity Surface",
                rows: &parity,
            },
        ],
        &[
            "openagent run --stream \"summarize this repo\"",
            "openagent tui --attach http://127.0.0.1:8787",
            "openagent models --catalog --verbose",
        ],
    )
}

pub(super) fn run_help() -> String {
    let session = [
        ("-c, --continue", "continue the latest session"),
        ("-s, --session <id>", "continue a specific session"),
        ("--fork", "fork before continuing"),
        ("--share", "mark the session shareable"),
        ("--title <title>", "set the session title"),
        ("--session-root <path>", "session store root"),
    ];
    let model = [
        ("-m, --model <provider/model>", "provider/model override"),
        ("--provider <id>", "provider override"),
        ("--agent <name>", "agent profile to use"),
        (
            "--agents <json-or-path>",
            "startup subagent profiles as inline JSON or JSON/YAML file",
        ),
        ("--variant <name>", "provider-specific variant"),
        ("--wire-api <chat|responses>", "OpenAI-compatible wire API"),
        ("--base-url <url>", "provider base URL"),
    ];
    let input = [
        ("--command <name>", "render a custom command template"),
        ("-f, --file <path>", "attach a file; repeatable"),
        ("--mcp-config <path-or-json>", "enable MCP tools"),
        (
            "--answer <text>",
            "pre-answer a queued question; repeatable",
        ),
        ("--dir, --workspace <path>", "workspace path"),
    ];
    let runtime = [
        ("--stream", "emit provider deltas as they arrive"),
        ("--format <text|json|default>", "output format"),
        ("--thinking", "show thinking blocks when available"),
        ("--interactive, -i", "run direct interactive mode"),
        ("--permission <ruleset>", "FULL, READONLY, PLAN_ONLY, NONE"),
        (
            "--dangerously-skip-permissions",
            "auto-approve permissions that are not denied",
        ),
        ("--skip-doctor", "skip local gateway preflight"),
    ];
    let remote = [
        ("--attach <url>", "run through a remote Bridge server"),
        ("--server-token <token>", "bearer token for --attach"),
        ("-u, --username <name>", "basic auth username"),
        ("-p, --password <password>", "basic auth password"),
    ];
    render_help_screen(
        "OpenAgent Run",
        "Start, resume, or attach an agent loop from the command line.",
        "openagent run [message..] [options]",
        &[
            HelpSection {
                title: "Session",
                rows: &session,
            },
            HelpSection {
                title: "Model And Agent",
                rows: &model,
            },
            HelpSection {
                title: "Input",
                rows: &input,
            },
            HelpSection {
                title: "Runtime",
                rows: &runtime,
            },
            HelpSection {
                title: "Remote Attach",
                rows: &remote,
            },
        ],
        &[
            "openagent run --stream \"fix the failing tests\"",
            "openagent run --agent reviewer --command review src/core.rs",
            "openagent approval respond --decision allow_once && openagent run --continue",
        ],
    )
}

pub(super) fn tui_help() -> &'static str {
    "Usage: openagent tui [options]\n\n\
     Options: --workspace <path>, --session-root <path>, -s/--session <id>, -c/--continue, --fork, --model <provider/model>, --agent <name>, --prompt <text>, --attach <url>, --server-token <token>, -u/--username <name>, -p/--password <password>, --skip-doctor"
}

pub(super) fn serve_help() -> &'static str {
    "Usage: openagent serve [options]\n\n\
     Options: --host <host>, --port <port>, --workspace <path>, --session-root <path>, --agents <json-or-path>, --auth-token <token>"
}

pub(super) fn client_help() -> &'static str {
    "Usage: openagent client [message..] [options]\n\n\
     Options: --server-url <url>, --server-token <token>, --workspace <path>, -s/--session <id>, -c/--continue, -f/--file <path>, --command <name>, --format <text|json>"
}

pub(super) fn attach_help() -> &'static str {
    "Usage: openagent attach <url> [options]\n\n\
     Options: --workspace <path>, -s/--session <id>, -c/--continue, --fork, --format <text|json>, --skip-health-check, --server-token <token>, --server-token-env <name>, -u/--username <name>, -p/--password <password>\n\
     Interactive commands: /sessions, /tasks, /task <id>, /resume <id>, /new, /fork, /interrupt [turn_id], /exit"
}

pub(super) fn terminal_help() -> &'static str {
    "Usage: openagent terminal [options] -- <command...>\n\n\
     Options: --server-url <url>, --attach <url>, --server-token <token>, --server-token-env <name>, -u/--username <name>, -p/--password <password>, --cwd <path>, --timeout-ms <n>, --command <text>, --format <text|json>\n\
     Runs through the Bridge API /api/terminal/run endpoint and is scoped to the server workspace."
}

pub(super) fn doctor_help() -> &'static str {
    "Usage: openagent doctor [options]\n\n\
     Options: --format <text|json>, --base-url <url>, --model <id>, --wire-api <chat|responses>, --api-key <key>"
}

pub(super) fn models_help() -> &'static str {
    "Usage: openagent models [provider] [options]\n\n\
     Options: --format <table|json>, --refresh, --offline, --catalog, --verbose, --ttl-seconds <n>, --models-url <url>"
}

pub(super) fn session_help() -> &'static str {
    "Usage: openagent session <list|export|import|share|checkpoints|restore|delete> [options]\n\n\
     list:   --session-root <path>, --format <table|json>, --max-count <n>\n\
     export: --session-root <path>, --sanitize [session_id]\n\
     import: --session-root <path> <file-or-url>\n\
     share:  --session-root <path> [session_id]\n\
     checkpoints: --session-root <path>, --format <table|json> [session_id]\n\
     restore: --session-root <path> <session_id> <checkpoint_id>\n\
     delete: --session-root <path> <session_id>"
}

pub(super) fn stats_help() -> &'static str {
    "Usage: openagent stats [options]\n\n\
     Options: --session-root <path>, --days <n>, --format <table|json>"
}

pub(super) fn command_help() -> &'static str {
    "Usage: openagent command <list|show|render> [options]\n\n\
     Options: --workspace <path>, --command-dir <path>, --format <table|json|text>"
}

pub(super) fn config_help() -> &'static str {
    "Usage: openagent config <init|show> [options]\n\n\
     init: --workspace <path>, --path <file>, --api-key <key>, --base-url <url>, --model <id>, --wire-api <chat|responses>, --max-steps <n>, --with-server-token, --force, --format <text|json>\n\
     show: --workspace <path>, --session-root <path>, --server-url <url>, --format <table|json>"
}

pub(super) fn auth_help(command_name: &str) -> String {
    format!(
        "Usage: openagent {command_name} <login|list|methods|logout> [options]\n\n\
         login: [provider-url] --provider <id> --api-key <key> --base-url <url> --model <id> --wire-api <chat|responses> --auth-file <file>\n\
         list: --auth-file <file> --format <table|json>\n\
         methods: [provider] --format <table|json>\n\
         logout: --provider <id> --auth-file <file>"
    )
}

pub(super) fn mcp_help() -> &'static str {
    "Usage: openagent mcp <list|show|add|remove|auth|logout|doctor|debug|test|start|stop|restart|enable|disable> [options]\n\n\
     add remote: name --url <url> --transport <auto|http|sse> --header KEY=VALUE --timeout-ms <n> --disabled --mcp-config <file>\n\
     add local:  name --command <program> --arg <value> --env KEY=VALUE --cwd <dir> --timeout-ms <n> --disabled --mcp-config <file>\n\
     local one-shot test: test <name> --mcp-config <file> --workspace <dir>\n\
     Bridge lifecycle: list|show|doctor|test|start|stop|restart|enable|disable --server-url <url> --server-token <token>\n\
     auth: list|status|login|set-token|callback\n\
     doctor/debug: --refresh --format <table|json>"
}

pub(super) fn approval_help() -> &'static str {
    "Usage: openagent approval <list|respond|reject> [options]\n\n\
     Options: --session-root <path>, -s/--session <id>, --request-id <id>, --decision <allow_once|allow_always|reject>, --note <text>, --format <json|text>"
}

pub(super) fn question_help() -> &'static str {
    "Usage: openagent question <list|reply|reject> [options]\n\n\
     Options: --session-root <path>, -s/--session <id>, --request-id <id>, --answer <text>, --format <json|text>"
}
