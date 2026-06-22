import SwiftUI
import XCTest
@testable import OpenSKSStudio

@MainActor
final class InteractionTests: XCTestCase {
    /// Hit-area: a primary surface button renders at least its declared min
    /// height, so padded/background regions are part of the tappable tile.
    func testSurfaceButtonRendersAtLeastMinHeight() throws {
        let button = Button("Run") {}
            .buttonStyle(SurfaceButtonStyle(emphasis: .primary, minHeight: 40))
            .frame(width: 220)
        let image = try XCTUnwrap(ImageRenderer(content: button).nsImage)
        XCTAssertGreaterThanOrEqual(image.size.height, 39.5, "primary button must be >=40pt tall")
    }

    func testSurfaceEmphasisExposesFourFactories() {
        let styles: [SurfaceButtonStyle] = [.primaryAction, .secondaryAction, .quietAction, .destructiveAction]
        XCTAssertEqual(styles.count, 4)
        XCTAssertEqual(SurfaceButtonStyle.quietAction.minHeight, 36)
    }

    func testStatusPillRenders() throws {
        let pill = StatusPill(kind: .success, label: "Ready")
        XCTAssertNotNil(ImageRenderer(content: pill).nsImage)
    }

    func testEmptyStateRenders() throws {
        let view = EmptyStateView(headline: "Empty", detail: "Nothing here yet", systemImage: "tray")
            .frame(width: 400, height: 300)
        XCTAssertNotNil(ImageRenderer(content: view).nsImage)
    }
}
