use std::{env, fs, path::Path, process::ExitCode};

use openagent_eval::{
    FaultInjectionPlanV1, QualityGatePolicyV1, audit_fault_injection_plan,
    execute_critical_fault_injection_plan,
};

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(2),
        Err(error) => {
            eprintln!("openharness-fault-plan: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<bool, String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 && args.len() != 4 {
        return Err(
            "usage: openharness-fault-plan <plan.json> <policy.json> <report.json> [--execute-critical]"
                .to_string(),
        );
    }
    let execute = args.get(3).is_some_and(|arg| arg == "--execute-critical");
    if args.len() == 4 && !execute {
        return Err("the only supported execution flag is --execute-critical".to_string());
    }
    let plan: FaultInjectionPlanV1 = read_json(&args[0])?;
    let policy: QualityGatePolicyV1 = read_json(&args[1])?;
    let workspace_root = find_workspace_root(Path::new(&args[0]))?;

    if execute {
        let report = execute_critical_fault_injection_plan(&plan, &policy, &workspace_root);
        write_json(&args[2], &report)?;
        println!(
            "fault injection: passed={}; cases={}; output={}",
            report.passed,
            report.cases.len(),
            args[2]
        );
        Ok(report.passed)
    } else {
        let audit = audit_fault_injection_plan(&plan, &policy);
        write_json(&args[2], &audit)?;
        println!(
            "fault plan audit: passed={}; scenarios={}; output={}",
            audit.passed,
            plan.scenarios.len(),
            args[2]
        );
        Ok(audit.passed)
    }
}

fn find_workspace_root(plan_path: &Path) -> Result<std::path::PathBuf, String> {
    let canonical = fs::canonicalize(plan_path)
        .map_err(|error| format!("resolve {}: {error}", plan_path.display()))?;
    canonical
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("cannot derive workspace root from {}", plan_path.display()))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, String> {
    let raw = fs::read_to_string(path).map_err(|error| format!("read {path}: {error}"))?;
    serde_json::from_str(&raw).map_err(|error| format!("parse {path}: {error}"))
}

fn write_json<T: serde::Serialize>(path: &str, value: &T) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(value)
        .map_err(|error| format!("serialize {}: {error}", path))?;
    fs::write(path, format!("{raw}\n")).map_err(|error| format!("write {path}: {error}"))
}
