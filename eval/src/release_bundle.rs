use std::{collections::BTreeMap, fs, path::Path, time::SystemTime};

use openagent_telemetry::{VersionIdentity, canonical_json_fingerprint};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    FAULT_INJECTION_EXECUTION_SCHEMA_VERSION, FaultInjectionExecutionReportV1,
    LOAD_TEST_REPORT_SCHEMA_VERSION, LoadTestReportV1, QualityGateDecisionStatus,
    QualityGateDecisionV1, QualityGateEvidenceV1, QualityGatePolicyV1,
    REGRESSION_REPLAY_REPORT_SCHEMA_VERSION, RegressionReplayReportV1, evaluate_quality_gate,
};

pub const RELEASE_READINESS_REQUEST_SCHEMA_VERSION: &str =
    "openharness.release_readiness.request.v1";
pub const RELEASE_READINESS_MANIFEST_SCHEMA_VERSION: &str =
    "openharness.release_readiness.manifest.v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseReadinessTier {
    Ci,
    Production,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseReadinessRequestV1 {
    pub schema_version: String,
    pub release_id: String,
    pub tier: ReleaseReadinessTier,
    pub source_revision: String,
    pub image_digest: Option<String>,
    pub quality_policy_path: String,
    pub quality_evidence_path: String,
    pub quality_decision_path: String,
    pub fault_report_path: String,
    pub regression_report_path: String,
    pub load_report_path: String,
    pub soak_report_path: Option<String>,
    pub observability_asset_paths: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseArtifactEvidenceV1 {
    pub kind: String,
    pub schema_version: String,
    pub path: String,
    pub content_fingerprint: String,
    pub passed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseReadinessManifestV1 {
    pub schema_version: String,
    pub release_id: String,
    pub tier: ReleaseReadinessTier,
    pub generated_at_ms: u64,
    pub source_revision: String,
    pub image_digest: Option<String>,
    pub candidate_id: String,
    pub versions: VersionIdentity,
    pub artifacts: Vec<ReleaseArtifactEvidenceV1>,
    pub observability_asset_fingerprints: BTreeMap<String, String>,
    pub manifest_fingerprint: String,
    pub passed: bool,
}

pub fn assemble_release_readiness_manifest(
    request: &ReleaseReadinessRequestV1,
    base_dir: &Path,
) -> Result<ReleaseReadinessManifestV1, String> {
    validate_request(request)?;
    let policy_value = read_value(&resolve(base_dir, &request.quality_policy_path))?;
    let policy: QualityGatePolicyV1 = deserialize(&policy_value, "quality policy")?;
    let evidence_value = read_value(&resolve(base_dir, &request.quality_evidence_path))?;
    let evidence: QualityGateEvidenceV1 = deserialize(&evidence_value, "quality evidence")?;
    let decision_value = read_value(&resolve(base_dir, &request.quality_decision_path))?;
    let decision: QualityGateDecisionV1 = deserialize(&decision_value, "quality decision")?;
    let expected_decision = evaluate_quality_gate(&policy, &evidence);
    if decision != expected_decision || decision.decision != QualityGateDecisionStatus::Pass {
        return Err(
            "quality decision is not a reproducible pass for supplied evidence".to_string(),
        );
    }

    let fault_value = read_value(&resolve(base_dir, &request.fault_report_path))?;
    let fault: FaultInjectionExecutionReportV1 = deserialize(&fault_value, "fault report")?;
    if fault.schema_version != FAULT_INJECTION_EXECUTION_SCHEMA_VERSION || !fault.passed {
        return Err("critical fault-injection report did not pass".to_string());
    }
    let regression_value = read_value(&resolve(base_dir, &request.regression_report_path))?;
    let regression: RegressionReplayReportV1 = deserialize(&regression_value, "regression report")?;
    if regression.schema_version != REGRESSION_REPLAY_REPORT_SCHEMA_VERSION || !regression.passed {
        return Err("regression replay report did not pass".to_string());
    }
    let load_value = read_value(&resolve(base_dir, &request.load_report_path))?;
    let load: LoadTestReportV1 = deserialize(&load_value, "load report")?;
    if load.schema_version != LOAD_TEST_REPORT_SCHEMA_VERSION || !load.passed {
        return Err("load-test report did not pass".to_string());
    }

    let mut artifacts = vec![
        artifact(
            "quality_policy",
            &request.quality_policy_path,
            &policy_value,
            true,
        ),
        artifact(
            "quality_evidence",
            &request.quality_evidence_path,
            &evidence_value,
            true,
        ),
        artifact(
            "quality_decision",
            &request.quality_decision_path,
            &decision_value,
            true,
        ),
        artifact(
            "fault_injection",
            &request.fault_report_path,
            &fault_value,
            true,
        ),
        artifact(
            "regression_replay",
            &request.regression_report_path,
            &regression_value,
            true,
        ),
        artifact("load_test", &request.load_report_path, &load_value, true),
    ];
    if let Some(path) = &request.soak_report_path {
        let soak_value = read_value(&resolve(base_dir, path))?;
        let soak: LoadTestReportV1 = deserialize(&soak_value, "soak report")?;
        if soak.schema_version != LOAD_TEST_REPORT_SCHEMA_VERSION || !soak.passed {
            return Err("soak report did not pass".to_string());
        }
        if soak.workload_fingerprint == load.workload_fingerprint {
            return Err(
                "production soak must be distinct from the load smoke workload".to_string(),
            );
        }
        artifacts.push(artifact("soak_test", path, &soak_value, true));
    }

    let mut observability_asset_fingerprints = BTreeMap::new();
    for asset_path in &request.observability_asset_paths {
        if observability_asset_fingerprints.contains_key(asset_path) {
            return Err(format!("duplicate observability asset path: {asset_path}"));
        }
        let bytes = fs::read(resolve(base_dir, asset_path))
            .map_err(|error| format!("read observability asset {asset_path}: {error}"))?;
        let fingerprint = canonical_json_fingerprint(&json!({"bytes": bytes}));
        observability_asset_fingerprints.insert(asset_path.clone(), fingerprint);
    }
    let mut manifest = ReleaseReadinessManifestV1 {
        schema_version: RELEASE_READINESS_MANIFEST_SCHEMA_VERSION.to_string(),
        release_id: request.release_id.clone(),
        tier: request.tier.clone(),
        generated_at_ms: now_ms(),
        source_revision: request.source_revision.clone(),
        image_digest: request.image_digest.clone(),
        candidate_id: evidence.subject.candidate_id,
        versions: evidence.subject.versions,
        artifacts,
        observability_asset_fingerprints,
        manifest_fingerprint: String::new(),
        passed: true,
    };
    manifest.manifest_fingerprint = release_readiness_manifest_fingerprint(&manifest);
    Ok(manifest)
}

#[must_use]
pub fn release_readiness_manifest_fingerprint(manifest: &ReleaseReadinessManifestV1) -> String {
    canonical_json_fingerprint(&json!({
        "schema_version": manifest.schema_version,
        "release_id": manifest.release_id,
        "tier": manifest.tier,
        "generated_at_ms": manifest.generated_at_ms,
        "source_revision": manifest.source_revision,
        "image_digest": manifest.image_digest,
        "candidate_id": manifest.candidate_id,
        "versions": manifest.versions,
        "artifacts": manifest.artifacts,
        "observability_asset_fingerprints": manifest.observability_asset_fingerprints,
        "passed": manifest.passed,
    }))
}

fn validate_request(request: &ReleaseReadinessRequestV1) -> Result<(), String> {
    if request.schema_version != RELEASE_READINESS_REQUEST_SCHEMA_VERSION {
        return Err(format!(
            "unsupported release-readiness request schema_version: {}",
            request.schema_version
        ));
    }
    validate_id(&request.release_id, "release_id")?;
    if request.source_revision.trim().is_empty() || request.source_revision.len() > 160 {
        return Err("source_revision must be non-empty and bounded".to_string());
    }
    if request.observability_asset_paths.is_empty() {
        return Err("release readiness requires observability asset fingerprints".to_string());
    }
    if request.tier == ReleaseReadinessTier::Production {
        if request.soak_report_path.is_none() {
            return Err("production readiness requires a passing soak report".to_string());
        }
        if !request
            .image_digest
            .as_deref()
            .is_some_and(valid_sha256_digest)
        {
            return Err("production readiness requires a sha256 image digest".to_string());
        }
    }
    Ok(())
}

fn artifact(kind: &str, path: &str, value: &Value, passed: bool) -> ReleaseArtifactEvidenceV1 {
    ReleaseArtifactEvidenceV1 {
        kind: kind.to_string(),
        schema_version: value
            .get("schema_version")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        path: path.to_string(),
        content_fingerprint: canonical_json_fingerprint(value),
        passed,
    }
}

fn resolve(base_dir: &Path, path: &str) -> std::path::PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn read_value(path: &Path) -> Result<Value, String> {
    let raw =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&raw).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn deserialize<T: serde::de::DeserializeOwned>(value: &Value, name: &str) -> Result<T, String> {
    serde_json::from_value(value.clone()).map_err(|error| format!("invalid {name}: {error}"))
}

fn validate_id(value: &str, name: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 160
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(format!("invalid {name}"));
    }
    Ok(())
}

fn valid_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}
