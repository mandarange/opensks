// AppCoordinator.swift — owns cross-cutting UI stores and is the seam for
// decomposing the legacy AppState God object in later PRs. PR-022 introduces it
// owning navigation; subsequent PRs migrate conversation / run / editor / git /
// design stores here off AppState.

import SwiftUI

@MainActor
final class AppCoordinator: ObservableObject {
    let navigation = NavigationStore()

    /// Conversation sidebar + thread store (PR-025). It starts with a live
    /// service rooted at the process working directory; once the real workspace
    /// path + bundled CLI are resolved (RootView.onAppear reads `AppState`), the
    /// service is rebound via `bindConversations(cli:workspace:)`.
    let conversations: ConversationStore

    /// Node-level pipeline projections keyed by run id (PR-029). The `.graph`
    /// route and the conversation thread's `PipelineRunCard`s both read live
    /// projections from here. Multiple concurrent runs coexist (one reducer per
    /// run id), so switching the selected run shows that run's nodes.
    let pipelines = PipelineProjectionStore()

    /// The run whose live graph the `.graph` route renders. Set when an operator
    /// opens a run's graph (e.g. from a `PipelineRunCard`'s "Open live graph").
    @Published var activeGraphRunId: String?

    /// The READ-ONLY Git studio store (PR-034). Starts with a live service rooted
    /// at the process working directory; rebound to the resolved workspace +
    /// bundled CLI via `bindGit(cli:workspace:)` once `AppState` resolves them.
    let git: GitStudioStore

    /// The LOCAL design-import store (PR-039). Drives the quarantine → human-review
    /// → promote flow. Starts with a live service rooted at the process working
    /// directory; rebound to the resolved workspace + bundled CLI via
    /// `bindDesignImport(cli:workspace:)` once `AppState` resolves them.
    let designImport: DesignImportStore

    /// The Design Studio store (PR-040). Drives the catalog → tokens / components /
    /// audit / revisions surface, with ATOMIC activation (a failing audit blocks it
    /// and keeps the previous active package). Starts with a live service rooted at
    /// the process working directory; rebound to the resolved workspace + bundled CLI
    /// via `bindDesignStudio(cli:workspace:)` once `AppState` resolves them.
    let designStudio: DesignStudioStore

    /// The Project Intelligence store (PR-041). Drives the architecture / code
    /// graph / glossary surface with per-section freshness badges. Starts with a
    /// live service rooted at the process working directory; rebound to the
    /// resolved workspace + bundled CLI via `bindIntelligence(cli:workspace:)`
    /// once `AppState` resolves them.
    let intelligence: IntelligenceStore

    /// The encrypted-vault + provenance store (PR-042). Drives the opt-in
    /// sanitized-summary export, the opt-in full-transcript encryption, and the
    /// identity-gated import. Holds NO transcript bytes and NO key material — it
    /// persists only the PUBLIC recipient via the Keychain. Starts with a live
    /// service rooted at the process working directory; rebound to the resolved
    /// workspace + bundled CLI via `bindVault(cli:workspace:)` once `AppState`
    /// resolves them.
    let vault: VaultStore

    /// Editor store reference (wired by `wireGit`/`wireIntelligence`) so an
    /// intelligence deep link to a file can open it in the code workspace.
    private weak var editorStore: EditorWorkspaceStore?

    /// Process-lifetime memory-pressure monitor (PR-043). On a WARNING it shrinks
    /// bounded caches + releases backgrounded heavy views; on CRITICAL it purges
    /// them. The conversation + pipeline stores register their background-release
    /// here so only the foreground view survives a pressure event.
    let memoryPressure = MemoryPressureMonitor()

    /// A bounded LRU cache for rendered thumbnails / pages (PR-043). Capacity caps
    /// live entries; the monitor shrinks it on warning and purges on critical.
    let renderCache = BoundedCache<String, Data>(capacity: 256)

    init() {
        let cwd = URL(fileURLWithPath: FileManager.default.currentDirectoryPath, isDirectory: true)
        let cli = cwd.appendingPathComponent("target/debug/opensks")
        conversations = ConversationStore(
            service: LiveConversationService(cli: cli, workspace: cwd)
        )
        git = GitStudioStore(
            service: LiveGitService(cli: cli, workspace: cwd)
        )
        designImport = DesignImportStore(
            service: LiveDesignImportService(cli: cli, workspace: cwd)
        )
        designStudio = DesignStudioStore(
            service: LiveDesignStudioService(cli: cli, workspace: cwd),
            catalog: AppCoordinator.seedDesignCatalog()
        )
        intelligence = IntelligenceStore(
            service: LiveIntelligenceService(cli: cli, workspace: cwd)
        )
        vault = VaultStore(
            service: LiveVaultService(cli: cli, workspace: cwd),
            recipientStore: KeychainVaultRecipientStore()
        )

        // PR-043: wire memory-pressure reclaim. The render cache shrinks/purges,
        // and the conversation + pipeline stores release their backgrounded heavy
        // views (full thread page / full node projection) so only the foreground
        // view survives. Then start listening to the OS for real pressure events.
        memoryPressure.register(cache: renderCache)
        conversations.registerForMemoryPressure(memoryPressure)
        pipelines.registerForMemoryPressure(memoryPressure)
        memoryPressure.start()
    }

    /// Rebind the conversation store's live service to the resolved workspace and
    /// bundled CLI (same values `AppState` uses), then reload.
    func bindConversations(cli: URL, workspace: URL) {
        conversations.updateService(
            LiveConversationService(cli: cli, workspace: workspace)
        )
        Task { await conversations.load() }
    }

    /// Rebind the Git studio to the resolved workspace + bundled CLI and refresh.
    func bindGit(cli: URL, workspace: URL) {
        git.rebind(service: LiveGitService(cli: cli, workspace: workspace))
    }

