// BrandAssetLoader.swift — loads canonical brand assets from the package
// resource bundle. The logo is the OpenSKS mark rasterized from the tracked
// `assets/opensks-logo.svg` (the same SVG used for the macOS app icon). There is
// no synthetic fallback: a missing resource is a test failure, never a silently
// substituted mark.

import AppKit

enum BrandAssetLoader {
    private static let resourceBundleName = "OpenSKSStudio_OpenSKSStudio"

    /// The canonical OpenSKS logo, or `nil` if the bundled resource is missing.
    static func logoImage() -> NSImage? {
        for bundle in candidateResourceBundles() {
            if
                let url = bundle.url(forResource: "OpenSKSLogo", withExtension: "png"),
                let image = NSImage(contentsOf: url)
            {
                return image
            }
        }
        return nil
    }

    private static func candidateResourceBundles() -> [Bundle] {
        let bundleFileName = "\(resourceBundleName).bundle"
        let packagedCandidates = [
            Bundle.main.resourceURL?.appendingPathComponent(bundleFileName, isDirectory: true),
            Bundle.main.executableURL?
                .deletingLastPathComponent()
                .appendingPathComponent(bundleFileName, isDirectory: true),
        ]
        let bundles = packagedCandidates.compactMap { url -> Bundle? in
            guard let url else { return nil }
            return Bundle(url: url)
        }
        return bundles.isEmpty ? [Bundle.module] : bundles
    }
}
