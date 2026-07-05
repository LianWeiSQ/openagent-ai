use openagent_lsp::{
    LspQuery, lsp_doctor, lsp_status, operation_from_str, operation_requires_position,
    query_workspace,
};

use super::*;

pub(super) fn lsp_command(args: &[String]) -> CliRunResult {
    if args.is_empty() || args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text(
            "Usage: openagent lsp <status|doctor|query|operation> [file] [--workspace <path>] [--line <n>] [--character <n>] [--query <text>] [--timeout-ms <n>]\n\n\
             Operations: diagnostics, goToDefinition, findReferences, hover, documentSymbol, workspaceSymbol, goToImplementation, prepareCallHierarchy, incomingCalls, outgoingCalls",
        );
    }
    match args[0].as_str() {
        "status" | "list" | "ls" => lsp_status_command(args),
        "doctor" => lsp_doctor_command(args),
        "query" => lsp_query_command(&args[1..]),
        operation if operation_from_str(operation).is_some() => lsp_query_command(args),
        _ => err_text(
            2,
            "lsp command must be status, doctor, query, or an LSP operation",
        ),
    }
}

fn lsp_status_command(args: &[String]) -> CliRunResult {
    let workspace = workspace_from_args(args);
    match lsp_status(&workspace) {
        Ok(servers) => CliRunResult::ok_json(&json!({"servers": servers})),
        Err(error) => err_text(1, error),
    }
}

fn lsp_doctor_command(args: &[String]) -> CliRunResult {
    let workspace = workspace_from_args(args);
    match lsp_doctor(&workspace) {
        Ok(report) => CliRunResult::ok_json(&json!(report)),
        Err(error) => err_text(1, error),
    }
}

fn lsp_query_command(args: &[String]) -> CliRunResult {
    let positionals = positional_args(
        args,
        &[
            "--workspace",
            "--file",
            "--path",
            "--line",
            "--character",
            "--column",
            "--query",
            "--timeout-ms",
            "--timeout",
            "--format",
        ],
    );
    let Some(operation_name) = positionals.first() else {
        return err_text(2, "lsp query requires an operation");
    };
    let Some(operation) = operation_from_str(operation_name) else {
        return err_text(2, format!("unsupported LSP operation: {operation_name}"));
    };
    let file_arg = value_for(args, &["--file", "--path"])
        .or_else(|| positionals.get(1).cloned())
        .unwrap_or_default();
    if file_arg.is_empty() {
        return err_text(2, "lsp query requires a file path");
    }
    let line = match optional_u64_arg(args, &["--line"]) {
        Ok(value) => value,
        Err(error) => return err_text(2, error),
    };
    let character = match optional_u64_arg(args, &["--character", "--column"]) {
        Ok(value) => value,
        Err(error) => return err_text(2, error),
    };
    if operation_requires_position(&operation) && (line.is_none() || character.is_none()) {
        return err_text(
            2,
            "cursor-based LSP operations require --line and --character",
        );
    }
    let timeout_ms = match optional_u64_arg(args, &["--timeout-ms", "--timeout"]) {
        Ok(value) => value,
        Err(error) => return err_text(2, error),
    };
    let workspace = workspace_from_args(args);
    match query_workspace(
        &workspace,
        LspQuery {
            operation,
            file_path: PathBuf::from(file_arg),
            line,
            character,
            query: value_for(args, &["--query"]),
            timeout_ms,
        },
    ) {
        Ok(result) => CliRunResult::ok_json(&json!(result)),
        Err(error) => err_text(1, error),
    }
}

fn optional_u64_arg(args: &[String], names: &[&str]) -> Result<Option<u64>, String> {
    let Some(raw) = value_for(args, names) else {
        return Ok(None);
    };
    raw.parse::<u64>()
        .map(Some)
        .map_err(|error| format!("invalid integer '{raw}': {error}"))
}
