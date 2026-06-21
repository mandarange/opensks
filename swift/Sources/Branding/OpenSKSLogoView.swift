// OpenSKSLogoView.swift — renders the canonical OpenSKS logo from the bundled
// brand asset. Used in the title bar, welcome, and About. No SF Symbol / gradient
// substitute (see DESIGN.md "Forbidden Patterns").

import SwiftUI

struct OpenSKSLogoView: View {
    var size: CGFloat

    var body: some View {
        Group {
            if let image = BrandAssetLoader.logoImage() {
                Image(nsImage: image)
                    .resizable()
                    .interpolation(.high)
                    .scaledToFit()
            } else {
                Color.clear
            }
        }
        .frame(width: size, height: size)
        .accessibilityLabel("OpenSKS")
    }
}
