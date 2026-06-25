use super::run_capability_command;

#[test]
fn capability_report_emits_valid_json_and_matrix() {
    let cwd = std::env::temp_dir();
    let out = run_capability_command(&["report".to_string()], &cwd).expect("report");
    let report: opensks_contracts::RuntimeCapabilityReport =
        serde_json::from_str(&out.stdout).expect("valid json capability report");
    report.validate().expect("report internally valid");
    assert!(
        report.generated_for.as_deref().is_some_and(|value| {
            value.contains("workspace:") && value.contains("fixture:local")
        }),
        "runtime report must identify the current workspace/build fixture"
    );
    assert!(
        report
            .capabilities
            .iter()
            .any(|c| c.id == "agent.local_test_edit"),
        "report must include known capabilities"
    );
    assert!(
        report
            .tool_registry
            .descriptor("mcp.invoke")
            .is_some_and(|tool| {
                tool.availability == opensks_contracts::ToolAvailability::Available
                    && tool.reason_code == "local_mcp_broker_executable"
            }),
        "runtime report must expose available MCP tool truth"
    );
    assert!(
        report
            .tool_registry
            .descriptor("image.generate")
            .is_some_and(|tool| {
                tool.availability == opensks_contracts::ToolAvailability::Available
                    && tool.reason_code == "provider_image_executor_route_required"
            }),
        "runtime report must expose provider-backed image tool truth"
    );
    assert!(
        report
            .tool_registry
            .descriptor("image.inspect")
            .is_some_and(|tool| {
                tool.availability == opensks_contracts::ToolAvailability::Available
                    && tool.reason_code == "provider_vision_executor_route_required"
            }),
        "runtime report must expose provider-backed vision tool truth"
    );
    let local_test = report
        .capabilities
        .iter()
        .find(|c| c.id == "agent.local_test_edit")
        .expect("agent.local_test_edit");
    if cfg!(feature = "simulation") {
        assert!(local_test.available);
        assert_eq!(
            local_test.reason_code,
            "explicit_local_test_adapter_real_file_io"
        );
    } else {
        assert!(!local_test.available);
        assert_eq!(
            local_test.reason_code,
            "simulation_feature_disabled_for_release_build"
        );
    }
    let code_edit = report
        .capabilities
        .iter()
        .find(|c| c.id == "agent.code_edit")
        .expect("agent.code_edit");
    assert_eq!(
        code_edit.reason_code,
        "agentic_loop_toolgateway_patch_engine_need_live_provider_credentials"
    );
    assert!(
        code_edit
            .evidence_refs
            .iter()
            .any(|e| e == "toolgateway:policy-enforced"),
        "agent.code_edit evidence must come from runtime ToolGateway state"
    );
    let stream = report
        .capabilities
        .iter()
        .find(|c| c.id == "stream.protocol")
        .expect("stream.protocol");
    assert!(
        !stream.reason_code.contains("quiet_window"),
        "runtime capability truth must not preserve stale quiet-window reason"
    );
    let matrix = run_capability_command(&["matrix".to_string()], &cwd).expect("matrix");
    assert!(matrix.stdout.contains("Runtime Truth Matrix"));
    assert!(matrix.stdout.contains("runtime capability report"));
    assert!(matrix.stdout.contains("Tool Registry"));
    assert!(matrix.stdout.contains("| `skill.invoke` |"));
    assert!(run_capability_command(&["nope".to_string()], &cwd).is_err());
}
