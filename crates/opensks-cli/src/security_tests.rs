use super::*;

fn temp_workspace(label: &str) -> PathBuf {
    let stamp = ClockStamp::now().expect("clock").compact_id();
    let root = std::env::temp_dir().join(format!("{label}-{stamp}"));
    fs::create_dir_all(&root).expect("temp workspace");
    root
}

fn security_json(args: &[&str], workspace: &Path) -> serde_json::Value {
    let owned: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let output = run_security_command(&owned, workspace).expect("security command ok");
    assert!(output.stdout.ends_with('\n'));
    serde_json::from_str(&output.stdout).expect("valid security json")
}

#[test]
fn security_report_emits_schema_checks_and_summary() {
    let root = temp_workspace("opensks-security-report");
    let report = security_json(
        &[
            "report",
            "--workspace",
            &root.to_string_lossy(),
            "--generated-at",
            "2026-06-22T00:00:00Z",
        ],
        &root,
    );
    assert_eq!(report["schema"], "opensks.security-report.v1");
    assert_eq!(report["generated_at"], "2026-06-22T00:00:00Z");

    let checks = report["checks"].as_array().expect("checks array");
    let names: Vec<&str> = checks
        .iter()
        .map(|c| c["name"].as_str().expect("check name"))
        .collect();
    for expected in [
        "redaction_enabled",
        "capabilities_deny_by_default",
        "approval_required_for_git_push",
        "reconnect_replay_supported",
        "dependency_advisories_scanned",
    ] {
        assert!(names.contains(&expected), "missing check {expected}");
    }
    assert!(checks.iter().all(|c| c["passed"] == true));

    let findings = report["findings"].as_array().expect("findings array");
    assert!(
        findings
            .iter()
            .any(|f| { f["id"] == "dependency-advisory-posture" && f["status"] == "accepted" })
    );
    assert!(report["summary"]["critical"].as_u64().is_some());
    fs::remove_dir_all(&root).ok();
}

#[test]
fn security_audit_passes_when_no_open_blocking_finding() {
    let root = temp_workspace("opensks-security-audit-pass");
    let report = security_json(&["audit", "--workspace", &root.to_string_lossy()], &root);
    assert_eq!(report["schema"], "opensks.security-report.v1");
    assert_eq!(report["summary"]["critical"], 0);
    assert_eq!(report["summary"]["high"], 0);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn security_report_folds_in_precomputed_findings() {
    let root = temp_workspace("opensks-security-findings");
    let findings_path = root.join("findings.json");
    fs::write(
        &findings_path,
        r#"[
          {
            "id": "open-critical",
            "severity": "critical",
            "category": "secrets",
            "title": "Hardcoded credential",
            "detail": "redacted",
            "status": "open"
          }
        ]"#,
    )
    .expect("write findings");

    let report = security_json(
        &[
            "report",
            "--workspace",
            &root.to_string_lossy(),
            "--findings",
            &findings_path.to_string_lossy(),
        ],
        &root,
    );
    assert_eq!(report["summary"]["critical"], 1);

    let audit = security_json(&["audit", "--workspace", &root.to_string_lossy()], &root);
    assert_eq!(audit["summary"]["critical"], 0);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn security_audit_fails_nonzero_on_open_blocking_builtin_finding() {
    let report = opensks_contracts::SecurityReport::new(
        "t",
        vec![opensks_contracts::SecurityFinding {
            id: "x".to_string(),
            severity: opensks_contracts::SecuritySeverity::High,
            category: "c".to_string(),
            title: "t".to_string(),
            detail: "d".to_string(),
            status: opensks_contracts::FindingStatus::Open,
            owner: None,
            deadline: None,
        }],
        vec![],
    );
    assert!(report.has_open_blocking_finding());
}

#[test]
fn security_unknown_subcommand_is_usage_error() {
    let root = temp_workspace("opensks-security-usage");
    let err = run_security_command(
        &[
            "bogus".to_string(),
            "--workspace".to_string(),
            root.to_string_lossy().into_owned(),
        ],
        &root,
    )
    .expect_err("unknown subcommand must error");
    assert!(matches!(err, CliError::Usage(_)));
    fs::remove_dir_all(&root).ok();
}
