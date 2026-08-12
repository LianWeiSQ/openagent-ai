use std::{env, fs, path::Path, process::ExitCode, time::SystemTime};

use openagent_eval::{
    LoadTestBaselineV1, LoadTestPlanV1, LoadTestReportV1, load_test_baseline_from_report,
    run_load_test, validate_load_test_baseline, validate_load_test_plan,
};

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(2),
        Err(error) => {
            eprintln!("openharness-load: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<bool, String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str) {
        Some("validate") if args.len() == 2 || args.len() == 3 => {
            let plan: LoadTestPlanV1 = read_json(Path::new(&args[1]))?;
            validate_load_test_plan(&plan)?;
            if let Some(path) = args.get(2) {
                let baseline: LoadTestBaselineV1 = read_json(Path::new(path))?;
                validate_load_test_baseline(&baseline)?;
            }
            println!("load plan valid: workload={}", plan.workload_id);
            Ok(true)
        }
        Some("run") if args.len() == 3 || args.len() == 4 => {
            let plan: LoadTestPlanV1 = read_json(Path::new(&args[1]))?;
            let baseline = args
                .get(3)
                .map(|path| read_json::<LoadTestBaselineV1>(Path::new(path)))
                .transpose()?;
            let report = run_load_test(&plan, baseline.as_ref())?;
            write_json(Path::new(&args[2]), &report)?;
            println!(
                "load test: passed={}; requests={}; success_rate={:.4}; p95={}ms; output={}",
                report.passed,
                report.metrics.total_requests,
                report.metrics.success_rate,
                report.metrics.p95_duration_ms,
                args[2]
            );
            Ok(report.passed)
        }
        Some("baseline") if args.len() == 4 => {
            let report: LoadTestReportV1 = read_json(Path::new(&args[1]))?;
            let baseline = load_test_baseline_from_report(&report, &args[2], now_ms()?)?;
            write_json(Path::new(&args[3]), &baseline)?;
            println!(
                "load baseline: id={}; fingerprint={}; output={}",
                baseline.baseline_id, baseline.content_fingerprint, args[3]
            );
            Ok(true)
        }
        _ => Err(
            "usage: openharness-load validate <plan.json> [baseline.json] | run <plan.json> <report.json> [baseline.json] | baseline <passing-report.json> <baseline-id> <baseline.json>"
                .to_string(),
        ),
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let raw =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&raw).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(value)
        .map_err(|error| format!("serialize {}: {error}", path.display()))?;
    fs::write(path, format!("{raw}\n"))
        .map_err(|error| format!("write {}: {error}", path.display()))
}

fn now_ms() -> Result<u64, String> {
    Ok(u64::try_from(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_millis(),
    )
    .unwrap_or(u64::MAX))
}
