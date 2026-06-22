import XCTest
@testable import OpenSKSStudio

final class PluralizeTests: XCTestCase {
    func testCountPluralisesOnlyWhenNotOne() {
        XCTAssertEqual(Pluralize.count(0, "provider"), "0 providers")
        XCTAssertEqual(Pluralize.count(1, "provider"), "1 provider")
        XCTAssertEqual(Pluralize.count(3, "provider"), "3 providers")
    }

    func testCountUsesExplicitPluralWhenGiven() {
        XCTAssertEqual(Pluralize.count(1, "entry", "entries"), "1 entry")
        XCTAssertEqual(Pluralize.count(2, "entry", "entries"), "2 entries")
    }
}
