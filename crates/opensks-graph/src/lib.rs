use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use opensks_contracts::{
    ApprovalPoint, ApprovalPolicy, COMPILED_PLAN_SCHEMA, CapabilityRequirements, CompileDiagnostic,
    CompileRepairAction, CompileRepairGroup, CompileRepairPlan, CompiledPlan, CompletionContract,
    DependencyIndex, DiagnosticSeverity, EdgeKind, EdgeSpec, GraphMetadata, GraphPoint,
    GraphPolicies, NodeKind, NodeSpec, OBJECTIVE_PLAN_RECEIPT_SCHEMA, PIPELINE_GRAPH_SCHEMA,
    PLANNER_SHARD_POLICY_SCHEMA, PipelineGraph, PlannerShardPolicy, PortRef, PortType,
    ProofRequirement, ResourcePlan, RetryPolicy, WorkKind, WorkTemplate,
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

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectiveGraphPlan {
    pub graph: PipelineGraph,
    pub compiled_plan: CompiledPlan,
    pub receipt: opensks_contracts::ObjectivePlanReceipt,
}

pub fn compile_graph(graph: &PipelineGraph) -> CompiledPlan {
    let mut diagnostics = validate_graph(graph);
    let graph_hash = graph_hash(graph);
    let shard_policy = planner_shard_policy(graph, &graph_hash);
    let work_templates = compile_work_templates(graph, shard_policy.as_ref());
    let dependency_index = dependency_index(graph);
    let resource_plan = resource_plan(graph);
    let approval_points = approval_points(graph);
    let final_seal_node_ids: Vec<String> = graph
        .nodes
        .values()
        .filter(|node| node.enabled && node.kind == NodeKind::FinalSeal)
        .map(|node| node.id.clone())
        .collect();
    let proof_requirements = proof_requirements(graph);
    let required_requirement_ids = proof_requirements
        .iter()
        .map(|requirement| requirement.id.clone())
        .collect::<Vec<_>>();
    if graph.policies.final_seal_required && final_seal_node_ids.is_empty() {
        diagnostics.push(error(
            None,
            None,
            "missing_final_seal",
            "graph policy requires a FinalSeal node",
        ));
    }
    let repair_plan = repair_plan_for_diagnostics(&diagnostics);
    let mut plan = CompiledPlan {
        schema: COMPILED_PLAN_SCHEMA.to_string(),
        graph_id: graph.id.clone(),
        graph_version: graph.version,
        graph_hash,
        plan_hash: String::new(),
        work_templates,
        dependency_index,
        resource_plan,
        shard_policy,
        approval_points,
        proof_contract: CompletionContract {
            required_requirement_ids,
            requirements: proof_requirements,
            final_seal_node_ids,
            evidence_required: true,
        },
        diagnostics,
        repair_plan,
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
    let mut edge_ids = BTreeSet::new();
    if graph.entry_nodes.is_empty() {
        diagnostics.push(error(
            None,
            None,
            "entry_node_missing",
            "graph must declare at least one entry node",
        ));
    }
    for entry in &graph.entry_nodes {
        match graph.nodes.get(entry) {
            Some(node) if node.enabled => {}
            Some(_) => diagnostics.push(error(
                Some(entry.clone()),
                None,
                "entry_node_disabled",
                "entry node must be enabled",
            )),
            None => diagnostics.push(error(
                Some(entry.clone()),
                None,
                "entry_node_unknown",
                "entry node must reference an existing node",
            )),
        }
    }
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
        if !edge_ids.insert(edge.id.clone()) {
            diagnostics.push(error(
                None,
                Some(edge.id.clone()),
                "duplicate_edge_id",
                "edge ids must be unique",
            ));
        }
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
        if !from.enabled && to.enabled {
            diagnostics.push(error(
                Some(to.id.clone()),
                Some(edge.id.clone()),
                "enabled_node_depends_on_disabled_node",
                "enabled nodes cannot depend on disabled node outputs",
            ));
        }
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

    diagnostics.extend(cycle_diagnostics(graph));
    diagnostics.extend(unreachable_enabled_node_diagnostics(graph));
    diagnostics.extend(requirement_diagnostics(graph));

    if graph.policies.final_seal_required {
        let outgoing = outgoing_enabled_edges(graph);
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

pub fn plan_graph_from_objective(
    request: &opensks_contracts::ObjectivePlanRequest,
) -> ObjectiveGraphPlan {
    let graph = objective_pipeline_graph(request);
    let compiled_plan = compile_graph(&graph);
    let objective_hash = stable_hash(request.objective.as_bytes());
    let shard_policy = compiled_plan.shard_policy.clone();
    let receipt = opensks_contracts::ObjectivePlanReceipt {
        schema: OBJECTIVE_PLAN_RECEIPT_SCHEMA.to_string(),
        objective_hash,
        graph_id: graph.id.clone(),
        graph_hash: compiled_plan.graph_hash.clone(),
        plan_hash: compiled_plan.plan_hash.clone(),
        source: "objective_planner".to_string(),
        max_parallelism: graph.policies.max_parallelism,
        role_count: normalized_role_count(request),
        work_template_count: compiled_plan.work_templates.len() as u32,
        repair_action_count: compiled_plan.repair_plan.actions.len() as u32,
        shard_policy_id: shard_policy.as_ref().map(|policy| policy.id.clone()),
        shard_policy,
        graph_ref: None,
        compiled_plan_ref: None,
        planner_provider_id: None,
        planner_model_id: None,
        planner_response_hash: None,
        planner_response_bytes: None,
        evidence_refs: objective_plan_evidence_refs(request),
    };
    ObjectiveGraphPlan {
        graph,
        compiled_plan,
        receipt,
    }
}

fn objective_pipeline_graph(request: &opensks_contracts::ObjectivePlanRequest) -> PipelineGraph {
    let objective_hash = stable_hash(request.objective.as_bytes());
    let short_hash = short_hash_segment(&objective_hash);
    let mut nodes = BTreeMap::new();
    let max_parallelism = normalized_max_parallelism(request);
    let role_count = normalized_role_count(request);
    let include_image_lane =
        request.include_image_lane || objective_mentions_any(&request.objective, IMAGE_TERMS);
    let include_research_lane =
        request.include_research_lane || objective_mentions_any(&request.objective, RESEARCH_TERMS);

    let mut goal = node("goal", NodeKind::GoalInput, "Objective input", 0.0, 0.0);
    goal.config = serde_json::json!({
        "objective_hash": objective_hash,
        "requirements": [{
            "id": "req-objective-understood",
            "description": "Objective is preserved as the planner input and traced by hash.",
            "evidence_refs": ["graph:objective-planner"]
        }]
    });
    nodes.insert(goal.id.clone(), goal);

    let mut decompose = node(
        "decompose",
        NodeKind::Decompose,
        "Decompose objective",
        180.0,
        0.0,
    );
    decompose.config = serde_json::json!({
        "objective_hash": objective_hash,
        "max_parallelism": max_parallelism,
        "role_count": role_count,
        "requirements": [{
            "id": "req-objective-decomposed",
            "description": "Objective is decomposed into role-scoped implementation and verification work.",
            "evidence_refs": ["graph:objective-planner"]
        }]
    });
    nodes.insert(decompose.id.clone(), decompose);

    if include_research_lane {
        let mut research = node(
            "research",
            NodeKind::WebResearch,
            "Research current context",
            360.0,
            -120.0,
        );
        research.approval = ApprovalPolicy::required("external_network");
        research.config = serde_json::json!({
            "objective_hash": objective_hash,
            "requirements": [{
                "id": "req-research-evidence",
                "description": "External research, when enabled, records source-bound evidence.",
                "evidence_refs": ["graph:objective-planner"]
            }]
        });
        nodes.insert(research.id.clone(), research);
    }

    if request.require_git_worktree {
        let mut worktree = node(
            "worktree",
            NodeKind::GitWorktree,
            "Prepare isolated worktree",
            360.0,
            0.0,
        );
        worktree.config = serde_json::json!({
            "isolation": "git_worktree_or_snapshot",
            "requirements": [{
                "id": "req-isolated-workspace",
                "description": "Code changes run in an isolated worktree or snapshot before integration.",
                "evidence_refs": ["graph:objective-planner"]
            }]
        });
        nodes.insert(worktree.id.clone(), worktree);
    }

    let mut role_router = node(
        "role_router",
        NodeKind::RoleRouter,
        "Route planner roles",
        540.0,
        0.0,
    );
    role_router.config = serde_json::json!({
        "roles": objective_roles(include_image_lane),
        "role_count": role_count,
        "requirements": [{
            "id": "req-role-routing",
            "description": "Planner roles are selected from provider/model capability, health, cost, and concurrency data.",
            "evidence_refs": ["provider:role-routing", "graph:objective-planner"]
        }]
    });
    nodes.insert(role_router.id.clone(), role_router);

    let mut workers = node(
        "workers",
        NodeKind::WorkerPool,
        "Parallel implementation workers",
        720.0,
        0.0,
    );
    workers.config = serde_json::json!({
        "role_count": role_count,
        "max_parallelism": max_parallelism,
        "requirements": [{
            "id": "req-parallel-candidates",
            "description": "Independent implementation workers produce isolated candidate evidence.",
            "evidence_refs": ["scheduler:parallel-batch-dispatch", "graph:objective-planner"]
        }]
    });
    nodes.insert(workers.id.clone(), workers);

    if include_image_lane {
        let mut image = node(
            "image",
            NodeKind::ImageGenerate,
            "Image or vision lane",
            720.0,
            -140.0,
        );
        image.config = serde_json::json!({
            "objective_hash": objective_hash,
            "requirements": [{
                "id": "req-image-lane",
                "description": "Image or vision work is routed through an image-capable model lane.",
                "evidence_refs": ["image:provider-route", "graph:objective-planner"]
            }]
        });
        nodes.insert(image.id.clone(), image);
    }

    let mut verifier = node(
        "verifier",
        NodeKind::VerifierPool,
        "Verifier pool",
        900.0,
        0.0,
    );
    verifier.config = serde_json::json!({
        "acceptance_criteria": [
            "targeted tests pass",
            "candidate diff is linked to proof requirements",
            "secret and workspace-path leakage checks pass"
        ],
        "requirements": [{
            "id": "req-verifier-evidence",
            "description": "Verifier workers produce evidence before integration.",
            "evidence_refs": ["graph:objective-planner"]
        }]
    });
    nodes.insert(verifier.id.clone(), verifier);

    if request.require_git_worktree {
        let mut apply = node(
            "apply",
            NodeKind::ApplyPatch,
            "Approval-gated integration",
            1080.0,
            0.0,
        );
        apply.approval = if request.require_integration_approval {
            ApprovalPolicy::required("integration_apply")
        } else {
            ApprovalPolicy::none()
        };
        apply.config = serde_json::json!({
            "requirements": [{
                "id": "req-integration-receipt",
                "description": "Integration writes a durable verification/final-seal receipt before main workspace mutation.",
                "evidence_refs": ["integration:verification-receipt", "graph:objective-planner"]
            }]
        });
        nodes.insert(apply.id.clone(), apply);
    }

    let mut seal = node("seal", NodeKind::FinalSeal, "Final seal", 1260.0, 0.0);
    seal.config = serde_json::json!({
        "requirements": [{
            "id": "req-final-seal",
            "description": "Final answer is allowed only after required evidence and final seal are present.",
            "evidence_refs": ["graph:objective-planner"]
        }]
    });
    nodes.insert(seal.id.clone(), seal);

    let mut edges = Vec::new();
    edges.push(control_edge(
        "edge-goal-decompose",
        "goal",
        "out",
        "decompose",
        "in",
    ));
    let mut previous = "decompose";
    if include_research_lane {
        edges.push(control_edge(
            "edge-decompose-research",
            "decompose",
            "out",
            "research",
            "in",
        ));
        previous = "research";
    }
    if request.require_git_worktree {
        edges.push(control_edge(
            "edge-planner-worktree",
            previous,
            "out",
            "worktree",
            "in",
        ));
        previous = "worktree";
    }
    edges.push(control_edge(
        "edge-planner-role-router",
        previous,
        "out",
        "role_router",
        "in",
    ));
    edges.push(control_edge(
        "edge-role-router-workers",
        "role_router",
        "out",
        "workers",
        "in",
    ));
    edges.push(control_edge(
        "edge-workers-verifier",
        "workers",
        "out",
        "verifier",
        "in",
    ));
    if include_image_lane {
        edges.push(control_edge(
            "edge-role-router-image",
            "role_router",
            "out",
            "image",
            "in",
        ));
        edges.push(control_edge(
            "edge-image-verifier",
            "image",
            "out",
            "verifier",
            "in",
        ));
    }
    if request.require_git_worktree {
        edges.push(control_edge(
            "edge-verifier-apply",
            "verifier",
            "out",
            "apply",
            "in",
        ));
        edges.push(control_edge(
            "edge-apply-seal",
            "apply",
            "out",
            "seal",
            "in",
        ));
    } else {
        edges.push(control_edge(
            "edge-verifier-seal",
            "verifier",
            "out",
            "seal",
            "in",
        ));
    }

    PipelineGraph {
        schema: PIPELINE_GRAPH_SCHEMA.to_string(),
        id: format!("objective-plan-{short_hash}"),
        name: "Objective Planner DAG".to_string(),
        version: 1,
        entry_nodes: vec!["goal".to_string()],
        nodes,
        edges,
        variables: BTreeMap::new(),
        policies: GraphPolicies {
            max_parallelism,
            allow_external_side_effects: false,
            final_seal_required: true,
        },
        metadata: GraphMetadata {
            description: "Objective-derived planner graph for isolated parallel implementation, verification, and approval-gated integration.".to_string(),
            created_by: "opensks-graph-objective-planner".to_string(),
            evidence_refs: objective_plan_evidence_refs(request),
        },
    }
}

const IMAGE_TERMS: &[&str] = &["image", "vision", "visual", "screenshot", "ui"];
const RESEARCH_TERMS: &[&str] = &["research", "web", "docs", "current", "latest"];

fn normalized_max_parallelism(request: &opensks_contracts::ObjectivePlanRequest) -> u32 {
    request.max_parallelism.clamp(1, 32)
}

fn normalized_role_count(request: &opensks_contracts::ObjectivePlanRequest) -> u32 {
    request
        .role_count
        .clamp(1, normalized_max_parallelism(request).max(1))
}

fn objective_mentions_any(objective: &str, terms: &[&str]) -> bool {
    let objective = objective.to_ascii_lowercase();
    terms.iter().any(|term| objective.contains(term))
}

fn objective_roles(include_image_lane: bool) -> Vec<&'static str> {
    let mut roles = vec!["planner", "code", "verifier", "arbiter"];
    if include_image_lane {
        roles.push("image");
    }
    roles
}

fn objective_plan_evidence_refs(request: &opensks_contracts::ObjectivePlanRequest) -> Vec<String> {
    let mut refs = vec![
        "graph:objective-planner".to_string(),
        "graph:dag-validation".to_string(),
        "graph:proof-contract-requirements".to_string(),
        "planner:shard-policy".to_string(),
    ];
    for evidence in &request.evidence_refs {
        if !refs.contains(evidence) {
            refs.push(evidence.clone());
        }
    }
    refs
}

fn planner_shard_policy(graph: &PipelineGraph, graph_hash: &str) -> Option<PlannerShardPolicy> {
    if !graph_is_objective_planner(graph) {
        return None;
    }
    let role_count = objective_graph_role_count(graph).max(1);
    let max_parallelism = graph.policies.max_parallelism.max(1);
    let implementation_shard_count = role_count.min(max_parallelism).max(1);
    let verifier_shard_count = if graph
        .nodes
        .values()
        .any(|node| node.enabled && node.kind == NodeKind::VerifierPool)
    {
        role_count.min(max_parallelism).max(1)
    } else {
        0
    };
    Some(PlannerShardPolicy {
        schema: PLANNER_SHARD_POLICY_SCHEMA.to_string(),
        id: format!("planner-shard-policy-{}", short_hash_segment(graph_hash)),
        source: "objective_planner".to_string(),
        role_count,
        max_parallelism,
        implementation_shard_count,
        verifier_shard_count,
        candidate_selection_policy: if graph
            .nodes
            .values()
            .any(|node| node.enabled && node.kind == NodeKind::ApplyPatch)
        {
            "planner_required_shards_before_approval_apply".to_string()
        } else {
            "planner_required_shards_before_final_seal".to_string()
        },
        required_gates: planner_shard_required_gates(graph),
        evidence_refs: vec![
            "planner:shard-policy".to_string(),
            "graph:objective-planner".to_string(),
            "graph:dag-validation".to_string(),
        ],
    })
}

fn graph_is_objective_planner(graph: &PipelineGraph) -> bool {
    graph.id.starts_with("objective-plan-")
        || graph
            .metadata
            .evidence_refs
            .iter()
            .any(|reference| reference == "graph:objective-planner")
}

fn objective_graph_role_count(graph: &PipelineGraph) -> u32 {
    graph
        .nodes
        .values()
        .find_map(|node| {
            node.config
                .get("role_count")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
        })
        .unwrap_or_else(|| {
            graph
                .nodes
                .values()
                .filter(|node| node.enabled)
                .filter(|node| {
                    matches!(
                        node.kind,
                        NodeKind::RoleRouter
                            | NodeKind::WorkerPool
                            | NodeKind::VerifierPool
                            | NodeKind::ApplyPatch
                            | NodeKind::ImageGenerate
                    )
                })
                .count()
                .max(1) as u32
        })
}

fn planner_shard_required_gates(graph: &PipelineGraph) -> Vec<String> {
    let mut gates = vec![
        "candidate_receipt_valid".to_string(),
        "target_policy_check".to_string(),
        "patch_apply_check".to_string(),
        "read_only_verifier_lanes".to_string(),
    ];
    if graph
        .nodes
        .values()
        .any(|node| node.enabled && node.approval.required)
    {
        gates.push("approval_event".to_string());
    }
    if graph.policies.final_seal_required {
        gates.push("final_seal".to_string());
    }
    gates
}

fn short_hash_segment(hash: &str) -> String {
    hash.strip_prefix("fnv1a64:")
        .unwrap_or(hash)
        .chars()
        .take(8)
        .collect()
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

fn compile_work_templates(
    graph: &PipelineGraph,
    shard_policy: Option<&PlannerShardPolicy>,
) -> Vec<WorkTemplate> {
    graph
        .nodes
        .values()
        .filter(|node| node.enabled)
        .map(|node| WorkTemplate {
            id: format!("work-template-{}", node.id),
            node_id: node.id.clone(),
            kind: work_kind_for_node(&node.kind),
            dependencies: incoming_enabled_dependencies(graph, &node.id),
            capability_requirements: capabilities_for_node(&node.kind),
            requirement_ids: requirement_ids_for_node(node),
            shard_policy_id: shard_policy.map(|policy| policy.id.clone()),
            shard_policy_selection_policy: shard_policy
                .map(|policy| policy.candidate_selection_policy.clone()),
            shard_policy_required_source_count: shard_policy
                .map(|policy| policy.implementation_shard_count as usize),
            shard_policy_required_verifier_count: shard_policy
                .map(|policy| policy.verifier_shard_count as usize),
        })
        .collect()
}

fn dependency_index(graph: &PipelineGraph) -> DependencyIndex {
    let prerequisites = graph
        .nodes
        .values()
        .filter(|node| node.enabled)
        .map(|node| {
            (
                node.id.clone(),
                incoming_enabled_dependencies(graph, &node.id),
            )
        })
        .collect();
    DependencyIndex { prerequisites }
}

fn incoming_enabled_dependencies(graph: &PipelineGraph, node_id: &str) -> Vec<String> {
    graph
        .edges
        .iter()
        .filter(|edge| edge.to.node_id == node_id)
        .filter(|edge| {
            graph
                .nodes
                .get(&edge.from.node_id)
                .is_some_and(|node| node.enabled)
        })
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

fn proof_requirements(graph: &PipelineGraph) -> Vec<ProofRequirement> {
    let mut requirements = BTreeMap::new();
    for node in graph.nodes.values().filter(|node| node.enabled) {
        for requirement in node_proof_requirements(node) {
            requirements
                .entry(requirement.id.clone())
                .or_insert(requirement);
        }
    }
    requirements.into_values().collect()
}

fn node_proof_requirements(node: &NodeSpec) -> Vec<ProofRequirement> {
    let mut requirements = BTreeMap::new();
    if let Some(ids) = node
        .config
        .get("requirement_ids")
        .and_then(|value| value.as_array())
    {
        for id in ids.iter().filter_map(|value| value.as_str()) {
            let id = id.trim();
            if id.is_empty() {
                continue;
            }
            requirements
                .entry(id.to_string())
                .or_insert_with(|| proof_requirement(node, id, id, Vec::new()));
        }
    }
    if let Some(items) = node
        .config
        .get("requirements")
        .and_then(|value| value.as_array())
    {
        for (index, item) in items.iter().enumerate() {
            if let Some(text) = item.as_str() {
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                let id = generated_requirement_id(&node.id, index, text);
                requirements
                    .entry(id.clone())
                    .or_insert_with(|| proof_requirement(node, &id, text, Vec::new()));
                continue;
            }
            let Some(object) = item.as_object() else {
                continue;
            };
            let description = object
                .get("description")
                .and_then(|value| value.as_str())
                .or_else(|| object.get("text").and_then(|value| value.as_str()))
                .unwrap_or("")
                .trim();
            let configured_id = object.get("id").and_then(|value| value.as_str());
            let id = configured_id
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    generated_requirement_id(
                        &node.id,
                        index,
                        if description.is_empty() {
                            "requirement"
                        } else {
                            description
                        },
                    )
                });
            let evidence_refs = object
                .get("evidence_refs")
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let description = if description.is_empty() {
                id.as_str()
            } else {
                description
            };
            requirements
                .entry(id.clone())
                .or_insert_with(|| proof_requirement(node, &id, description, evidence_refs));
        }
    }
    if let Some(items) = node
        .config
        .get("acceptance_criteria")
        .and_then(|value| value.as_array())
    {
        for (index, item) in items.iter().enumerate() {
            let Some(text) = item
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let id = generated_requirement_id(&node.id, index, text);
            requirements
                .entry(id.clone())
                .or_insert_with(|| proof_requirement(node, &id, text, Vec::new()));
        }
    }
    requirements.into_values().collect()
}

fn requirement_ids_for_node(node: &NodeSpec) -> Vec<String> {
    node_proof_requirements(node)
        .into_iter()
        .map(|requirement| requirement.id)
        .collect()
}

fn proof_requirement(
    node: &NodeSpec,
    id: &str,
    description: &str,
    evidence_refs: Vec<String>,
) -> ProofRequirement {
    ProofRequirement {
        id: id.to_string(),
        source_node_id: Some(node.id.clone()),
        description: description.to_string(),
        evidence_required: true,
        evidence_refs,
    }
}

fn generated_requirement_id(node_id: &str, index: usize, text: &str) -> String {
    let hash = stable_hash(text.as_bytes())
        .strip_prefix("fnv1a64:")
        .unwrap_or("0000000000000000")
        .chars()
        .take(8)
        .collect::<String>();
    format!(
        "req-{}-{}-{}",
        safe_requirement_segment(node_id),
        index.saturating_add(1),
        hash
    )
}

fn safe_requirement_segment(value: &str) -> String {
    let segment = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let collapsed = segment
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if collapsed.is_empty() {
        "node".to_string()
    } else {
        collapsed
    }
}

fn requirement_diagnostics(graph: &PipelineGraph) -> Vec<CompileDiagnostic> {
    let mut diagnostics = Vec::new();
    for node in graph.nodes.values().filter(|node| node.enabled) {
        if let Some(value) = node.config.get("requirement_ids")
            && !value.is_array()
        {
            diagnostics.push(error(
                Some(node.id.clone()),
                None,
                "requirement_ids_not_array",
                "node config requirement_ids must be an array of strings",
            ));
        }
        if let Some(values) = node
            .config
            .get("requirement_ids")
            .and_then(|value| value.as_array())
        {
            for value in values {
                if value.as_str().map(str::trim).is_none_or(str::is_empty) {
                    diagnostics.push(error(
                        Some(node.id.clone()),
                        None,
                        "requirement_id_empty",
                        "requirement_ids entries must be non-empty strings",
                    ));
                }
            }
        }
        if let Some(value) = node.config.get("requirements")
            && !value.is_array()
        {
            diagnostics.push(error(
                Some(node.id.clone()),
                None,
                "requirements_not_array",
                "node config requirements must be an array",
            ));
        }
        if let Some(values) = node
            .config
            .get("requirements")
            .and_then(|value| value.as_array())
        {
            for value in values {
                match value {
                    serde_json::Value::String(text) if text.trim().is_empty() => {
                        diagnostics.push(error(
                            Some(node.id.clone()),
                            None,
                            "requirement_description_empty",
                            "requirements string entries must be non-empty",
                        ));
                    }
                    serde_json::Value::Object(object) => {
                        if object
                            .get("id")
                            .and_then(|value| value.as_str())
                            .is_some_and(|id| id.trim().is_empty())
                        {
                            diagnostics.push(error(
                                Some(node.id.clone()),
                                None,
                                "requirement_id_empty",
                                "requirement object id must be non-empty when present",
                            ));
                        }
                    }
                    serde_json::Value::String(_) => {}
                    _ => diagnostics.push(error(
                        Some(node.id.clone()),
                        None,
                        "requirement_invalid",
                        "requirements entries must be strings or objects",
                    )),
                }
            }
        }
        if let Some(value) = node.config.get("acceptance_criteria")
            && !value.is_array()
        {
            diagnostics.push(error(
                Some(node.id.clone()),
                None,
                "acceptance_criteria_not_array",
                "node config acceptance_criteria must be an array of strings",
            ));
        }
        if let Some(values) = node
            .config
            .get("acceptance_criteria")
            .and_then(|value| value.as_array())
        {
            for value in values {
                if value.as_str().map(str::trim).is_none_or(str::is_empty) {
                    diagnostics.push(error(
                        Some(node.id.clone()),
                        None,
                        "acceptance_criterion_empty",
                        "acceptance_criteria entries must be non-empty strings",
                    ));
                }
            }
        }
    }
    diagnostics
}

fn repair_plan_for_diagnostics(diagnostics: &[CompileDiagnostic]) -> CompileRepairPlan {
    let actions = diagnostics
        .iter()
        .enumerate()
        .map(|(index, diagnostic)| repair_action_for_diagnostic(index, diagnostic))
        .collect::<Vec<_>>();
    if actions.is_empty() {
        return CompileRepairPlan::default();
    }
    let groups = repair_groups_for_actions(&actions);
    CompileRepairPlan {
        bounded: true,
        max_iterations: 3,
        reason_code: "blocking_diagnostics_require_bounded_repair".to_string(),
        actions,
        groups,
    }
}

fn repair_action_for_diagnostic(
    index: usize,
    diagnostic: &CompileDiagnostic,
) -> CompileRepairAction {
    let (action, description) = repair_instruction(&diagnostic.reason_code);
    CompileRepairAction {
        id: repair_action_id(index, diagnostic),
        diagnostic_reason_code: diagnostic.reason_code.clone(),
        target_node_id: diagnostic.node_id.clone(),
        target_edge_id: diagnostic.edge_id.clone(),
        action: action.to_string(),
        description: description.to_string(),
        evidence_required: diagnostic.severity == DiagnosticSeverity::Error,
    }
}

fn repair_instruction(reason_code: &str) -> (&'static str, &'static str) {
    match reason_code {
        "entry_node_missing" => (
            "declare_enabled_entry_node",
            "Add at least one enabled entry node before compiling the graph.",
        ),
        "entry_node_disabled" => (
            "enable_entry_node_or_remove_reference",
            "Enable the referenced entry node or remove it from entry_nodes.",
        ),
        "entry_node_unknown" => (
            "replace_unknown_entry_reference",
            "Replace entry_nodes references that do not match a declared graph node.",
        ),
        "node_key_id_mismatch" => (
            "align_node_map_key_and_id",
            "Make each node map key match its node id.",
        ),
        "loop_bound_missing" => (
            "add_loop_bounds",
            "Add max_iterations, stop_condition, and no_progress_limit to bounded loop nodes.",
        ),
        "side_effect_requires_approval" => (
            "add_explicit_approval_policy",
            "Attach an explicit approval policy before a side-effecting node can run.",
        ),
        "duplicate_edge_id" => (
            "assign_unique_edge_id",
            "Give every graph edge a unique id.",
        ),
        "edge_endpoint_missing" => (
            "restore_or_remove_missing_edge_endpoint",
            "Restore missing endpoint nodes or remove edges that reference them.",
        ),
        "enabled_node_depends_on_disabled_node" | "required_input_from_disabled_node" => (
            "enable_or_reroute_dependency",
            "Enable the dependency source, reroute the edge, or disable the dependent path.",
        ),
        "port_type_mismatch" => (
            "match_port_types_or_insert_adapter",
            "Connect compatible ports or insert an adapter node with a typed conversion.",
        ),
        "cycle_detected" => (
            "break_cycle",
            "Remove or redirect one control edge so enabled graph paths are acyclic.",
        ),
        "unreachable_enabled_node" => (
            "connect_or_disable_unreachable_node",
            "Connect the enabled node to an enabled entry path or disable it.",
        ),
        "terminal_path_without_final_seal" => (
            "connect_terminal_path_to_final_seal",
            "Connect terminal enabled paths to a FinalSeal, Cancelled, or Blocked node.",
        ),
        "missing_final_seal" => (
            "add_final_seal_node",
            "Add an enabled FinalSeal node when graph policy requires final sealing.",
        ),
        "requirement_ids_not_array"
        | "requirement_id_empty"
        | "requirements_not_array"
        | "requirement_description_empty"
        | "requirement_invalid"
        | "acceptance_criteria_not_array"
        | "acceptance_criterion_empty" => (
            "normalize_proof_requirement_config",
            "Rewrite proof requirement config as non-empty strings or structured requirement objects.",
        ),
        _ => (
            "manual_graph_repair",
            "Inspect the diagnostic and update the graph before compiling again.",
        ),
    }
}

fn repair_groups_for_actions(actions: &[CompileRepairAction]) -> Vec<CompileRepairGroup> {
    let mut groups: BTreeMap<String, CompileRepairGroup> = BTreeMap::new();
    for action in actions {
        let (group_kind, description) = repair_group_descriptor(&action.diagnostic_reason_code);
        let group = groups
            .entry(group_kind.to_string())
            .or_insert_with(|| CompileRepairGroup {
                id: format!("repair-group-{}", safe_requirement_segment(group_kind)),
                group_kind: group_kind.to_string(),
                description: description.to_string(),
                action_ids: Vec::new(),
                diagnostic_reason_codes: Vec::new(),
                node_ids: Vec::new(),
                edge_ids: Vec::new(),
            });
        push_unique(&mut group.action_ids, action.id.clone());
        push_unique(
            &mut group.diagnostic_reason_codes,
            action.diagnostic_reason_code.clone(),
        );
        if let Some(node_id) = &action.target_node_id {
            push_unique(&mut group.node_ids, node_id.clone());
        }
        if let Some(edge_id) = &action.target_edge_id {
            push_unique(&mut group.edge_ids, edge_id.clone());
        }
    }
    groups.into_values().collect()
}

fn repair_group_descriptor(reason_code: &str) -> (&'static str, &'static str) {
    match reason_code {
        "entry_node_missing" | "entry_node_disabled" | "entry_node_unknown" => (
            "entry_path",
            "Entry path repairs required before the graph can start.",
        ),
        "node_key_id_mismatch" | "duplicate_edge_id" | "edge_endpoint_missing" => (
            "graph_integrity",
            "Graph identity and endpoint repairs required before planning.",
        ),
        "cycle_detected" => (
            "control_flow_conflict",
            "Control-flow cycle repairs required before scheduler planning.",
        ),
        "enabled_node_depends_on_disabled_node"
        | "required_input_from_disabled_node"
        | "unreachable_enabled_node" => (
            "dependency_path",
            "Dependency path repairs required for enabled scheduler work.",
        ),
        "port_type_mismatch" => (
            "port_type_conflict",
            "Typed port repairs required before data/control flow can be connected.",
        ),
        "terminal_path_without_final_seal" | "missing_final_seal" => (
            "final_seal_path",
            "Terminal path repairs required to reach a valid final seal.",
        ),
        "loop_bound_missing" => (
            "loop_bounds",
            "Loop-bound repairs required before iterative work can run.",
        ),
        "side_effect_requires_approval" => (
            "approval_policy",
            "Approval-policy repairs required before side effects can run.",
        ),
        "requirement_ids_not_array"
        | "requirement_id_empty"
        | "requirements_not_array"
        | "requirement_description_empty"
        | "requirement_invalid"
        | "acceptance_criteria_not_array"
        | "acceptance_criterion_empty" => (
            "proof_contract_config",
            "Proof-contract repairs required before acceptance evidence can be sealed.",
        ),
        _ => (
            "manual_graph_repair",
            "Manual graph repair required for uncategorized diagnostics.",
        ),
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn repair_action_id(index: usize, diagnostic: &CompileDiagnostic) -> String {
    let target = diagnostic
        .node_id
        .as_deref()
        .or(diagnostic.edge_id.as_deref())
        .unwrap_or("graph");
    format!(
        "repair-{}-{}-{}",
        index.saturating_add(1),
        safe_requirement_segment(&diagnostic.reason_code),
        safe_requirement_segment(target)
    )
}

fn outgoing_enabled_edges(graph: &PipelineGraph) -> BTreeMap<String, Vec<&EdgeSpec>> {
    let mut outgoing: BTreeMap<String, Vec<&EdgeSpec>> = BTreeMap::new();
    for edge in &graph.edges {
        let Some(from) = graph.nodes.get(&edge.from.node_id) else {
            continue;
        };
        let Some(to) = graph.nodes.get(&edge.to.node_id) else {
            continue;
        };
        if !from.enabled || !to.enabled {
            continue;
        }
        outgoing
            .entry(edge.from.node_id.clone())
            .or_default()
            .push(edge);
    }
    outgoing
}

fn cycle_diagnostics(graph: &PipelineGraph) -> Vec<CompileDiagnostic> {
    let mut diagnostics = Vec::new();
    let mut state: BTreeMap<String, VisitState> = BTreeMap::new();
    let mut stack = Vec::new();
    for node in graph.nodes.values().filter(|node| node.enabled) {
        if !state.contains_key(&node.id) {
            visit_for_cycle(graph, &node.id, &mut state, &mut stack, &mut diagnostics);
        }
    }
    diagnostics
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisitState {
    Visiting,
    Done,
}

fn visit_for_cycle(
    graph: &PipelineGraph,
    node_id: &str,
    state: &mut BTreeMap<String, VisitState>,
    stack: &mut Vec<String>,
    diagnostics: &mut Vec<CompileDiagnostic>,
) {
    state.insert(node_id.to_string(), VisitState::Visiting);
    stack.push(node_id.to_string());
    for edge in graph
        .edges
        .iter()
        .filter(|edge| edge.from.node_id == node_id)
    {
        let Some(to) = graph.nodes.get(&edge.to.node_id) else {
            continue;
        };
        if !to.enabled {
            continue;
        }
        match state.get(&to.id).copied() {
            Some(VisitState::Done) => {}
            Some(VisitState::Visiting) => {
                let cycle = stack
                    .iter()
                    .skip_while(|id| *id != &to.id)
                    .cloned()
                    .chain(std::iter::once(to.id.clone()))
                    .collect::<Vec<_>>()
                    .join(" -> ");
                diagnostics.push(error(
                    Some(to.id.clone()),
                    Some(edge.id.clone()),
                    "cycle_detected",
                    &format!("graph must be acyclic; cycle: {cycle}"),
                ));
            }
            None => visit_for_cycle(graph, &to.id, state, stack, diagnostics),
        }
    }
    let _ = stack.pop();
    state.insert(node_id.to_string(), VisitState::Done);
}

fn unreachable_enabled_node_diagnostics(graph: &PipelineGraph) -> Vec<CompileDiagnostic> {
    let mut reachable = BTreeSet::new();
    let mut stack = graph
        .entry_nodes
        .iter()
        .filter(|id| graph.nodes.get(*id).is_some_and(|node| node.enabled))
        .cloned()
        .collect::<Vec<_>>();
    while let Some(node_id) = stack.pop() {
        if !reachable.insert(node_id.clone()) {
            continue;
        }
        for edge in graph
            .edges
            .iter()
            .filter(|edge| edge.from.node_id == node_id)
        {
            if graph
                .nodes
                .get(&edge.to.node_id)
                .is_some_and(|node| node.enabled)
            {
                stack.push(edge.to.node_id.clone());
            }
        }
    }
    graph
        .nodes
        .values()
        .filter(|node| node.enabled)
        .filter(|node| !reachable.contains(&node.id))
        .map(|node| {
            error(
                Some(node.id.clone()),
                None,
                "unreachable_enabled_node",
                "enabled node must be reachable from an enabled entry node",
            )
        })
        .collect()
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
        assert!(first.repair_plan.bounded);
        assert_eq!(first.repair_plan.max_iterations, 0);
        assert_eq!(first.repair_plan.reason_code, "no_repairs_required");
        assert!(first.repair_plan.actions.is_empty());
        assert!(first.repair_plan.groups.is_empty());
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

    #[test]
    fn compiler_rejects_cycles_duplicate_edges_and_bad_entries() {
        let mut graph = single_model_safe_template();
        graph.entry_nodes = vec!["missing".to_string()];
        graph.edges = vec![
            control_edge("edge-cycle", "goal", "out", "delegate", "in"),
            control_edge("edge-cycle", "delegate", "out", "goal", "in"),
            control_edge("edge-delegate-seal", "delegate", "out", "seal", "in"),
        ];
        let plan = compile_graph(&graph);
        for reason in ["entry_node_unknown", "duplicate_edge_id", "cycle_detected"] {
            assert!(
                plan.diagnostics
                    .iter()
                    .any(|item| item.reason_code == reason),
                "missing diagnostic {reason}: {:#?}",
                plan.diagnostics
            );
        }
        assert!(plan.repair_plan.bounded);
        assert_eq!(plan.repair_plan.max_iterations, 3);
        assert_eq!(
            plan.repair_plan.reason_code,
            "blocking_diagnostics_require_bounded_repair"
        );
        for reason in ["entry_node_unknown", "duplicate_edge_id", "cycle_detected"] {
            assert!(
                plan.repair_plan
                    .actions
                    .iter()
                    .any(|item| item.diagnostic_reason_code == reason),
                "missing repair action for {reason}: {:#?}",
                plan.repair_plan
            );
        }
        let cycle_repair = plan
            .repair_plan
            .actions
            .iter()
            .find(|item| item.diagnostic_reason_code == "cycle_detected")
            .expect("cycle repair");
        assert_eq!(cycle_repair.target_edge_id.as_deref(), Some("edge-cycle"));
        assert_eq!(cycle_repair.action, "break_cycle");
        assert!(cycle_repair.evidence_required);
        let control_group = plan
            .repair_plan
            .groups
            .iter()
            .find(|group| group.group_kind == "control_flow_conflict")
            .expect("control-flow repair group");
        assert!(control_group.action_ids.contains(&cycle_repair.id));
        assert!(control_group.edge_ids.contains(&"edge-cycle".to_string()));
        assert!(
            control_group
                .diagnostic_reason_codes
                .contains(&"cycle_detected".to_string())
        );
        assert!(
            plan.repair_plan
                .groups
                .iter()
                .any(|group| group.group_kind == "entry_path")
        );
        assert!(
            plan.repair_plan
                .groups
                .iter()
                .any(|group| group.group_kind == "graph_integrity")
        );
    }

    #[test]
    fn compiler_promotes_acceptance_criteria_into_proof_contract() {
        let mut graph = single_model_safe_template();
        graph.nodes.get_mut("delegate").expect("delegate").config = serde_json::json!({
            "requirement_ids": ["req-explicit"],
            "acceptance_criteria": [
                "candidate patch is produced in an isolated workspace"
            ],
            "requirements": [{
                "id": "req-tests",
                "description": "targeted tests pass",
                "evidence_refs": ["test:targeted"]
            }]
        });
        let plan = compile_graph(&graph);
        let delegate = plan
            .work_templates
            .iter()
            .find(|template| template.node_id == "delegate")
            .expect("delegate work template");
        assert!(
            delegate
                .requirement_ids
                .contains(&"req-explicit".to_string())
        );
        assert!(delegate.requirement_ids.contains(&"req-tests".to_string()));
        assert!(
            delegate
                .requirement_ids
                .iter()
                .any(|id| id.starts_with("req-delegate-1-"))
        );
        assert_eq!(
            plan.proof_contract.required_requirement_ids,
            delegate.requirement_ids
        );
        let test_requirement = plan
            .proof_contract
            .requirements
            .iter()
            .find(|requirement| requirement.id == "req-tests")
            .expect("structured requirement");
        assert_eq!(test_requirement.source_node_id.as_deref(), Some("delegate"));
        assert_eq!(test_requirement.description, "targeted tests pass");
        assert_eq!(test_requirement.evidence_refs, vec!["test:targeted"]);
        assert!(test_requirement.evidence_required);
    }

    #[test]
    fn malformed_requirement_config_is_blocking_diagnostic() {
        let mut graph = single_model_safe_template();
        graph.nodes.get_mut("delegate").expect("delegate").config = serde_json::json!({
            "requirement_ids": ["req-ok", ""],
            "acceptance_criteria": ["ship it", ""],
            "requirements": [42]
        });
        let plan = compile_graph(&graph);
        for reason in [
            "requirement_id_empty",
            "acceptance_criterion_empty",
            "requirement_invalid",
        ] {
            assert!(
                plan.diagnostics
                    .iter()
                    .any(|item| item.reason_code == reason),
                "missing diagnostic {reason}: {:#?}",
                plan.diagnostics
            );
        }
    }

    #[test]
    fn objective_planner_generates_valid_parallel_integration_dag() {
        let mut request =
            opensks_contracts::ObjectivePlanRequest::new("Implement provider UI with visual proof");
        request.max_parallelism = 8;
        request.role_count = 5;
        let planned = plan_graph_from_objective(&request);
        assert!(planned.graph.id.starts_with("objective-plan-"));
        assert_eq!(planned.graph.policies.max_parallelism, 8);
        assert!(
            planned
                .graph
                .metadata
                .evidence_refs
                .contains(&"graph:objective-planner".to_string())
        );
        assert!(
            planned
                .compiled_plan
                .diagnostics
                .iter()
                .all(|item| item.severity != DiagnosticSeverity::Error),
            "{:#?}",
            planned.compiled_plan.diagnostics
        );
        assert!(planned.compiled_plan.resource_plan.requires_git_worktree);
        assert!(planned.compiled_plan.resource_plan.requires_image);
        let shard_policy = planned
            .compiled_plan
            .shard_policy
            .as_ref()
            .expect("planner shard policy");
        assert_eq!(shard_policy.schema, PLANNER_SHARD_POLICY_SCHEMA);
        assert_eq!(shard_policy.source, "objective_planner");
        assert_eq!(shard_policy.role_count, 5);
        assert_eq!(shard_policy.max_parallelism, 8);
        assert_eq!(shard_policy.implementation_shard_count, 5);
        assert_eq!(shard_policy.verifier_shard_count, 5);
        assert_eq!(
            shard_policy.candidate_selection_policy,
            "planner_required_shards_before_approval_apply"
        );
        assert!(
            shard_policy
                .required_gates
                .contains(&"approval_event".to_string())
        );
        assert!(
            planned
                .graph
                .metadata
                .evidence_refs
                .contains(&"planner:shard-policy".to_string())
        );
        assert!(
            planned
                .compiled_plan
                .approval_points
                .iter()
                .any(|point| point.scope == "integration_apply")
        );
        for node_id in ["role_router", "workers", "verifier", "apply", "seal"] {
            assert!(
                planned
                    .compiled_plan
                    .work_templates
                    .iter()
                    .any(|template| template.node_id == node_id),
                "missing work template for {node_id}"
            );
        }
        assert!(
            planned
                .compiled_plan
                .proof_contract
                .required_requirement_ids
                .contains(&"req-parallel-candidates".to_string())
        );
        assert_eq!(planned.receipt.source, "objective_planner");
        assert_eq!(planned.receipt.repair_action_count, 0);
        assert_eq!(
            planned.receipt.shard_policy_id.as_deref(),
            Some(shard_policy.id.as_str())
        );
        assert_eq!(planned.receipt.shard_policy.as_ref(), Some(shard_policy));
        assert_eq!(
            planned.receipt.work_template_count as usize,
            planned.compiled_plan.work_templates.len()
        );
        assert!(
            planned
                .compiled_plan
                .work_templates
                .iter()
                .all(|template| template.shard_policy_id.as_deref()
                    == Some(shard_policy.id.as_str()))
        );
        assert!(planned.compiled_plan.work_templates.iter().all(|template| {
            template.shard_policy_selection_policy.as_deref()
                == Some(shard_policy.candidate_selection_policy.as_str())
        }));
        assert!(planned.compiled_plan.work_templates.iter().all(|template| {
            template.shard_policy_required_source_count
                == Some(shard_policy.implementation_shard_count as usize)
        }));
        assert!(planned.compiled_plan.work_templates.iter().all(|template| {
            template.shard_policy_required_verifier_count
                == Some(shard_policy.verifier_shard_count as usize)
        }));
    }

    #[test]
    fn objective_planner_can_emit_read_only_final_seal_dag() {
        let mut request = opensks_contracts::ObjectivePlanRequest::new("Summarize architecture");
        request.require_git_worktree = false;
        request.include_research_lane = true;
        let planned = plan_graph_from_objective(&request);
        assert!(
            planned
                .compiled_plan
                .diagnostics
                .iter()
                .all(|item| item.severity != DiagnosticSeverity::Error),
            "{:#?}",
            planned.compiled_plan.diagnostics
        );
        assert!(!planned.compiled_plan.resource_plan.requires_git_worktree);
        assert!(
            planned
                .compiled_plan
                .approval_points
                .iter()
                .any(|point| point.scope == "external_network")
        );
        assert!(
            planned
                .graph
                .edges
                .iter()
                .any(|edge| edge.id == "edge-verifier-seal")
        );
    }

    #[test]
    fn disabled_nodes_are_not_compiled_as_scheduler_dependencies() {
        let mut graph = single_model_safe_template();
        graph.nodes.get_mut("delegate").expect("delegate").enabled = false;
        graph.edges = vec![control_edge("edge-goal-seal", "goal", "out", "seal", "in")];
        let plan = compile_graph(&graph);
        assert!(
            plan.work_templates
                .iter()
                .all(|template| template.node_id != "delegate")
        );
        assert_eq!(
            plan.dependency_index.prerequisites.get("seal"),
            Some(&vec!["goal".to_string()])
        );
        assert!(!plan.dependency_index.prerequisites.contains_key("delegate"));
    }

    #[test]
    fn enabled_dependency_from_disabled_node_is_blocking_diagnostic() {
        let mut graph = single_model_safe_template();
        graph.nodes.get_mut("delegate").expect("delegate").enabled = false;
        graph.edges = vec![
            control_edge("edge-goal-delegate", "goal", "out", "delegate", "in"),
            control_edge("edge-delegate-seal", "delegate", "out", "seal", "in"),
        ];
        let plan = compile_graph(&graph);
        assert!(
            plan.diagnostics
                .iter()
                .any(|item| item.reason_code == "enabled_node_depends_on_disabled_node"),
            "{:#?}",
            plan.diagnostics
        );
    }
}
