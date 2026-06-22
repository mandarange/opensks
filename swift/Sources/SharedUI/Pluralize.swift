// Pluralize.swift — count + noun formatting (P2 quality fix).
//
// The title bar (and other surfaces) showed "1 provider" / "3 provider" — singular
// regardless of count. This shared helper pluralises a count consistently so the
// recurring defect has one correct home.

import Foundation

enum Pluralize {
    /// "<n> <noun>" with the noun pluralised by adding "s" (or an explicit plural)
    /// when `n != 1`. Examples: `count(1, "provider") == "1 provider"`,
    /// `count(3, "provider") == "3 providers"`, `count(2, "entry", "entries")`.
    static func count(_ n: Int, _ singular: String, _ plural: String? = nil) -> String {
        let noun = n == 1 ? singular : (plural ?? singular + "s")
        return "\(n) \(noun)"
    }
}
