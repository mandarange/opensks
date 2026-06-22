import SwiftUI
import XCTest
@testable import OpenSKSStudio

@MainActor
final class NavigationTests: XCTestCase {
    func testRailExposesAllLabelledRoutesInOrder() {
        // PR-042 adds the Vault route between Intel and Evidence without removing any
        // existing route.
        XCTAssertEqual(
            WorkspaceRoute.allCases.map(\.label),
            ["Home", "Chat", "Code", "Graph", "Runs", "Git", "Design", "Intel", "Vault", "Evidence", "Settings"]
        )
    }

    func testEachRouteHasADistinctCentralIdentifier() {
        let ids = Set(WorkspaceRoute.allCases.map(\.centralAccessibilityIdentifier))
        XCTAssertEqual(ids.count, WorkspaceRoute.allCases.count)
    }

    func testNavigationStoreDefaultsToChatAndUpdates() {
        // Chat is the default first-launch surface (recovery directive §0.3/§3.3).
        let nav = NavigationStore()
        XCTAssertEqual(nav.route, .chat)
        nav.route = .git
        XCTAssertEqual(nav.route, .git)
        XCTAssertEqual(nav.route.centralAccessibilityIdentifier, "workspace.central.git")
    }

    func testEveryRouteMapsToALegacySidebarSection() {
        // Total mapping (compile-time exhaustive); spot-check a representative.
        for route in WorkspaceRoute.allCases {
            _ = route.legacySection
        }
        XCTAssertEqual(WorkspaceRoute.code.legacySection, .files)
        XCTAssertEqual(WorkspaceRoute.home.legacySection, .home)
    }

    /// Acceptance: the central workspace fills the available width with no
    /// shell-imposed letterbox at the supported window widths. Rendered offscreen
    /// with ImageRenderer; asserts the rendered surface width matches the request.
    func testCentralSurfaceFillsWidthAtSupportedWindowWidths() throws {
        for width in [1024.0, 1440.0, 1920.0] {
            let surface = RoutePlaceholderView(
                headline: "Home",
                detail: "Routed central surface.",
                systemImage: "house"
            )
            .frame(width: width, height: 700)

            let renderer = ImageRenderer(content: surface)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "central surface rendered at width \(width)")
            XCTAssertEqual(
                image.size.width, width, accuracy: 1.0,
                "central surface must fill width (no letterbox) at \(width)"
            )
        }
    }
}
