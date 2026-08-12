use openagent_eval::{FaultInjectionPlanV1, QualityGatePolicyV1, audit_fault_injection_plan};

#[test]
fn checked_in_fault_injection_plan_covers_every_release_critical_case() {
    let policy: QualityGatePolicyV1 =
        serde_json::from_str(include_str!("../policies/release-v1.json")).expect("release policy");
    let plan: FaultInjectionPlanV1 =
        serde_json::from_str(include_str!("../fault-injection-v1.json"))
            .expect("fault injection plan");
    let audit = audit_fault_injection_plan(&plan, &policy);
    assert!(audit.passed, "{:?}", audit.violations);
    assert!(audit.missing_critical_cases.is_empty());
    assert_eq!(audit.plan_fingerprint.len(), 64);
    assert_eq!(
        plan.scenarios.iter().filter(|case| case.critical).count(),
        6
    );
}
