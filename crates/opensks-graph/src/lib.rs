use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use opensks_contracts::{
    ApprovalPoint, ApprovalPolicy, COMPILED_PLAN_SCHEMA, CapabilityRequirements, CompileDiagnostic,
    CompiledPlan, CompletionContract, DependencyIndex, DiagnosticSeverity, EdgeKind, EdgeSpec,
    GraphMetadata, GraphPoint, GraphPolicies, NodeKind, NodeSpec, PIPELINE_GRAPH_SCHEMA,
    PipelineGraph, PortRef, PortType, ResourcePlan, RetryPolicy, WorkKind, WorkTemplate,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GraphError {
    #[error("graph has blocking diagnostics")]
    BlockingDiagnostics,
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn compile_graph(graph: &PipelineGraph) -> CompiledPlan {
    let mut diagnostics = validate_graph(graph);
    let graph_hash = graph_hash(graph);
    let work_templates = compile_work_templates(graph);
    let dependency_index = dependency_index(graph);
    let resource_plan = resource_plan(graph);
    let approval_points = approval_points(graph);
    let final_seal_node_ids: Vec<String> = graph
        .nodes
        .values()
        .filter(|node| node.enabled && node.kind == NodeKind::FinalSeal)
        .map(|node| node.id.clone())
        .collect();
    if graph.policies.final_seal_required && final_seal_node_ids.is_empty() {
        diagnostics.push(error(
            None,
            None,
            "missing_final_seal",
            "graph policy requires a FinalSeal node",
        ));
    }
    let mut plan = CompiledPlan {
        schema: COMPILED_PLAN_SCHEMA.to_string(),
        graph_id: graph.id.clone(),
        graph_version: graph.version,
        graph_hash,
        plan_hash: String::new(),
        work_templates,
        dependency_index,
        resource_plan,
        approval_points,
        proof_contract: CompletionContract {
            required_requirement_ids: Vec::new(),
            final_seal_node_ids,
            evidence_required: true,
        },
        diagnostics,
    };
    plan.plan_hash = stable_hash(
        serde_json::to_string(&plan)
            .unwrap_or_else(|_| "unserializable-plan".to_string())
            .as_bytes(),
    );
    plan
}

pub fn validate_graph(graph: &PipelineGraph) -> Vec<CompileDiagnostic> {
    let mut diagnostics = Vec::new();
    for (key, node) in &graph.nodes {
        if key != &node.id {
            diagnostics.push(error(
                Some(node.id.clone()),
                None,
                "node_key_id_mismatch",
                "node map key must match node id",
            ));
        }
        if node.enabled && node.kind == NodeKind::Loop {
            for field in ["max_iterations", "stop_condition", "no_progress_limit"] {
                if node.config.get(field).is_none() {
                    diagnostics.push(error(
                        Some(node.id.clone()),
                        None,
                        "loop_bound_missing",
                        "Loop nodes require max_iterations, stop_condition, and no_progress_limit",
                    ));
                    break;
                }
            }
        }
        if node.enabled && is_side_effect_node(&node.kind) && !node.approval.required {
            diagnostics.push(error(
                Some(node.id.clone()),
                None,
                "side_effect_requires_approval",
                "side-effect nodes require an explicit approval policy",
            ));
        }
    }

    for edge in &graph.edges {
        let from = graph.nodes.get(&edge.from.node_id);
        let to = graph.nodes.get(&edge.to.node_id);
        if from.is_none() || to.is_none() {
            diagnostics.push(error(
                None,
                Some(edge.id.clone()),
                "edge_endpoint_missing",
                "edge endpoint must reference existing nodes",
            ));
            continue;
        }
        let from = from.expect("from checked");
        let to = to.expect("to checked");
        if !from.enabled && required_input_port(&to.kind, &edge.to.port) {
            diagnostics.push(error(
                Some(to.id.clone()),
                Some(edge.id.clone()),
                "required_input_from_disabled_node",
                "disabled node output cannot feed a required input",
            ));
        }
        if let (Some(left), Some(right)) = (
            output_port_type(&from.kind, &edge.from.port),
            input_port_type(&to.kind, &edge.to.port),
        ) {
            if left != right && edge.kind != EdgeKind::Control {
                diagnostics.push(error(
                    Some(to.id.clone()),
                    Some(edge.id.clone()),
                    "port_type_mismatch",
                    "edge port types are incompatible",
                ));
            }
        }
    }

    if graph.policies.final_seal_required {
        let outgoing = outgoing_edges(graph);
        for node in graph.nodes.values().filter(|node| node.enabled) {
            if outgoing.get(&node.id).is_none_or(Vec::is_empty)
                && !matches!(
                    node.kind,
                    NodeKind::FinalSeal | NodeKind::Cancelled | NodeKind::Blocked
                )
            {
                diagnostics.push(error(
                    Some(node.id.clone()),
                    None,
                    "terminal_path_without_final_seal",
                    "terminal graph paths must end in FinalSeal, Cancelled, or Blocked",
                ));
            }
        }
    }

    diagnostics
}

pub fn default_templates() -> Vec<PipelineGraph> {
    vec![
        single_model_safe_template(),
        balanced_multi_model_template(),
        extreme_parallel_template(),
        image_heavy_product_build_template(),
        research_report_template(),
    ]
}

pub fn write_default_templates(workspace: &Path) -> Result<Vec<String>, GraphError> {
    let dir = workspace
        .join(".opensks")
        .join("pipelines")
        .join("templates");
    fs::create_dir_all(&dir)?;
    let mut written = Vec::new();
    for graph in default_templates() {
        let path = dir.join(format!("{}.graph.json", graph.id));
        fs::write(&path, serde_json::to_string_pretty(&graph)? + "\n")?;
        written.push(path.display().to_string());
    }
    Ok(written)
}

pub fn single_model_safe_template() -> PipelineGraph {
    let mut nodes = BTreeMap::new();
    nodes.insert(
        "goal".to_string(),
        node("goal", NodeKind::GoalInput, "Goal input", 0.0, 0.0),
    );
    nodes.insert(
        "delegate".to_string(),
        node(
            "delegate",
            NodeKind::Delegate,
            "Delegate to model",
            220.0,
            0.0,
        ),
    );
    nodes.insert(
        "seal".to_string(),
        node("seal", NodeKind::FinalSeal, "Final seal", 460.0, 0.0),
    );
    PipelineGraph {
        schema: PIPELINE_GRAPH_SCHEMA.to_string(),
        id: "single-model-safe".to_string(),
        name: "Single Model Safe".to_string(),
        version: 1,
        entry_nodes: vec!["goal".to_string()],
        nodes,
        edges: vec![
            control_edge("edge-goal-delegate", "goal", "out", "delegate", "in"),
            control_edge("edge-delegate-seal", "delegate", "out", "seal", "in"),
        ],
        variables: BTreeMap::new(),
        policies: GraphPolicies {
            max_parallelism: 1,
            allow_external_side_effects: false,
            final_seal_required: true,
        },
        metadata: GraphMetadata {
            description: "Default automatic safe pipeline with one compatible model.".to_string(),
            created_by: "opensks-graph".to_string(),
            evidence_refs: Vec::new(),
        },
    }
}

fn balanced_multi_model_template() -> PipelineGraph {
    let mut graph = single_model_safe_template();
    graph.id = "balanced-multi-model".to_string();
    graph.name = "Balanced Multi-Model".to_string();
    graph.policies.max_parallelism = 4;
    graph.nodes.insert(
        "verify".to_string(),
        node(
            "verify",
            NodeKind::VerifierPool,
            "Verifier pool",
            340.0,
            120.0,
        ),
    );
    graph.edges = vec![
        control_edge("edge-goal-delegate", "goal", "out", "delegate", "in"),
        control_edge("edge-delegate-verify", "delegate", "out", "verify", "in"),
        control_edge("edge-verify-seal", "verify", "out", "seal", "in"),
    ];
    graph
}

fn extreme_parallel_template() -> PipelineGraph {
    let mut graph = balanced_multi_model_template();
    graph.id = "extreme-parallel".to_string();
    graph.name = "Extreme Parallel".to_string();
    graph.policies.max_parallelism = 28;
    graph.nodes.insert(
        "workers".to_string(),
        node("workers", NodeKind::WorkerPool, "Worker pool", 220.0, 180.0),
    );
    graph.edges.push(control_edge(
        "edge-delegate-workers",
        "delegate",
        "out",
        "workers",
        "in",
    ));
    graph.edges.push(control_edge(
        "edge-workers-verify",
        "workers",
        "out",
        "verify",
        "in",
    ));
    graph
}

fn image_heavy_product_build_template() -> PipelineGraph {
    let mut graph = single_model_safe_template();
    graph.id = "image-heavy-product-build".to_string();
    graph.name = "Image-Heavy Product Build".to_string();
    graph.nodes.insert(
        "image".to_string(),
        node(
            "image",
            NodeKind::ImageGenerate,
            "Image generate",
            220.0,
            130.0,
        ),
    );
    graph.edges = vec![
        control_edge("edge-goal-image", "goal", "out", "image", "in"),
        control_edge("edge-image-seal", "image", "out", "seal", "in"),
    ];
    graph
}

fn research_report_template() -> PipelineGraph {
    let mut graph = single_model_safe_template();
    graph.id = "research-report".to_string();
    graph.name = "Research & Report".to_string();
    graph.nodes.insert(
        "research".to_string(),
        node(
            "research",
            NodeKind::WebResearch,
            "Web research",
            220.0,
            130.0,
        ),
    );
    if let Some(research) = graph.nodes.get_mut("research") {
        research.approval = ApprovalPolicy::required("external_network");
    }
    graph.edges = vec![
        control_edge("edge-goal-research", "goal", "out", "research", "in"),
        control_edge("edge-research-seal", "research", "out", "seal", "in"),
    ];
    graph
}

fn compile_work_templates(graph: &PipelineGraph) -> Vec<WorkTemplate> {
    graph
        .nodes
        .values()
        .filter(|node| node.enabled)
        .map(|node| WorkTemplate {
            id: format!("work-template-{}", node.id),
            node_id: node.id.clone(),
            kind: work_kind_for_node(&node.kind),
            dependencies: incoming_dependencies(graph, &node.id),
            capability_requirements: capabilities_for_node(&node.kind),
            requirement_ids: Vec::new(),
        })
        .collect()
}

fn dependency_index(graph: &PipelineGraph) -> DependencyIndex {
    let prerequisites = graph
        .nodes
        .keys()
        .map(|node_id| (node_id.clone(), incoming_dependencies(graph, node_id)))
        .collect();
    DependencyIndex { prerequisites }
}

fn incoming_dependencies(graph: &PipelineGraph, node_id: &str) -> Vec<String> {
    graph
        .edges
        .iter()
        .filter(|edge| edge.to.node_id == node_id)
        .map(|edge| edge.from.node_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn resource_plan(graph: &PipelineGraph) -> ResourcePlan {
    ResourcePlan {
        max_parallelism: graph.policies.max_parallelism.max(1),
        requires_git_worktree: graph
            .nodes
            .values()
            .any(|node| matches!(node.kind, NodeKind::GitWorktree | NodeKind::ApplyPatch)),
        requires_image: graph.nodes.values().any(|node| {
            matches!(
                node.kind,
                NodeKind::ImageGenerate
                    | NodeKind::ImageEdit
                    | NodeKind::ImageVariation
                    | NodeKind::VisualReview
            )
        }),
        requires_external_side_effect_approval: graph
            .nodes
            .values()
            .any(|node| is_side_effect_node(&node.kind)),
    }
}

fn approval_points(graph: &PipelineGraph) -> Vec<ApprovalPoint> {
    graph
        .nodes
        .values()
        .filter(|node| node.enabled && node.approval.required)
        .map(|node| ApprovalPoint {
            node_id: node.id.clone(),
            scope: node.approval.scope.clone(),
            reason_code: "explicit_node_approval".to_string(),
        })
        .collect()
}

fn outgoing_edges(graph: &PipelineGraph) -> BTreeMap<String, Vec<&EdgeSpec>> {
    let mut outgoing: BTreeMap<String, Vec<&EdgeSpec>> = BTreeMap::new();
    for edge in &graph.edges {
        outgoing
            .entry(edge.from.node_id.clone())
            .or_default()
            .push(edge);
    }
    outgoing
}

fn node(id: &str, kind: NodeKind, display_name: &str, x: f64, y: f64) -> NodeSpec {
    NodeSpec {
        id: id.to_string(),
        kind,
        display_name: display_name.to_string(),
        enabled: true,
        position: GraphPoint { x, y },
        inputs: BTreeMap::new(),
        config: serde_json::json!({}),
        retry: RetryPolicy::default(),
        timeout_ms: None,
        approval: ApprovalPolicy::none(),
        hook_refs: Vec::new(),
    }
}

fn control_edge(
    id: &str,
    from_node: &str,
    from_port: &str,
    to_node: &str,
    to_port: &str,
) -> EdgeSpec {
    EdgeSpec {
        id: id.to_string(),
        from: PortRef {
            node_id: from_node.to_string(),
            port: from_port.to_string(),
        },
        to: PortRef {
            node_id: to_node.to_string(),
            port: to_port.to_string(),
        },
        kind: EdgeKind::Control,
        condition: None,
    }
}

fn output_port_type(kind: &NodeKind, port: &str) -> Option<PortType> {
    if port == "out" {
        return Some(PortType::Control);
    }
    match (kind, port) {
        (NodeKind::GoalInput, "objective") => Some(PortType::String),
        (NodeKind::ModelCall | NodeKind::Delegate, "response") => Some(PortType::ModelResponse),
        (NodeKind::GeneratePatch, "patch") => Some(PortType::PatchEnvelope),
        (NodeKind::ImageGenerate, "image") => Some(PortType::ImageRef),
        (NodeKind::RunTests, "verification") => Some(PortType::VerificationResult),
        _ => None,
    }
}

fn input_port_type(kind: &NodeKind, port: &str) -> Option<PortType> {
    if port == "in" {
        return Some(PortType::Control);
    }
    match (kind, port) {
        (NodeKind::ModelCall | NodeKind::Delegate, "prompt") => Some(PortType::String),
        (NodeKind::FinalSeal, "proof") => Some(PortType::ProofRef),
        (NodeKind::ApplyPatch, "patch") => Some(PortType::PatchEnvelope),
        (NodeKind::ImageGenerate, "prompt") => Some(PortType::String),
        _ => None,
    }
}

fn required_input_port(kind: &NodeKind, port: &str) -> bool {
    matches!(
        (kind, port),
        (NodeKind::ModelCall, "prompt")
            | (NodeKind::Delegate, "prompt")
            | (NodeKind::ApplyPatch, "patch")
            | (NodeKind::FinalSeal, "proof")
    )
}

fn is_side_effect_node(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::GitPush
            | NodeKind::PullRequest
            | NodeKind::ApplyPatch
            | NodeKind::RunCommand
            | NodeKind::BrowserAction
            | NodeKind::AppAction
            | NodeKind::ComputerAction
    )
}

fn work_kind_for_node(kind: &NodeKind) -> WorkKind {
    match kind {
        NodeKind::ModelCall
        | NodeKind::Delegate
        | NodeKind::WorkerPool
        | NodeKind::CandidatePool
        | NodeKind::RoleRouter
        | NodeKind::FallbackRouter => WorkKind::ModelInference,
        NodeKind::RunTests
        | NodeKind::StaticAnalysis
        | NodeKind::SecurityScan
        | NodeKind::VerifierPool => WorkKind::Verification,
        NodeKind::GeneratePatch => WorkKind::WriteCandidate,
        NodeKind::ApplyPatch | NodeKind::GitStage | NodeKind::GitCommit => WorkKind::Integration,
        NodeKind::Approval | NodeKind::GitPush | NodeKind::PullRequest => WorkKind::Approval,
        NodeKind::RunCommand | NodeKind::McpTool | NodeKind::Skill => WorkKind::ToolExecution,
        _ => WorkKind::Planning,
    }
}

fn capabilities_for_node(kind: &NodeKind) -> CapabilityRequirements {
    match kind {
        NodeKind::ImageGenerate | NodeKind::ImageEdit | NodeKind::ImageVariation => {
            CapabilityRequirements::image_output()
        }
        NodeKind::GeneratePatch | NodeKind::ApplyPatch | NodeKind::RunTests => {
            CapabilityRequirements::code()
        }
        NodeKind::ModelCall | NodeKind::Delegate | NodeKind::WorkerPool => {
            CapabilityRequirements::text()
        }
        _ => CapabilityRequirements::default(),
    }
}

fn error(
    node_id: Option<String>,
    edge_id: Option<String>,
    reason_code: &str,
    message: &str,
) -> CompileDiagnostic {
    CompileDiagnostic {
        severity: DiagnosticSeverity::Error,
        node_id,
        edge_id,
        reason_code: reason_code.to_string(),
        message: message.to_string(),
    }
}

fn graph_hash(graph: &PipelineGraph) -> String {
    stable_hash(
        serde_json::to_string(graph)
            .unwrap_or_else(|_| "unserializable-graph".to_string())
            .as_bytes(),
    )
}

fn stable_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_template_compiles_with_deterministic_hash() {
        let graph = single_model_safe_template();
        let first = compile_graph(&graph);
        let second = compile_graph(&graph);
        assert_eq!(first.plan_hash, second.plan_hash);
        assert!(
            first
                .proof_contract
                .final_seal_node_ids
                .contains(&"seal".to_string())
        );
        assert!(
            first
                .diagnostics
                .iter()
                .all(|item| item.severity != DiagnosticSeverity::Error)
        );
    }

    #[test]
    fn loop_without_bound_is_compile_error() {
        let mut graph = single_model_safe_template();
        graph.nodes.insert(
            "loop".to_string(),
            node("loop", NodeKind::Loop, "Loop", 120.0, 120.0),
        );
        graph.edges = vec![control_edge("edge-goal-loop", "goal", "out", "loop", "in")];
        let plan = compile_graph(&graph);
        assert!(
            plan.diagnostics
                .iter()
                .any(|item| item.reason_code == "loop_bound_missing")
        );
        assert!(
            plan.diagnostics
                .iter()
                .any(|item| item.reason_code == "terminal_path_without_final_seal")
        );
    }

    #[test]
    fn side_effect_node_requires_approval() {
        let mut graph = single_model_safe_template();
        graph.nodes.insert(
            "push".to_string(),
            node("push", NodeKind::GitPush, "Git push", 320.0, 120.0),
        );
        graph
            .edges
            .push(control_edge("edge-seal-push", "seal", "out", "push", "in"));
        let plan = compile_graph(&graph);
        assert!(
            plan.diagnostics
                .iter()
                .any(|item| item.reason_code == "side_effect_requires_approval")
        );
    }
}
