import SwiftUI

struct SecureCredentialField: View {
    @Binding var credential: String
    var title = "API key"
    var footer = "Stored in Keychain. The provider registry only keeps a secret reference."

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s6) {
            SecureField(title, text: $credential)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("providers.wizard.credential")
            Label(footer, systemImage: "lock.shield")
                .font(Theme.ui(11))
                .foregroundStyle(Theme.muted)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier("providers.wizard.credential.footer")
        }
    }
}
