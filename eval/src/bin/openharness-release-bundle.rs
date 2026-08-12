use std::{env, fs, path::Path, process::ExitCode};

use openagent_eval::{ReleaseReadinessRequestV1, assemble_release_readiness_manifest};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("openharness-release-bundle: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: openharness-release-bundle <request.json> <manifest.json>".to_string());
    }
    let raw = fs::read_to_string(&args[0]).map_err(|error| format!("read {}: {error}", args[0]))?;
    let request = serde_json::from_str::<ReleaseReadinessRequestV1>(&raw)
        .map_err(|error| format!("parse {}: {error}", args[0]))?;
    let base_dir = Path::new(&args[0])
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let manifest = assemble_release_readiness_manifest(&request, base_dir)?;
    let serialized = serde_json::to_string_pretty(&manifest)
        .map_err(|error| format!("serialize release manifest: {error}"))?;
    fs::write(&args[1], format!("{serialized}\n"))
        .map_err(|error| format!("write {}: {error}", args[1]))?;
    println!(
        "release readiness: passed={}; release={}; fingerprint={}; output={}",
        manifest.passed, manifest.release_id, manifest.manifest_fingerprint, args[1]
    );
    Ok(())
}
