// BrandAssetLoader.swift — loads canonical brand assets from the package
// resource bundle. The logo is the OpenSKS mark rasterized from the tracked
// `assets/opensks-logo.svg` (the same SVG used for the macOS app icon). There is
// no synthetic fallback: a missing resource is a test failure, never a silently
// substituted mark.

import AppKit

enum BrandAssetLoader {
    /// The canonical OpenSKS logo, or `nil` if the bundled resource is missing.
    static func logoImage() -> NSImage? {
        guard
            let url = Bundle.module.url(forResource: "OpenSKSLogo", withExtension: "png"),
            let image = NSImage(contentsOf: url)
        else {
            return nil
        }
        return image
    }
}
