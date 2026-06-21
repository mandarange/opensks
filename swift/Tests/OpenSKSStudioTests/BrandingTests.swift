import XCTest
@testable import OpenSKSStudio

final class BrandingTests: XCTestCase {
    func testCanonicalLogoResourceIsBundled() {
        // Acceptance (PR-021): a missing asset must fail here, never silently
        // fall back to a synthetic mark.
        XCTAssertNotNil(
            BrandAssetLoader.logoImage(),
            "OpenSKSLogo.png must be bundled in the OpenSKSStudio resources"
        )
    }

    func testGeneratedDesignTokensAreAvailable() {
        // The dark token bootstrap is generated from the opensks-studio-dark
        // design package and adopted by Theme as aliases.
        XCTAssertEqual(GeneratedDesignTokens.revision, 1)
        XCTAssertEqual(GeneratedDesignTokens.sizeHitTargetPrimary, 44)
        XCTAssertEqual(Theme.accent, GeneratedDesignTokens.colorAccentPrimary)
    }
}
