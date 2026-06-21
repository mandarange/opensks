import Foundation

struct IntelligenceRecord: Identifiable, Equatable, Sendable {
    let id: String
    let kind: String
    let title: String
    let path: String?
    let summary: String
}

@MainActor
final class ProjectIntelligenceStore: ObservableObject {
    @Published private(set) var records: [IntelligenceRecord] = []
    @Published private(set) var freshness = "stale"

    func load(records: [IntelligenceRecord], freshness: String) {
        self.records = records.sorted { $0.title < $1.title }
        self.freshness = freshness
    }

    func visibleRecords(limit: Int) -> [IntelligenceRecord] {
        Array(records.prefix(max(0, limit)))
    }

    func recordCount(kind: String) -> Int {
        records.filter { $0.kind == kind }.count
    }

    func sourcePath(for id: String) -> String? {
        records.first { $0.id == id }?.path
    }

    var freshnessLabel: String {
        freshness == "fresh" ? "Fresh" : "Stale"
    }
}
