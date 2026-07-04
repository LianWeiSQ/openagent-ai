use openagent_core::{
    SkillRegistry, SkillRegistryOptions, render_skill_document, skill_info_model_invocable,
};

use super::*;

pub(super) fn skills_command(args: &[String]) -> CliRunResult {
    if args.is_empty() || args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text(
            "Usage: openagent skills <list|show|doctor> [name] [--workspace <path>] [--root <path>] [--query <text>] [--limit <n>] [--format json]",
        );
    }
    match args[0].as_str() {
        "list" | "ls" => skills_list(args),
        "show" => skills_show(args),
        "doctor" => skills_doctor(args),
        _ => err_text(2, "skills command must be list, show, or doctor"),
    }
}

fn skills_registry(args: &[String]) -> SkillRegistry {
    let roots = values_for(args, &["--root", "--skill-root", "--skill-roots"]);
    SkillRegistry::new_with_options(
        Some(workspace_from_args(args)),
        (!roots.is_empty()).then_some(roots),
        Option::<PathBuf>::None,
        SkillRegistryOptions {
            include_builtin_skills: true,
        },
    )
}

fn skills_list(args: &[String]) -> CliRunResult {
    let registry = skills_registry(args);
    let query = value_for(args, &["--query", "-q"]);
    let limit = value_for(args, &["--limit"])
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0);
    let mut report = registry.report(query.as_deref(), limit);
    report.skills.retain(skill_info_model_invocable);
    CliRunResult::ok_json(&json!({
        "skills": report.skills,
        "query": query,
        "loaded_count": report.loaded_count,
        "scanned_files": report.scanned_files,
        "invalid_count": report.invalid_count,
        "duplicate_count": report.duplicate_count,
    }))
}

fn skills_show(args: &[String]) -> CliRunResult {
    let positionals = positional_args(
        args,
        &[
            "--workspace",
            "--root",
            "--skill-root",
            "--skill-roots",
            "--format",
        ],
    );
    let Some(name) = positionals.get(1).or_else(|| positionals.first()) else {
        return err_text(2, "skills show requires a name");
    };
    let registry = skills_registry(args);
    match registry.get(name) {
        Some(document) => CliRunResult::ok_json(&json!({
            "name": document.name,
            "description": document.description,
            "location": document.location,
            "directory": document.directory,
            "metadata": document.metadata,
            "content": document.content,
            "rendered": render_skill_document(&document, true),
        })),
        None => err_text(1, format!("skill not found: {name}")),
    }
}

fn skills_doctor(args: &[String]) -> CliRunResult {
    let registry = skills_registry(args);
    let report = registry.report(None, None);
    CliRunResult::ok_json(&json!({
        "loaded_count": report.loaded_count,
        "scanned_files": report.scanned_files,
        "invalid_count": report.invalid_count,
        "duplicate_count": report.duplicate_count,
        "skills": report.skills,
        "issues": report.issues,
    }))
}
