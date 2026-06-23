# Runtime Truth Matrix (generated)

<!-- GENERATED FILE — do not edit by hand.
     Regenerate with: cargo run -p xtask -- capability-matrix
     Source of truth: opensks_contracts::baseline_capability_report() -->

Each coding-agent capability declares how real it is at runtime. The app must never
present a `Foundation`/`Simulation` surface as if it were `Live` (recovery directive §18).

| Capability | Surface | Maturity | User label | Available | Reason | Evidence |
|---|---|---|---|:--:|---|---|
| `chat.answer` | Chat assistant answer | Foundation | Needs setup | no | `real_answer_path_needs_model_configured` | crate:opensks-adapter, test:live_openrouter_returns_real_text |
| `agent.code_edit` | Chat code edit | Foundation | Needs setup | no | `agentic_loop_and_openrouter_tool_driver_present_need_model_credentials` | crate:opensks-adapter, loop:agentic, driver:openrouter-tools |
| `agent.parallel_build` | Parallel subcontract build | Foundation | Needs setup | no | `scheduler_present_but_sync_deterministic_worker` | — |
| `model.dispatch` | Model provider dispatch | Foundation | Needs setup | no | `openrouter_adapter_present_needs_api_key` | crate:opensks-adapter, adapter:openrouter |
| `agent.local_test_edit` | Local test agent file edit | Live | Available | yes | `deterministic_adapter_performs_real_file_io` | crate:opensks-adapter, test:local_test_adapter_really_edits_a_file_on_disk |
| `image.generate` | Image generation | Foundation | Needs setup | no | `fake_image_model_no_adapter` | — |
| `web.research` | Web research tool | Unavailable | Unavailable | no | `no_web_tool_implementation` | — |
| `conversation.persistence` | Conversation persistence | Live | Available | yes | `durable_sqlite_repository` | crate:opensks-conversation, table:conversations |
| `file.edit_manual` | Manual file editing | Live | Available | yes | `safe_file_service_with_optimistic_concurrency` | crate:opensks-file-service, schema:save-text-result |
| `git.commit` | Git commit | Live | Available | yes | `reviewed_index_hash_commit_path` | crate:opensks-git-service, schema:git-commit |
| `git.push` | Git push | Live | Available | yes | `protected_push_approval_outbox` | crate:opensks-git-service, schema:git-isolation |
| `stream.protocol` | Engine stream protocol | Degraded | Limited | yes | `swift_quiet_window_still_in_product_path` | crate:opensks-stream, schema:engine-stream-frame |
| `pipeline.graph` | Live pipeline graph | Foundation | Needs setup | no | `projection_present_no_ingest_or_edges` | — |
| `design.generation` | Design system generation | Foundation | Needs setup | no | `studio_scaffold_without_persist_compile_apply` | — |
