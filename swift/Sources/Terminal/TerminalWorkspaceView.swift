import SwiftUI

struct TerminalWorkspaceView: View {
    @EnvironmentObject private var state: AppState
    @StateObject private var viewModel = TerminalViewModel()

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider().overlay(Theme.stroke)
            content
            Divider().overlay(Theme.stroke)
            inputBar
        }
        .background(Theme.bg)
        .onAppear {
            viewModel.configure(
                client: state.makeTerminalDaemonClient(),
                cwd: state.workspace.path
            )
            if viewModel.session.status == .disconnected {
                viewModel.startSession()
            }
        }
        .confirmationDialog(
            "This command requires approval before execution.",
            isPresented: approvalBinding,
            titleVisibility: .visible
        ) {
            Button("Approve and Insert") { viewModel.approvePendingInsert() }
            Button("Cancel", role: .cancel) { viewModel.cancelPendingApproval() }
        } message: {
            if let pending = viewModel.pendingApproval {
                Text("\(pending.display)\nRisk: \(pending.risk.displayLabel)")
            }
        }
    }

    private var approvalBinding: Binding<Bool> {
        Binding(
            get: { viewModel.pendingApproval != nil },
            set: { if !$0 { viewModel.cancelPendingApproval() } }
        )
    }

    private var header: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Terminal")
                    .font(Theme.ui(18, .semibold))
                    .foregroundStyle(Theme.text)
                HStack(spacing: 12) {
                    metadata("cwd", viewModel.session.cwd)
                    metadata("shell", viewModel.session.shell)
                }
            }
            Spacer()
            StatusPill(kind: viewModel.daemonStatus.pillKind, label: "daemon: \(viewModel.daemonStatus.label)")
            if viewModel.session.status == .running {
                Button {
                    viewModel.stopSession()
                } label: {
                    Label("Stop", systemImage: "stop.fill")
                }
                .buttonStyle(TerminalHeaderButtonStyle())
            } else {
                Button {
                    viewModel.startSession()
                } label: {
                    Label("Start", systemImage: "play.fill")
                }
                .buttonStyle(TerminalHeaderButtonStyle())
            }
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 14)
        .background(Theme.panel)
    }

    private func metadata(_ label: String, _ value: String) -> some View {
        HStack(spacing: 5) {
            Text("\(label):")
                .font(Theme.mono(10.5, .semibold))
                .foregroundStyle(Theme.faint)
            Text(value)
                .font(Theme.mono(10.5))
                .foregroundStyle(Theme.textSoft)
                .lineLimit(1)
                .truncationMode(.middle)
        }
    }

    private var content: some View {
        ScrollViewReader { proxy in
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    if viewModel.blocks.isEmpty && viewModel.suggestions.isEmpty && viewModel.agentMessages.isEmpty {
                        emptyState
                    }
                    ForEach(viewModel.blocks) { block in
                        TerminalCommandBlockView(block: block)
                            .id(block.id)
                    }
                    if !viewModel.suggestions.isEmpty {
                        suggestionList
                    }
                    if !viewModel.agentMessages.isEmpty {
                        agentMessageList
                    }
                    if let lastError = viewModel.lastError {
                        errorState(lastError)
                    }
                    Color.clear.frame(height: 1).id("terminal-bottom")
                }
                .padding(18)
            }
            .onChange(of: viewModel.blocks.count) { _ in
                withAnimation(.linear(duration: 0.1)) {
                    proxy.scrollTo("terminal-bottom", anchor: .bottom)
                }
            }
        }
    }

    private var emptyState: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(emptyStateTitle)
                .font(Theme.ui(14, .semibold))
                .foregroundStyle(Theme.text)
            Text(emptyStateDetail)
                .font(Theme.ui(12))
                .foregroundStyle(Theme.textSoft)
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .fill(Theme.panelDeep)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .strokeBorder(Theme.stroke)
        )
    }

    private var emptyStateTitle: String {
        switch viewModel.daemonStatus {
        case .unsupportedPlatform:
            return "PTY terminal runtime is not supported on this platform yet."
        case .unavailable:
            return "Terminal daemon is not connected."
        case .providerUnavailable:
            return "AI command proposals are not connected yet."
        default:
            return "Terminal session ready."
        }
    }

    private var emptyStateDetail: String {
        switch viewModel.daemonStatus {
        case .unavailable:
            return "Run `cargo run -- terminal smoke` to verify the local runtime."
        case .providerUnavailable:
            return "Deterministic suggestions are available."
        default:
            return viewModel.session.cwd
        }
    }

    private var suggestionList: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(viewModel.suggestions) { suggestion in
                TerminalSuggestionView(
                    suggestion: suggestion,
                    onInsert: { viewModel.insertSuggestion(suggestion) },
                    onRun: { viewModel.runSuggestion(suggestion) },
                    onExplain: { viewModel.explainSuggestion(suggestion) }
                )
            }
        }
    }

    private var agentMessageList: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(viewModel.agentMessages) { message in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Image(systemName: message.isError ? "exclamationmark.triangle.fill" : "sparkles")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(message.isError ? Theme.coral : Theme.violet)
                    Text(message.text)
                        .font(Theme.ui(12))
                        .foregroundStyle(message.isError ? Theme.coral : Theme.textSoft)
                        .textSelection(.enabled)
                    Spacer()
                }
                .padding(.vertical, 3)
            }
        }
    }

    private func errorState(_ message: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(Theme.gold)
            Text(message)
                .font(Theme.ui(12))
                .foregroundStyle(Theme.textSoft)
                .textSelection(.enabled)
            Spacer()
        }
        .padding(12)
        .background(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .fill(Theme.gold.opacity(0.08))
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .strokeBorder(Theme.gold.opacity(0.28))
        )
    }

    private var inputBar: some View {
        HStack(spacing: 10) {
            Text("input:")
                .font(Theme.mono(11.5, .semibold))
                .foregroundStyle(Theme.faint)
            TextField("", text: $viewModel.input)
                .textFieldStyle(.plain)
                .font(Theme.mono(12.5))
                .foregroundStyle(Theme.text)
                .onSubmit { viewModel.submitInput() }
                .onChange(of: viewModel.input) { _ in
                    viewModel.scheduleSuggestionRequest()
                }
            if let ghost = viewModel.ghostSuggestion {
                Text("ghost: \(ghost.display)")
                    .font(Theme.mono(11.5))
                    .foregroundStyle(Theme.faint)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Button {
                viewModel.submitInput()
            } label: {
                Image(systemName: "return")
            }
            .buttonStyle(TerminalIconButtonStyle())
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(Theme.terminal)
        .overlay(alignment: .top) {
            Rectangle().fill(Theme.stroke).frame(height: 1)
        }
        .background(acceptSuggestionShortcuts)
    }

    private var acceptSuggestionShortcuts: some View {
        ZStack {
            Button("") { viewModel.acceptGhostSuggestion() }
                .keyboardShortcut(.tab, modifiers: [])
            Button("") { viewModel.acceptGhostSuggestion() }
                .keyboardShortcut(.rightArrow, modifiers: [])
        }
        .buttonStyle(.plain)
        .opacity(0)
        .frame(width: 0, height: 0)
        .accessibilityHidden(true)
    }
}

private struct TerminalHeaderButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(Theme.ui(11.5, .semibold))
            .foregroundStyle(Theme.text)
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(
                RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                    .fill(configuration.isPressed ? Theme.accentTint : Theme.input)
            )
            .overlay(
                RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                    .strokeBorder(Theme.stroke)
            )
    }
}

private struct TerminalIconButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 12, weight: .bold))
            .foregroundStyle(configuration.isPressed ? Theme.accentInk : Theme.accent)
            .frame(width: 28, height: 26)
            .background(
                RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                    .fill(configuration.isPressed ? Theme.accent : Theme.input)
            )
            .overlay(
                RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                    .strokeBorder(Theme.stroke)
            )
    }
}
