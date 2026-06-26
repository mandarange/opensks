# Runtime Truth Matrix (generated)

<!-- GENERATED FILE — do not edit by hand.
     Regenerate with: cargo run -p xtask -- capability-matrix
     Source of truth: runtime capability report (contracts DTO + runtime registry evidence) -->

Each coding-agent capability declares how real it is at runtime. The app must never
present a `Foundation`/`Simulation` surface as if it were `Live` (recovery directive §18).

| Capability | Surface | Maturity | User label | Available | Reason | Evidence |
|---|---|---|---|:--:|---|---|
| `chat.answer` | Chat assistant answer | Foundation | Needs setup | no | `model_credentials_missing_for_live_chat_answer` | runtime:capability-registry, adapter:openrouter-native-http |
| `agent.code_edit` | Chat code edit | Foundation | Needs setup | no | `agentic_loop_toolgateway_patch_engine_need_live_provider_credentials` | crate:opensks-adapter, crate:opensks-patch-engine, toolgateway:policy-enforced, adapter:request-patch-lease, scheduler:lease-visible-to-worker, daemon:turn-scheduler-worker-route, patch-engine:typed-preflight-read, patch-engine:pre-apply-revalidated, patch-engine:path-lease-bound, patch-engine:fence-token-bound, patch-engine:stale-temp-scavenger, patch-engine:rollback-fault-injected, patch-engine:attempt-aware-recovery, patch-engine:read-back-verified, patch-engine:fsynced-transaction-journal, patch-engine:transactional-delete-rename, driver:openrouter-tools, driver:provider-failure-terminal |
| `agent.parallel_build` | Parallel subcontract build | Foundation | Needs setup | no | `objective_plan_live_model_planner_apply_seal_runtime_present_live_vendor_pending` | crate:opensks-scheduler, planner:shard-policy, scheduler:objective-plan-turn-bootstrap, daemon:objective-plan-live-model-planner, daemon:objective-plan-artifact, daemon:objective-plan-child-runtime, daemon:objective-plan-apply-runtime, daemon:objective-plan-seal-runtime, integration:planner-shard-selection, daemon:role-worker-parallel-batch, daemon:role-worker-model-call, daemon:semantic-verifier-judgment, integration:semantic-verifier-gate, daemon:role-worker-code-candidate, integration:role-candidate-aggregate, integration:aggregate-candidate-ready, schema:integration-candidate-receipt, integration:candidate-selection-receipt, schema:integration-candidate-selection-receipt, integration:verification-receipt, integration:read-only-verifier-lane, provider:role-routing, provider:health-cost-concurrency-scoring, scheduler:parallel-batch-dispatch, scheduler:provider-model-semaphore, scheduler:provider-registry-concurrency, scheduler:duplicate-outcome-rejected, scheduler:worker-context-pack |
| `model.dispatch` | Model provider dispatch | Foundation | Needs setup | no | `openrouter_secret_missing` | provider:openrouter-native-reqwest, registry:runtime-overlay |
| `agent.local_test_edit` | Local test agent file edit | Unavailable | Unavailable | no | `simulation_feature_disabled_for_release_build` | build:simulation-feature-disabled |
| `image.generate` | Image generation | Foundation | Needs setup | no | `provider_image_lane_present_needs_enabled_image_route` | crate:opensks-image, crate:opensks-adapter, daemon:provider-image-tool-executor, schema:image-provenance-receipt |
| `image.inspect` | Image inspection | Foundation | Needs setup | no | `provider_vision_lane_present_needs_enabled_vision_route` | crate:opensks-image, crate:opensks-adapter, daemon:provider-image-tool-executor, schema:image-provenance-receipt |
| `web.research` | Web research tool | Unavailable | Unavailable | no | `no_web_tool_implementation` | — |
| `conversation.persistence` | Conversation persistence | Live | Available | yes | `durable_sqlite_repository` | crate:opensks-conversation, table:conversations |
| `file.edit_manual` | Manual file editing | Live | Available | yes | `safe_file_service_with_optimistic_concurrency` | crate:opensks-file-service, schema:save-text-result |
| `git.commit` | Git commit | Live | Available | yes | `reviewed_index_hash_commit_path` | crate:opensks-git-service, schema:git-commit |
| `git.push` | Git push | Degraded | Limited | yes | `protected_push_outbox_local_remote_proof_only` | crate:opensks-git-service, test:push_cli_full_handshake_pushes_to_local_bare_remote_only |
| `stream.protocol` | Engine stream protocol | Live | Available | yes | `daemon_stream_protocol_v2_explicit_terminal_frames` | daemon:request_completed, swift:explicit-terminal-router, schema:engine-stream-frame, test:request_response_ends_with_an_explicit_terminal_marker, test:subscribe_events_emits_stream_v2_frames |
| `pipeline.graph` | Live pipeline graph | Foundation | Needs setup | no | `objective_planner_live_model_artifact_apply_seal_runtime_present_live_vendor_pending` | crate:opensks-graph, crate:opensks-engine, graph:objective-planner, graph:dag-validation, graph:proof-contract-requirements, graph:bounded-repair-plan, graph:repair-groups, planner:shard-policy, engine:scheduler-requirement-propagation, scheduler:objective-plan-turn-bootstrap, daemon:objective-plan-live-model-planner, daemon:objective-plan-artifact, daemon:objective-plan-child-runtime, daemon:objective-plan-apply-runtime, daemon:objective-plan-seal-runtime, schema:compiled-plan, swift:pipeline-projection-ingest, conversation:timeline-read-model, swift:conversation-timeline-read-model |
| `design.generation` | Design system generation | Foundation | Needs setup | no | `studio_scaffold_without_persist_compile_apply` | — |

