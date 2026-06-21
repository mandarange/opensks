import Foundation

struct GraphEditorNode: Identifiable, Equatable, Codable, Sendable {
    let id: String
    var kind: String
    var title: String
    var inputType: String?
    var outputType: String?
}

struct GraphEditorEdge: Identifiable, Equatable, Codable, Sendable {
    let id: String
    let fromNodeId: String
    let toNodeId: String
    let portType: String
}

struct GraphCompileProblem: Identifiable, Equatable, Sendable {
    let id: String
    let nodeId: String?
    let message: String
}

struct GraphEditorDocument: Equatable, Codable, Sendable {
    let schema: String
    var id: String
    var name: String
    var nodes: [GraphEditorNode]
    var edges: [GraphEditorEdge]
    var runTemplateId: String
}

private struct PipelineGraphDTO: Encodable {
    let schema: String
    let id: String
    let name: String
    let version: Int
    let entryNodes: [String]
    let nodes: [String: PipelineNodeDTO]
    let edges: [PipelineEdgeDTO]
    let variables: [String: PipelineGraphValueDTO]
    let policies: PipelineGraphPoliciesDTO
    let metadata: PipelineGraphMetadataDTO
}

private struct PipelineGraphValueDTO: Encodable {}

private struct PipelineNodeDTO: Encodable {
    let id: String
    let kind: String
    let displayName: String
    let enabled: Bool
    let position: PipelineGraphPointDTO
    let inputs: [String: PipelinePortBindingDTO]
    let config: [String: String]
    let retry: PipelineRetryPolicyDTO
    let timeoutMs: UInt64?
    let approval: PipelineApprovalPolicyDTO
    let hookRefs: [String]
}

private struct PipelineGraphPointDTO: Encodable {
    let x: Double
    let y: Double
}

private struct PipelinePortBindingDTO: Encodable {}

private struct PipelineRetryPolicyDTO: Encodable {
    let maxAttempts: Int
    let backoffMs: UInt64
    let retryableReasonCodes: [String]
}

private struct PipelineApprovalPolicyDTO: Encodable {
    let required: Bool
    let scope: String
}

private struct PipelineEdgeDTO: Encodable {
    let id: String
    let from: PipelinePortRefDTO
    let to: PipelinePortRefDTO
    let kind: String
}

private struct PipelinePortRefDTO: Encodable {
    let nodeId: String
    let port: String
}

private struct PipelineGraphPoliciesDTO: Encodable {
    let maxParallelism: Int
    let allowExternalSideEffects: Bool
    let finalSealRequired: Bool
}

private struct PipelineGraphMetadataDTO: Encodable {
    let description: String
    let createdBy: String
    let evidenceRefs: [String]
}

@MainActor
final class GraphEditorStore: ObservableObject {
    @Published private(set) var documentId = "single-model-safe"
    @Published private(set) var documentName = "Single Model Safe"
    @Published private(set) var nodes: [GraphEditorNode] = []
    @Published private(set) var edges: [GraphEditorEdge] = []
    @Published private(set) var problems: [GraphCompileProblem] = []
    @Published private(set) var lastSavedPath: String?
    @Published private(set) var lastLoadedPath: String?
    @Published private(set) var lastExportedGraphPath: String?

    private var undoStack: [([GraphEditorNode], [GraphEditorEdge])] = []
    private var redoStack: [([GraphEditorNode], [GraphEditorEdge])] = []

    var exportedGraphRelativePath: String {
        Self.exportedGraphRelativePath
    }

    var canRunDaemonTemplate: Bool {
        Self.daemonTemplateIds.contains(documentId)
    }

    private static let daemonTemplateIds: Set<String> = [
        "single-model-safe",
        "balanced-multi-model",
        "extreme-parallel",
        "image-heavy-product-build",
        "research-report"
    ]

    private static let exportedGraphRelativePath = ".opensks/pipelines/editor/current.graph.json"

    private static let sideEffectNodeKinds: Set<String> = [
        "apply_patch",
        "run_command",
        "git_push",
        "pull_request",
        "browser_action",
        "app_action",
        "computer_action"
    ]

