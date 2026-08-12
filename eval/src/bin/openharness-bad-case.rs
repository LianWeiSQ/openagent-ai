use std::{env, fs, process::ExitCode, time::SystemTime};

use openagent_eval::{
    BadCaseArtifactV1, BadCaseCaptureInputV1, BadCaseState, capture_bad_case,
    promote_bad_case_to_fixture, transition_bad_case, validate_bad_case_artifact,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("openharness-bad-case: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str) {
        Some("capture") if args.len() == 3 => {
            let input: BadCaseCaptureInputV1 = read_json(&args[1])?;
            write_json(&args[2], &capture_bad_case(input)?)
        }
        Some("validate") if args.len() == 2 => {
            let artifact: BadCaseArtifactV1 = read_json(&args[1])?;
            validate_bad_case_artifact(&artifact)
        }
        Some("transition") if args.len() >= 5 => {
            let artifact: BadCaseArtifactV1 = read_json(&args[1])?;
            let next = parse_state(&args[2])?;
            let note = args.get(5).map(String::as_str).unwrap_or_default();
            let updated = transition_bad_case(&artifact, next, &args[3], note, now_ms()?)?;
            write_json(&args[4], &updated)
        }
        Some("promote") if args.len() == 7 => {
            let artifact: BadCaseArtifactV1 = read_json(&args[1])?;
            let (promoted, fixture) = promote_bad_case_to_fixture(
                &artifact,
                &args[2],
                &args[3],
                &args[4],
                now_ms()?,
            )?;
            write_json(&args[5], &promoted)?;
            write_json(&args[6], &fixture)
        }
        _ => Err(
            "usage: openharness-bad-case capture <capture.json> <record.json> | validate <record.json> | transition <record.json> <state> <owner> <output.json> [note] | promote <record.json> <fixture-id> <dataset-version> <owner> <updated-record.json> <fixture.json>"
                .to_string(),
        ),
    }
}

fn parse_state(raw: &str) -> Result<BadCaseState, String> {
    serde_json::from_str(&format!("\"{raw}\""))
        .map_err(|_| format!("invalid bad case state: {raw}"))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, String> {
    let raw = fs::read_to_string(path).map_err(|error| format!("read {path}: {error}"))?;
    serde_json::from_str(&raw).map_err(|error| format!("parse {path}: {error}"))
}

fn write_json<T: serde::Serialize>(path: &str, value: &T) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(value)
        .map_err(|error| format!("serialize {path}: {error}"))?;
    fs::write(path, format!("{raw}\n")).map_err(|error| format!("write {path}: {error}"))
}

fn now_ms() -> Result<u64, String> {
    let millis = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_millis();
    Ok(u64::try_from(millis).unwrap_or(u64::MAX))
}
