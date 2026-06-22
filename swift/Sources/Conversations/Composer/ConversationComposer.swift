// ConversationComposer.swift — the ONLY primary send path for a conversation
// (PR-027). A multiline text field bound to the per-conversation draft plus a
// single primary Send button. Send is disabled when the trimmed draft is empty
// or a send is already in flight, so one Send starts exactly one deterministic
// engine run. The whole button tile is the hit target (SurfaceButtonStyle
// .primaryAction). This composer does NOT call the legacy engine-run path.

import SwiftUI

struct ConversationComposer: View {
    @ObservedObject var store: ConversationStore
    let conversationID: String

    /// Draft text bound through the store so it survives selection changes and
    /// is cleared on a successful send.
    private var draftBinding: Binding<String> {
        Binding(
            get: { store.draft(for: conversationID) },
            set: { store.setDraft($0, for: conversationID) }
        )
    }

    private var trimmedDraft: String {
        store.draft(for: conversationID).trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var canSend: Bool {
        !trimmedDraft.isEmpty && !store.isSending
    }

    var body: some View {
        VStack(spacing: 0) {
            Divider().overlay(Theme.stroke)
            HStack(alignment: .bottom, spacing: 10) {
                TextField("Message the engine…", text: draftBinding, axis: .vertical)
                    .textFieldStyle(.plain)
                    .font(Theme.ui(13))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1...6)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
                    .background(
                        RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                            .fill(Theme.input)
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                            .strokeBorder(Theme.stroke, lineWidth: 1)
                    )
                    .accessibilityIdentifier("conversation.composer.field")

                Button(action: send) {
                    Label("Send", systemImage: "paperplane.fill")
                        .labelStyle(.titleAndIcon)
                }
                .buttonStyle(.primaryAction)
                .frame(width: 110)
                .disabled(!canSend)
                .accessibilityIdentifier("conversation.composer.send")
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 12)
        }
        .background(Theme.bg)
    }

    private func send() {
        let text = store.draft(for: conversationID)
        Task { await store.send(conversationID: conversationID, text: text) }
    }
}