## Tool Registry

Tools are emitted from the same `ToolRegistry` snapshot used by provider adapters and executors.

| Tool | Surface | Availability | Permission | Reason | Evidence |
|---|---|---|---|---|---|
| `workspace.list_directory` | List Directory | Available | ReadOnly | `workspace_tool_executable` | tool-registry:canonical-catalog |
| `workspace.read_file_range` | Read File Range | Available | ReadOnly | `workspace_tool_executable` | tool-registry:canonical-catalog |
| `workspace.search_text` | Search Text | Available | ReadOnly | `workspace_tool_executable` | tool-registry:canonical-catalog |
| `codegraph.query_symbol` | Query Symbol | Available | ReadOnly | `codegraph_executor_available` | tool-registry:canonical-catalog |
| `codegraph.references` | Symbol References | Available | ReadOnly | `codegraph_executor_available` | tool-registry:canonical-catalog |
| `context.build_pack` | Build Context Pack | Available | ReadOnly | `context_executor_available` | tool-registry:canonical-catalog |
| `workspace.propose_patch` | Propose Patch | Available | Allow | `patch_engine_executable` | tool-registry:canonical-catalog |
| `workspace.diff_patch` | Diff Patch | Available | Allow | `patch_engine_executable` | tool-registry:canonical-catalog |
| `command.run` | Run Command | Available | Ask | `command_runner_executable` | tool-registry:canonical-catalog |
| `test.run_targeted` | Run Targeted Tests | Available | Ask | `command_runner_executable` | tool-registry:canonical-catalog |
| `git.status` | Git Status | Available | ReadOnly | `git_service_read_only` | tool-registry:canonical-catalog |
| `git.diff` | Git Diff | Available | ReadOnly | `git_service_read_only` | tool-registry:canonical-catalog |
| `git.log` | Git Log | Available | ReadOnly | `git_service_read_only` | tool-registry:canonical-catalog |
| `artifact.read` | Read Artifact | Available | ReadOnly | `artifact_store_executable` | tool-registry:canonical-catalog |
| `artifact.write` | Write Artifact | Available | Ask | `artifact_store_executable` | tool-registry:canonical-catalog |
| `image.generate` | Generate Image | Available | Ask | `provider_image_executor_route_required` | tool-registry:canonical-catalog |
| `image.inspect` | Inspect Image | Available | ReadOnly | `provider_vision_executor_route_required` | tool-registry:canonical-catalog |
| `mcp.invoke` | Invoke MCP | Available | Ask | `local_mcp_broker_executable` | tool-registry:canonical-catalog |
| `skill.invoke` | Invoke Skill | Available | Ask | `local_skill_registry_executable` | tool-registry:canonical-catalog |
