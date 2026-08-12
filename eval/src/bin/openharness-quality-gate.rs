use std::{env, fs, process::ExitCode};

use openagent_eval::{
    QualityGateDecisionStatus, QualityGateEvidenceV1, QualityGatePolicyV1, evaluate_quality_gate,
};

fn main() -> ExitCode {
    match run() {
        Ok(QualityGateDecisionStatus::Pass) => ExitCode::SUCCESS,
        Ok(QualityGateDecisionStatus::Fail) => ExitCode::from(2),
        Err(error) => {
            eprintln!("openharness-quality-gate: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<QualityGateDecisionStatus, String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err(
            "usage: openharness-quality-gate <policy.json> <evidence.json> <decision.json>"
                .to_string(),
        );
    }
    let policy: QualityGatePolicyV1 = read_json(&args[0])?;
    let evidence: QualityGateEvidenceV1 = read_json(&args[1])?;
    let decision = evaluate_quality_gate(&policy, &evidence);
    let serialized = serde_json::to_string_pretty(&decision)
        .map_err(|error| format!("serialize decision: {error}"))?;
    fs::write(&args[2], format!("{serialized}\n"))
        .map_err(|error| format!("write {}: {error}", args[2]))?;
    println!(
        "quality gate: {:?}; evidence={}; decision={}",
        decision.decision, decision.evidence_id, args[2]
    );
    Ok(decision.decision)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, String> {
    let raw = fs::read_to_string(path).map_err(|error| format!("read {path}: {error}"))?;
    serde_json::from_str(&raw).map_err(|error| format!("parse {path}: {error}"))
}
