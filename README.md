# OpenSKS

OpenSKS is a Rust-native autonomous coding OS prototype. The current vertical
slice implements the PRD v3 `/goal` foundation: a proof-first goal-loop intake
that writes mission artifacts, stop policy, tool plan, Voxel TriWiki seed data,
progress ledger, and a final seal.

## Usage

```bash
cargo run -- goal "Implement a bounded goal loop with Voxel TriWiki"
cargo run -- goal "Implement MCP browser QA" --mode naruto --max-waves 2
cargo run -- goal status <mission-id>
```

The CLI writes runtime artifacts under:

```text
.opensks/missions/<mission-id>/
```

Each mission currently includes:

```text
goal-loop.json
goal-state.jsonl
progress-ledger.json
stop-policy.json
tool-plan.json
voxel-triwiki.json
voxels.jsonl
final-seal.json
```

The final seal is intentionally marked `partial`: the MVP proves intake,
artifact writing, capability planning, and Voxel TriWiki seeding. Real worker
execution, repair waves, MCP broker execution, browser/computer/app control,
and full QA remain future implementation phases.
