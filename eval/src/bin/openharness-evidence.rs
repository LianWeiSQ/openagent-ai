use std::{env, fs, path::Path, process::ExitCode};

use openagent_eval::{ReleaseEvidenceAssemblyRequestV1, assemble_release_evidence};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("openharness-evidence: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: openharness-evidence <request.json> <evidence.json>".to_string());
    }
    let raw = fs::read_to_string(&args[0]).map_err(|error| format!("read {}: {error}", args[0]))?;
    let request = serde_json::from_str::<ReleaseEvidenceAssemblyRequestV1>(&raw)
        .map_err(|error| format!("parse {}: {error}", args[0]))?;
    let base_dir = Path::new(&args[0])
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let evidence = assemble_release_evidence(&request, base_dir)?;
    let serialized = serde_json::to_string_pretty(&evidence)
        .map_err(|error| format!("serialize evidence: {error}"))?;
    fs::write(&args[1], format!("{serialized}\n"))
        .map_err(|error| format!("write {}: {error}", args[1]))?;
    println!(
        "release evidence: cases={}; candidate={}; output={}",
        evidence.cases.len(),
        evidence.subject.candidate_id,
        args[1]
    );
    Ok(())
}
