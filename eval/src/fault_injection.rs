use std::{
    collections::BTreeSet,
    path::Path,
    process::Command,
    time::{Instant, SystemTime},
};

use openagent_telemetry::canonical_json_fingerprint;
use serde::{Deserialize, Serialize};

use crate::QualityGatePolicyV1;

pub const FAULT_INJECTION_PLAN_SCHEMA_VERSION: &str = "openharness.fault_injection.plan.v1";
pub const FAULT_INJECTION_AUDIT_SCHEMA_VERSION: &str = "openharness.fault_injection.audit.v1";
pub const FAULT_INJECTION_EXECUTION_SCHEMA_VERSION: &str =
    "openharness.fault_injection.execution.v1";

const MAX_CAPTURED_OUTPUT_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaultInjectionScenarioV1 {
    pub id: String,
    pub layer: String,
    pub injection: String,
    pub expected: String,
    pub test_command: String,
    pub critical: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaultInjectionPlanV1 {
    pub schema_version: String,
    pub plan_id: String,
    pub scenarios: Vec<FaultInjectionScenarioV1>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaultInjectionAuditV1 {
    pub schema_version: String,
    pub plan_id: String,
    pub plan_fingerprint: String,
    pub passed: bool,
    pub missing_critical_cases: Vec<String>,
    pub violations: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultInjectionExecutionStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaultInjectionCaseResultV1 {
    pub scenario_id: String,
    pub test_command: String,
    pub status: FaultInjectionExecutionStatus,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub output_truncated: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaultInjectionExecutionReportV1 {
    pub schema_version: String,
    pub plan_id: String,
    pub plan_fingerprint: String,
    pub generated_at_ms: u64,
    pub executed_critical_only: bool,
    pub audit: FaultInjectionAuditV1,
    pub cases: Vec<FaultInjectionCaseResultV1>,
    pub passed: bool,
}

#[must_use]
pub fn audit_fault_injection_plan(
    plan: &FaultInjectionPlanV1,
    policy: &QualityGatePolicyV1,
) -> FaultInjectionAuditV1 {
    let mut violations = Vec::new();
    if plan.schema_version != FAULT_INJECTION_PLAN_SCHEMA_VERSION {
        violations.push(format!(
            "unsupported fault injection schema_version: {}",
            plan.schema_version
        ));
    }
    if plan.plan_id.trim().is_empty() {
        violations.push("fault injection plan_id must not be empty".to_string());
    }
    let mut ids = BTreeSet::new();
    for scenario in &plan.scenarios {
        if scenario.id.trim().is_empty()
            || scenario.layer.trim().is_empty()
            || scenario.injection.trim().is_empty()
            || scenario.expected.trim().is_empty()
        {
            violations.push(format!(
                "fault injection scenario {} has empty required fields",
                scenario.id
            ));
        }
        if !ids.insert(scenario.id.clone()) {
            violations.push(format!(
                "duplicate fault injection scenario: {}",
                scenario.id
            ));
        }
        if !scenario.test_command.starts_with("cargo test ")
            || !scenario.test_command.ends_with("-- --exact")
        {
            violations.push(format!(
                "fault injection scenario {} must use a deterministic exact cargo test command",
                scenario.id
            ));
        }
    }
    let critical_ids = plan
        .scenarios
        .iter()
        .filter(|scenario| scenario.critical)
        .map(|scenario| scenario.id.clone())
        .collect::<BTreeSet<_>>();
    let missing_critical_cases = policy
        .required_critical_cases
        .difference(&critical_ids)
        .cloned()
        .collect::<Vec<_>>();
    if !missing_critical_cases.is_empty() {
        violations.push(format!(
            "fault injection plan is missing {} release-critical scenarios",
            missing_critical_cases.len()
        ));
    }
    violations.sort();
    violations.dedup();
    let plan_fingerprint = serde_json::to_value(plan)
        .map(|value| canonical_json_fingerprint(&value))
        .unwrap_or_else(|_| canonical_json_fingerprint(&serde_json::Value::Null));
    FaultInjectionAuditV1 {
        schema_version: FAULT_INJECTION_AUDIT_SCHEMA_VERSION.to_string(),
        plan_id: plan.plan_id.clone(),
        plan_fingerprint,
        passed: violations.is_empty(),
        missing_critical_cases,
        violations,
    }
}

/// Execute the release-critical scenarios using argv-only process spawning.
///
/// The plan is audited before execution. An invalid plan is represented as a
/// failed report and no child process is started. Commands never pass through a
/// shell, and must match the deterministic `cargo test ... -- --exact` contract
/// enforced by [`audit_fault_injection_plan`].
pub fn execute_critical_fault_injection_plan(
    plan: &FaultInjectionPlanV1,
    policy: &QualityGatePolicyV1,
    workspace_root: &Path,
) -> FaultInjectionExecutionReportV1 {
    let audit = audit_fault_injection_plan(plan, policy);
    let generated_at_ms = now_ms();
    if !audit.passed {
        return FaultInjectionExecutionReportV1 {
            schema_version: FAULT_INJECTION_EXECUTION_SCHEMA_VERSION.to_string(),
            plan_id: plan.plan_id.clone(),
            plan_fingerprint: audit.plan_fingerprint.clone(),
            generated_at_ms,
            executed_critical_only: true,
            audit,
            cases: Vec::new(),
            passed: false,
        };
    }

    let cases = plan
        .scenarios
        .iter()
        .filter(|scenario| scenario.critical)
        .map(|scenario| execute_scenario(scenario, workspace_root))
        .collect::<Vec<_>>();
    let passed = !cases.is_empty()
        && cases
            .iter()
            .all(|case| case.status == FaultInjectionExecutionStatus::Passed);
    FaultInjectionExecutionReportV1 {
        schema_version: FAULT_INJECTION_EXECUTION_SCHEMA_VERSION.to_string(),
        plan_id: plan.plan_id.clone(),
        plan_fingerprint: audit.plan_fingerprint.clone(),
        generated_at_ms,
        executed_critical_only: true,
        audit,
        cases,
        passed,
    }
}

fn execute_scenario(
    scenario: &FaultInjectionScenarioV1,
    workspace_root: &Path,
) -> FaultInjectionCaseResultV1 {
    let Some((program, args)) = deterministic_command_argv(&scenario.test_command) else {
        return FaultInjectionCaseResultV1 {
            scenario_id: scenario.id.clone(),
            test_command: scenario.test_command.clone(),
            status: FaultInjectionExecutionStatus::Skipped,
            exit_code: None,
            duration_ms: 0,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            output_truncated: false,
            error: Some("command failed deterministic argv validation".to_string()),
        };
    };

    let started = Instant::now();
    match Command::new(program)
        .args(args)
        .current_dir(workspace_root)
        .output()
    {
        Ok(output) => {
            let (stdout_tail, stdout_truncated) = output_tail(&output.stdout);
            let (stderr_tail, stderr_truncated) = output_tail(&output.stderr);
            FaultInjectionCaseResultV1 {
                scenario_id: scenario.id.clone(),
                test_command: scenario.test_command.clone(),
                status: if output.status.success() {
                    FaultInjectionExecutionStatus::Passed
                } else {
                    FaultInjectionExecutionStatus::Failed
                },
                exit_code: output.status.code(),
                duration_ms: elapsed_ms(started),
                stdout_tail,
                stderr_tail,
                output_truncated: stdout_truncated || stderr_truncated,
                error: None,
            }
        }
        Err(error) => FaultInjectionCaseResultV1 {
            scenario_id: scenario.id.clone(),
            test_command: scenario.test_command.clone(),
            status: FaultInjectionExecutionStatus::Failed,
            exit_code: None,
            duration_ms: elapsed_ms(started),
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            output_truncated: false,
            error: Some(error.to_string()),
        },
    }
}

fn deterministic_command_argv(command: &str) -> Option<(&str, Vec<&str>)> {
    let argv = command.split_ascii_whitespace().collect::<Vec<_>>();
    if argv.len() < 6
        || argv.first().copied() != Some("cargo")
        || argv.get(1).copied() != Some("test")
        || argv.get(argv.len().saturating_sub(2)).copied() != Some("--")
        || argv.last().copied() != Some("--exact")
        || argv.iter().any(|token| {
            token.contains(';')
                || token.contains('|')
                || token.contains('&')
                || token.contains('`')
                || token.contains("$(")
        })
    {
        return None;
    }
    Some((argv[0], argv[1..].to_vec()))
}

fn output_tail(bytes: &[u8]) -> (String, bool) {
    let truncated = bytes.len() > MAX_CAPTURED_OUTPUT_BYTES;
    let start = bytes.len().saturating_sub(MAX_CAPTURED_OUTPUT_BYTES);
    (
        String::from_utf8_lossy(&bytes[start..]).into_owned(),
        truncated,
    )
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{deterministic_command_argv, output_tail};

    #[test]
    fn deterministic_fault_commands_are_argv_only() {
        let (program, args) =
            deterministic_command_argv("cargo test -p example --test suite exact_case -- --exact")
                .expect("valid command");
        assert_eq!(program, "cargo");
        assert_eq!(args.last().copied(), Some("--exact"));
        assert!(
            deterministic_command_argv(
                "cargo test -p example --test suite exact_case; touch /tmp/oops -- --exact"
            )
            .is_none()
        );
    }

    #[test]
    fn captured_output_is_bounded() {
        let input = vec![b'x'; super::MAX_CAPTURED_OUTPUT_BYTES + 10];
        let (tail, truncated) = output_tail(&input);
        assert!(truncated);
        assert_eq!(tail.len(), super::MAX_CAPTURED_OUTPUT_BYTES);
    }
}
