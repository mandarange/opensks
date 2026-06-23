# Runtime Truth Matrix (generated)

<!-- GENERATED FILE — do not edit by hand.
     Regenerate with: cargo run -p xtask -- capability-matrix
     Source of truth: runtime capability report (contracts DTO + runtime registry evidence) -->

Each coding-agent capability declares how real it is at runtime. The app must never
present a `Foundation`/`Simulation` surface as if it were `Live` (recovery directive §18).

| Capability | Surface | Maturity | User label | Available | Reason | Evidence |
|---|---|---|---|:--:|---|---|
| `chat.answer` | Chat assistant answer | Foundation | Needs setup | no | `model_credentials_missing_for_live_chat_answer` | runtime:capability-registry, adapter:openrouter-native-http |
| `agent.code_edit` | Chat code edit | Foundation | Needs setup | no | `agentic_loop_toolgateway_patch_engine_need_live_provider_credentials` | crate:opensks-adapter, crate:opensks-patch-engine, toolgateway:policy-enforced, patch-engine:fsynced-transaction-journal, patch-engine:transactional-delete-rename, driver:openrouter-tools, driver:provider-failure-terminal |
| `agent.parallel_build` | Parallel subcontract build | Foundation | Needs setup | no | `scheduler_present_but_sync_deterministic_worker` | — |
| `model.dispatch` | Model provider dispatch | Foundation | Needs setup | no | `openrouter_secret_missing` | provider:openrouter-native-reqwest, registry:runtime-overlay |
| `agent.local_test_edit` | Local test agent file edit | Unavailable | Unavailable | no | `simulation_feature_disabled_for_release_build` | build:simulation-feature-disabled |
| `image.generate` | Image generation | Foundation | Needs setup | no | `fake_image_model_no_adapter` | — |
| `web.research` | Web research tool | Unavailable | Unavailable | no | `no_web_tool_implementation` | — |
| `conversation.persistence` | Conversation persistence | Live | Available | yes | `durable_sqlite_repository` | crate:opensks-conversation, table:conversations |
| `file.edit_manual` | Manual file editing | Live | Available | yes | `safe_file_service_with_optimistic_concurrency` | crate:opensks-file-service, schema:save-text-result |
| `git.commit` | Git commit | Live | Available | yes | `reviewed_index_hash_commit_path` | crate:opensks-git-service, schema:git-commit |
| `git.push` | Git push | Degraded | Limited | yes | `protected_push_outbox_local_remote_proof_only` | crate:opensks-git-service, test:push_cli_full_handshake_pushes_to_local_bare_remote_only |
| `stream.protocol` | Engine stream protocol | Degraded | Limited | yes | `daemon_ndjson_explicit_terminal_protocol_v2_missing` | daemon:request_completed, swift:explicit-terminal-router, test:request_response_ends_with_an_explicit_terminal_marker |
| `pipeline.graph` | Live pipeline graph | Foundation | Needs setup | no | `timeline_read_model_no_live_event_stream_projection` | swift:pipeline-projection-ingest, conversation:timeline-read-model, swift:conversation-timeline-read-model |
| `design.generation` | Design system generation | Foundation | Needs setup | no | `studio_scaffold_without_persist_compile_apply` | — |