    private static let supportedNodeKinds: Set<String> = [
        "goal_input", "requirement_extractor", "requirement_gate", "branch", "switch", "join_all",
        "join_any", "loop", "delay", "queue", "approval", "breakpoint", "subgraph", "final_seal",
        "code_graph_query", "context_pack", "tri_wiki_recall", "wrongness_recall", "glossary_query",
        "architecture_snapshot", "web_research", "mcp_resource", "reasoning_strategy", "socratic_review",
        "debate", "red_team", "consensus", "arbiter", "decompose", "critique", "synthesize",
        "model_call", "delegate", "candidate_pool", "worker_pool", "verifier_pool", "role_router",
        "fallback_router", "quorum", "read_files", "search_code", "run_command", "mcp_tool", "skill",
        "generate_patch", "apply_patch", "run_tests", "static_analysis", "security_scan",
        "image_generate", "image_edit", "image_variation", "screenshot_capture", "visual_review",
        "image_voxel_anchor", "before_after_compare", "git_status", "git_diff", "git_worktree",
        "git_stage", "git_commit", "git_push", "pull_request", "browser_observe", "browser_action",
        "app_inspect", "app_action", "computer_observe", "computer_action", "cancelled", "blocked"
    ]

    func reset(nodes: [GraphEditorNode] = [], edges: [GraphEditorEdge] = []) {
        self.documentId = "editor-draft"
        self.documentName = "Editor Draft"
        self.nodes = nodes
        self.edges = edges
        self.problems = compileProblems(nodes: nodes, edges: edges)
        undoStack.removeAll()
        redoStack.removeAll()
    }

    func loadSingleModelSafeTemplate() {
        apply(document: GraphEditorDocument(
            schema: "opensks.graph-editor-document.v1",
            id: "single-model-safe",
            name: "Single Model Safe",
            nodes: [
                GraphEditorNode(id: "goal", kind: "goal_input", title: "Goal input", inputType: nil, outputType: "control"),
                GraphEditorNode(id: "delegate", kind: "delegate", title: "Delegate to model", inputType: "control", outputType: "control"),
                GraphEditorNode(id: "seal", kind: "final_seal", title: "Final seal", inputType: "control", outputType: nil)
            ],
            edges: [
                GraphEditorEdge(id: "edge-goal-delegate", fromNodeId: "goal", toNodeId: "delegate", portType: "control"),
                GraphEditorEdge(id: "edge-delegate-seal", fromNodeId: "delegate", toNodeId: "seal", portType: "control")
            ],
            runTemplateId: "single-model-safe"
        ))
    }

    func addNode(_ node: GraphEditorNode) {
        saveUndo()
        nodes.append(node)
        redoStack.removeAll()
        problems = compileProblems(nodes: nodes, edges: edges)
    }

    func connect(_ edge: GraphEditorEdge) {
        saveUndo()
        edges.append(edge)
        redoStack.removeAll()
        problems = compileProblems(nodes: nodes, edges: edges)
    }

    func visibleNodes(limit: Int) -> [GraphEditorNode] {
        Array(nodes.prefix(max(0, limit)))
    }

    func currentDocument() -> GraphEditorDocument {
        GraphEditorDocument(
            schema: "opensks.graph-editor-document.v1",
            id: documentId,
            name: documentName,
            nodes: nodes,
            edges: edges,
            runTemplateId: canRunDaemonTemplate ? documentId : "single-model-safe"
        )
    }

    @discardableResult
    func saveCurrentDocument(workspace: URL) throws -> URL {
        let url = editorDocumentURL(workspace: workspace)
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        let data = try JSONEncoder.opensks.encode(currentDocument())
        try data.write(to: url, options: [.atomic])
        let graphURL = try exportPipelineGraph(workspace: workspace)
        lastSavedPath = url.path
        lastExportedGraphPath = graphURL.path
        return url
    }

    @discardableResult
    func exportPipelineGraph(workspace: URL) throws -> URL {
        let url = exportedGraphURL(workspace: workspace)
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        let data = try JSONEncoder.opensks.encode(currentPipelineGraph())
        try data.write(to: url, options: [.atomic])
        lastExportedGraphPath = url.path
        return url
    }

    @discardableResult
    func loadSavedDocument(workspace: URL) throws -> GraphEditorDocument {
        let url = editorDocumentURL(workspace: workspace)
        let data = try Data(contentsOf: url)
        let document = try JSONDecoder.opensks.decode(GraphEditorDocument.self, from: data)
        apply(document: document)
        lastLoadedPath = url.path
        return document
    }

    func undo() {
        guard let previous = undoStack.popLast() else { return }
        redoStack.append((nodes, edges))
        nodes = previous.0
        edges = previous.1
        problems = compileProblems(nodes: nodes, edges: edges)
    }

    func redo() {
        guard let next = redoStack.popLast() else { return }
        undoStack.append((nodes, edges))
        nodes = next.0
        edges = next.1
        problems = compileProblems(nodes: nodes, edges: edges)
    }