    /// Rebind the LOCAL design-import store to the resolved workspace + bundled CLI
    /// and re-read the quarantine listing.
    func bindDesignImport(cli: URL, workspace: URL) {
        designImport.rebind(service: LiveDesignImportService(cli: cli, workspace: workspace))
    }

    /// Rebind the Design Studio store to the resolved workspace + bundled CLI,
    /// re-read the active design package, and load the registry-driven catalog
    /// (DESIGN-101) so the sidebar reflects the packages actually on disk.
    func bindDesignStudio(cli: URL, workspace: URL) {
        designStudio.rebind(service: LiveDesignStudioService(cli: cli, workspace: workspace))
        Task { await designStudio.loadRegistryCatalog() }
    }

    /// Rebind the Project Intelligence store to the resolved workspace + bundled CLI
    /// and reload its sections (architecture / code graph / glossary).
    func bindIntelligence(cli: URL, workspace: URL) {
        intelligence.updateService(LiveIntelligenceService(cli: cli, workspace: workspace))
        Task { await intelligence.loadAll() }
    }

    /// Rebind the Vault store (PR-042) to the resolved workspace + bundled CLI and
    /// re-read the workspace inventory (summaries + redacted vaults). The configured
    /// PUBLIC recipient persists across rebinds (it lives in the Keychain, not the
    /// service).
    func bindVault(cli: URL, workspace: URL) {
        vault.rebind(service: LiveVaultService(cli: cli, workspace: workspace))
    }

    /// Navigate an Intelligence deep link onto the EXISTING routes (no new route is
    /// invented, none removed): a conversation ref → the `.chat` thread (selecting
    /// the conversation), a run ref → the `.graph` route focused on that run, a file
    /// ref → the `.code` editor opening the file. The target's id is the source of
    /// truth so a record/result lands on exactly the right surface.
    func openIntelTarget(_ target: IntelDeepLinkTarget) {
        switch target {
        case .conversation(let id):
            conversations.selectedConversationID = id
            Task { await conversations.select(id) }
            navigation.route = .chat
        case .run(let id):
            openGraph(runId: id)
        case .file(let path, _):
            if let editorStore {
                Task { _ = await editorStore.open(path: path) }
            }
            navigation.route = .code
        }
    }

    /// The catalog of design packages shown in the Design Studio sidebar. Seeded
    /// with the canonical live package (`opensks-studio-dark`) whose tokens drive
    /// the app, so the Tokens tab has real content. Additional promoted packages
    /// arrive once the registry listing is wired.
    static func seedDesignCatalog() -> [DesignPackage] {
        [
            DesignPackage(
                packageId: "opensks-studio-dark",
                title: "OpenSKS Studio (Dark)",
                tokens: liveStudioDarkTokens()
            )
        ]
    }

    /// The live `opensks-studio-dark` token paths/values, read from the generated
    /// tokens so the editor reflects the compiled source of truth.
    private static func liveStudioDarkTokens() -> [DesignTokenEntry] {
        [
            DesignTokenEntry(path: "color.canvas", value: "#0E1015"),
            DesignTokenEntry(path: "color.surface.base", value: "#13161B"),
            DesignTokenEntry(path: "color.surface.sidebar", value: "#101216"),
            DesignTokenEntry(path: "color.surface.raised", value: "#181B21"),
            DesignTokenEntry(path: "color.border.subtle", value: "#262A32"),
            DesignTokenEntry(path: "color.border.strong", value: "#2C313A"),
            DesignTokenEntry(path: "color.focus", value: "#5EDEC4"),
            DesignTokenEntry(path: "color.text.primary", value: "#E9EDF3"),
            DesignTokenEntry(path: "color.text.secondary", value: "#BCC4D0"),
            DesignTokenEntry(path: "color.text.muted", value: "#7E8796"),
            DesignTokenEntry(path: "color.accent.primary", value: "#5EDEC4"),
            DesignTokenEntry(path: "color.accent.secondary", value: "#9D8EF5"),
            DesignTokenEntry(path: "color.status.success", value: "#5EDEC4"),
            DesignTokenEntry(path: "color.status.warning", value: "#E0B25C"),
            DesignTokenEntry(path: "color.status.danger", value: "#E0876E"),
            DesignTokenEntry(path: "color.status.running", value: "#70B0F4"),
            DesignTokenEntry(path: "radius.control", value: "9"),
            DesignTokenEntry(path: "radius.card", value: "12")
        ]
    }

    /// Wire the Git studio (PR-035 + PR-036) to the rest of the app: the editor
    /// store so a dirty-buffer switch preflight can see unsaved work, a commit-card
    /// sink so a successful LOCAL commit posts a receipt into the active
    /// conversation thread, and a push-card sink so a successful APPROVED push
    /// posts a SEPARATE push receipt. Idempotent — safe to call again after a
    /// rebind.
    func wireGit(editorStore: EditorWorkspaceStore) {
        git.editorStore = editorStore
        // Stash the editor store so an Intelligence deep link to a file can open it.
        self.editorStore = editorStore
        git.onCommitted = { [weak self] result, message in
            self?.conversations.postCommitCard(result, message: message)
        }
        git.onPushed = { [weak self] receipt, intent in
            self?.conversations.postPushCard(receipt, intent: intent)
        }
    }

    /// Focus the `.graph` route on a specific run and navigate there. Used by a
    /// `PipelineRunCard`'s "Open live graph" control. Selecting a different run
    /// id swaps the projection the graph renders without disturbing other runs'
    /// state in the store.
    func openGraph(runId: String) {
        activeGraphRunId = runId
        navigation.route = .graph
    }
}
