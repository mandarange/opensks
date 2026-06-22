// EmptyState.swift — a reusable empty / placeholder surface with an optional
// primary action. Routed placeholders and empty lists share this so empty
// states are consistent and always suggest a next step where one exists.

import SwiftUI

struct EmptyStateView: View {
    let headline: String
    let detail: String
    let systemImage: String
    var actionTitle: String? = nil
    var action: (() -> Void)? = nil

    var body: some View {
        VStack(spacing: Theme.s12) {
            Image(systemName: systemImage)
                .font(.system(size: 34, weight: .regular))
                .foregroundStyle(Theme.muted)
            Text(headline)
                .font(Theme.ui(18, .semibold))
                .foregroundStyle(Theme.text)
            Text(detail)
                .font(Theme.ui(12))
                .foregroundStyle(Theme.muted)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 420)
            if let actionTitle, let action {
                Button(action: action) { Text(actionTitle) }
                    .buttonStyle(.secondaryAction)
                    .frame(maxWidth: 240)
                    .padding(.top, 4)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(40)
    }
}