    private func apply(document: GraphEditorDocument) {
        documentId = document.id
        documentName = document.name
        nodes = document.nodes
        edges = document.edges
        problems = compileProblems(nodes: nodes, edges: edges)
        undoStack.removeAll()
        redoStack.removeAll()
    }

    private func editorDocumentURL(workspace: URL) -> URL {
        workspace
            .appendingPathComponent(".opensks", isDirectory: true)
            .appendingPathComponent("pipelines", isDirectory: true)
            .appendingPathComponent("editor", isDirectory: true)
            .appendingPathComponent("current.graph-editor.json")
    }

    private func exportedGraphURL(workspace: URL) -> URL {
        workspace
            .appendingPathComponent(".opensks", isDirectory: true)
            .appendingPathComponent("pipelines", isDirectory: true)
            .appendingPathComponent("editor", isDirectory: true)
            .appendingPathComponent("current.graph.json")
    }

    private func currentPipelineGraph() -> PipelineGraphDTO {
        let incoming = Dictionary(grouping: edges, by: \.toNodeId)
        let entryNodes = nodes
            .filter { incoming[$0.id, default: []].isEmpty }
            .map(\.id)
        let nodePairs = nodes.enumerated().map { index, node in
            (
                node.id,
                PipelineNodeDTO(
                    id: node.id,
                    kind: node.kind,
                    displayName: node.title,
                    enabled: true,
                    position: PipelineGraphPointDTO(
                        x: Double(index * 220),
                        y: Double((index % 2) * 120)
                    ),
                    inputs: [:],
                    config: [:],
                    retry: PipelineRetryPolicyDTO(
                        maxAttempts: 1,
                        backoffMs: 0,
                        retryableReasonCodes: []
                    ),
                    timeoutMs: nil,
                    approval: PipelineApprovalPolicyDTO(required: false, scope: "none"),
                    hookRefs: []
                )
            )
        }
        let pipelineEdges = edges.map { edge in
            PipelineEdgeDTO(
                id: edge.id,
                from: PipelinePortRefDTO(nodeId: edge.fromNodeId, port: "out"),
                to: PipelinePortRefDTO(nodeId: edge.toNodeId, port: "in"),
                kind: edge.portType == "control" ? "control" : "data"
            )
        }
        return PipelineGraphDTO(
            schema: "opensks.pipeline-graph.v1",
            id: documentId,
            name: documentName,
            version: 1,
            entryNodes: entryNodes.isEmpty ? nodes.prefix(1).map(\.id) : entryNodes,
            nodes: Dictionary(uniqueKeysWithValues: nodePairs),
            edges: pipelineEdges,
            variables: [:],
            policies: PipelineGraphPoliciesDTO(
                maxParallelism: 1,
                allowExternalSideEffects: false,
                finalSealRequired: true
            ),
            metadata: PipelineGraphMetadataDTO(
                description: "Graph exported from OpenSKS Graph Editor.",
                createdBy: "opensks-studio",
                evidenceRefs: ["studio:graph-editor-export"]
            )
        )
    }

    private func saveUndo() {
        undoStack.append((nodes, edges))
        if undoStack.count > 50 { undoStack.removeFirst() }
    }

    private func compileProblems(nodes: [GraphEditorNode], edges: [GraphEditorEdge]) -> [GraphCompileProblem] {
        var problems: [GraphCompileProblem] = []
        for edge in edges {
            guard let from = nodes.first(where: { $0.id == edge.fromNodeId }),
                  let to = nodes.first(where: { $0.id == edge.toNodeId }) else {
                problems.append(GraphCompileProblem(id: "missing-\(edge.id)", nodeId: nil, message: "Missing edge endpoint"))
                continue
            }
            if let out = from.outputType, let input = to.inputType, out != input || edge.portType != input {
                problems.append(GraphCompileProblem(id: "type-\(edge.id)", nodeId: to.id, message: "Typed port mismatch"))
            }
        }
        for node in nodes {
            if !Self.supportedNodeKinds.contains(node.kind) {
                problems.append(GraphCompileProblem(id: "kind-\(node.id)", nodeId: node.id, message: "Unsupported graph node kind"))
            }
            if Self.sideEffectNodeKinds.contains(node.kind) {
                problems.append(GraphCompileProblem(id: "approval-\(node.id)", nodeId: node.id, message: "Side-effect node requires approval policy"))
            }
        }
        if nodes.filter({ $0.kind == "final_seal" }).isEmpty {
            problems.append(GraphCompileProblem(id: "missing-final-seal", nodeId: nil, message: "FinalSeal node required"))
        }
        return problems
    }
}
