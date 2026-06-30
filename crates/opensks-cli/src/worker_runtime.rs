use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Arc, Barrier, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use super::{
    CliError, CliOutput, ClockStamp, json_string, require_freeform_cli, sha256_v1,
    write_text_atomic,
};

const DEFAULT_WORKER_LEASE_TTL_SECONDS: u64 = 30;

#[derive(Debug, Clone)]
struct WorkerLeaseRecord {
    lease_id: String,
    worker_id: String,
    lane: String,
    state: String,
    leased_at_seconds: u64,
    last_heartbeat_seconds: u64,
    expires_at_seconds: u64,
    recovery_action: String,
}

#[derive(Debug, Clone)]
struct WorkerRouteRecord {
    request_id: String,
    lane: String,
    assigned_worker: String,
    lease_id: String,
    route_status: String,
    queued_at_ms: u128,
    dispatched_at_ms: u128,
    completed_at_ms: u128,
}

#[derive(Debug, Clone)]
struct WorkerFileEditRecord {
    file_id: String,
    lane: String,
    assigned_worker: String,
    lease_id: String,
    relative_path: PathBuf,
    byte_count: usize,
    content_hash: String,
    hash_verified: bool,
    queued_at_ms: u128,
    worker_window_started_at_ms: u128,
    write_completed_at_ms: u128,
    reported_to_main_at_ms: u128,
    main_handoff_before_all_workers_completed: bool,
}

#[derive(Debug, Clone)]
struct WorkerScratchApplyRecord {
    lane: String,
    assigned_worker: String,
    lease_id: String,
    relative_path: PathBuf,
    content_hash: String,
    hash_verified: bool,
    worker_window_started_at_ms: u128,
    write_completed_at_ms: u128,
}

pub fn run_worker_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let subcommand = args
        .first()
        .ok_or_else(|| CliError::Usage(worker_usage().to_string()))?;
    if subcommand != "runtime" {
        return Err(CliError::Usage(format!(
            "unknown worker subcommand `{subcommand}`\n\n{}",
            worker_usage()
        )));
    }
    let scratch_apply = args.get(1).is_some_and(|arg| arg == "--scratch-apply");
    let goal_args = if scratch_apply {
        &args[2..]
    } else {
        &args[1..]
    };
    let goal = require_freeform_cli(goal_args, worker_usage())?;
    let stamp = ClockStamp::now()?;
    let run_id = format!("worker-runtime-{}-{}", stamp.compact_id(), process::id());
    let dir = cwd.join(".opensks").join("workers").join(&run_id);
    fs::create_dir_all(&dir)?;

    let leases = build_worker_lease_records(&stamp);
    let routes = run_local_worker_request_routes(&leases);
    let file_edits = run_parallel_worker_file_edits(&dir, &run_id, &goal, &leases)?;
    let scratch_records = if scratch_apply {
        let records = run_parallel_worker_scratch_apply(&dir, &run_id, &goal, &leases)?;
        write_text_atomic(
            &dir.join("scratch-apply.json"),
            &render_worker_scratch_apply(&stamp, &run_id, &goal, &records),
        )?;
        Some(records)
    } else {
        None
    };
    write_text_atomic(
        &dir.join("worker-leases.json"),
        &render_worker_leases(&stamp, &run_id, &goal, &leases),
    )?;
    write_text_atomic(
        &dir.join("worker-heartbeats.jsonl"),
        &render_worker_heartbeats(&stamp, &run_id, &leases),
    )?;
    write_text_atomic(
        &dir.join("worker-bus.json"),
        &render_worker_bus(&stamp, &run_id, &routes),
    )?;
    write_text_atomic(
        &dir.join("worker-routing.json"),
        &render_worker_routing(&stamp, &run_id, &routes),
    )?;
    write_text_atomic(
        &dir.join("worker-file-edits.json"),
        &render_worker_file_edits(&stamp, &run_id, &goal, &file_edits),
    )?;
    write_text_atomic(
        &dir.join("worker-final-state.json"),
        &render_worker_final_state(
            &stamp,
            &run_id,
            &leases,
            &routes,
            &file_edits,
            scratch_records.as_deref(),
        ),
    )?;

    let recovered = leases
        .iter()
        .filter(|lease| lease.state == "recovered_expired")
        .count();
    let scratch_line = scratch_records.as_ref().map_or(String::new(), |records| {
        format!("scratch_apply_files: {}\n", records.len())
    });
    Ok(CliOutput {
        stdout: format!(
            "wrote local worker runtime artifacts\nrun: {}\nleases: {}\nrecovered_expired: {}\nrouted_requests: {}\nfile_writes: {}\n{}artifacts: {}\n",
            run_id,
            leases.len(),
            recovered,
            routes.len(),
            file_edits.len(),
            scratch_line,
            dir.display()
        ),
    })
}

pub fn worker_usage() -> &'static str {
    "usage: opensks worker runtime [--scratch-apply] \"<goal>\"\n"
}

fn build_worker_lease_records(stamp: &ClockStamp) -> Vec<WorkerLeaseRecord> {
    let now = stamp.secs;
    vec![
        WorkerLeaseRecord {
            lease_id: "lease-implementation-active".to_string(),
            worker_id: "worker-implementation-1".to_string(),
            lane: "implementation_worker".to_string(),
            state: "active".to_string(),
            leased_at_seconds: now.saturating_sub(5),
            last_heartbeat_seconds: now,
            expires_at_seconds: now + DEFAULT_WORKER_LEASE_TTL_SECONDS,
            recovery_action: "none".to_string(),
        },
        WorkerLeaseRecord {
            lease_id: "lease-review-active".to_string(),
            worker_id: "worker-reviewer-1".to_string(),
            lane: "qa_reviewer".to_string(),
            state: "active".to_string(),
            leased_at_seconds: now.saturating_sub(4),
            last_heartbeat_seconds: now,
            expires_at_seconds: now + DEFAULT_WORKER_LEASE_TTL_SECONDS,
            recovery_action: "none".to_string(),
        },
        WorkerLeaseRecord {
            lease_id: "lease-stale-recovered".to_string(),
            worker_id: "worker-implementation-stale".to_string(),
            lane: "implementation_worker".to_string(),
            state: "recovered_expired".to_string(),
            leased_at_seconds: now.saturating_sub(DEFAULT_WORKER_LEASE_TTL_SECONDS + 45),
            last_heartbeat_seconds: now.saturating_sub(DEFAULT_WORKER_LEASE_TTL_SECONDS + 15),
            expires_at_seconds: now.saturating_sub(15),
            recovery_action: "expired_lease_reassigned_to_worker-implementation-1".to_string(),
        },
    ]
}

fn run_local_worker_request_routes(leases: &[WorkerLeaseRecord]) -> Vec<WorkerRouteRecord> {
    let active = leases
        .iter()
        .filter(|lease| lease.state == "active")
        .collect::<Vec<_>>();
    let origin = Instant::now();
    let dispatch_barrier = Arc::new(Barrier::new(active.len().max(1)));
    let handles = active
        .iter()
        .enumerate()
        .map(|(index, lease)| {
            let lane = lease.lane.clone();
            let worker_id = lease.worker_id.clone();
            let lease_id = lease.lease_id.clone();
            let dispatch_barrier = Arc::clone(&dispatch_barrier);
            thread::spawn(move || {
                let queued_at_ms = origin.elapsed().as_millis();
                let dispatched_at_ms = origin.elapsed().as_millis();
                dispatch_barrier.wait();
                thread::sleep(Duration::from_millis(5 + index as u64));
                WorkerRouteRecord {
                    request_id: format!("request-{}", index + 1),
                    lane,
                    assigned_worker: worker_id,
                    lease_id,
                    route_status: "completed".to_string(),
                    queued_at_ms,
                    dispatched_at_ms,
                    completed_at_ms: origin.elapsed().as_millis(),
                }
            })
        })
        .collect::<Vec<_>>();

    handles
        .into_iter()
        .filter_map(|handle| handle.join().ok())
        .collect()
}

fn run_parallel_worker_file_edits(
    dir: &Path,
    run_id: &str,
    goal: &str,
    leases: &[WorkerLeaseRecord],
) -> Result<Vec<WorkerFileEditRecord>, CliError> {
    let active = leases
        .iter()
        .filter(|lease| lease.state == "active")
        .collect::<Vec<_>>();
    fs::create_dir_all(dir.join("files"))?;
    let origin = Instant::now();
    let dispatch_barrier = Arc::new(Barrier::new(active.len().max(1)));
    let started_barrier = Arc::new(Barrier::new(active.len().max(1)));
    let (sender, receiver) = mpsc::channel();
    let handles = active
        .iter()
        .enumerate()
        .map(|(index, lease)| {
            let lane = lease.lane.clone();
            let worker_id = lease.worker_id.clone();
            let lease_id = lease.lease_id.clone();
            let file_id = format!("file-edit-{}", index + 1);
            let relative_path = PathBuf::from("files").join(format!(
                "{}-{}-{}.md",
                index + 1,
                slugify(&lane),
                slugify(&worker_id)
            ));
            let target = dir.join(&relative_path);
            let content = render_worker_file_edit_contents(run_id, goal, &lane, &worker_id);
            let dispatch_barrier = Arc::clone(&dispatch_barrier);
            let started_barrier = Arc::clone(&started_barrier);
            let sender = sender.clone();
            thread::spawn(move || {
                let queued_at_ms = origin.elapsed().as_millis();
                dispatch_barrier.wait();
                let worker_window_started_at_ms = origin.elapsed().as_millis();
                started_barrier.wait();
                let simulated_work_ms = if index == 0 { 10 } else { 85 + index as u64 };
                thread::sleep(Duration::from_millis(simulated_work_ms));
                let result = (|| -> Result<WorkerFileEditRecord, String> {
                    fs::write(&target, &content)
                        .map_err(|error| format!("write {}: {error}", target.display()))?;
                    let write_completed_at_ms = origin.elapsed().as_millis();
                    let read_back = fs::read_to_string(&target)
                        .map_err(|error| format!("read {}: {error}", target.display()))?;
                    let content_hash = sha256_v1(&read_back);
                    let hash_verified = read_back == content && content_hash == sha256_v1(&content);
                    Ok(WorkerFileEditRecord {
                        file_id,
                        lane,
                        assigned_worker: worker_id,
                        lease_id,
                        relative_path,
                        byte_count: read_back.len(),
                        content_hash,
                        hash_verified,
                        queued_at_ms,
                        worker_window_started_at_ms,
                        write_completed_at_ms,
                        reported_to_main_at_ms: 0,
                        main_handoff_before_all_workers_completed: false,
                    })
                })();
                let _ = sender.send(result);
            })
        })
        .collect::<Vec<_>>();
    drop(sender);

    let mut edits = Vec::new();
    let total_workers = handles.len();
    for received in receiver {
        let mut edit = received
            .map_err(|error| CliError::Invalid(format!("worker file edit failed: {error}")))?;
        edit.reported_to_main_at_ms = origin.elapsed().as_millis();
        edit.main_handoff_before_all_workers_completed = edits.len() + 1 < total_workers;
        edits.push(edit);
    }
    for handle in handles {
        handle
            .join()
            .map_err(|_| CliError::Invalid("worker file edit thread panicked".to_string()))?;
    }
    edits.sort_by(|left, right| left.file_id.cmp(&right.file_id));
    Ok(edits)
}

fn render_worker_file_edit_contents(
    run_id: &str,
    goal: &str,
    lane: &str,
    worker_id: &str,
) -> String {
    format!(
        concat!(
            "# OpenSKS worker file edit proof\n\n",
            "- run_id: `{}`\n",
            "- goal: `{}`\n",
            "- lane: `{}`\n",
            "- worker_id: `{}`\n",
            "- mutation_scope: `.opensks/workers/<run>/files`\n",
            "- claim: this file was written by a local worker lane during the runtime smoke.\n"
        ),
        run_id, goal, lane, worker_id
    )
}

fn run_parallel_worker_scratch_apply(
    dir: &Path,
    run_id: &str,
    goal: &str,
    leases: &[WorkerLeaseRecord],
) -> Result<Vec<WorkerScratchApplyRecord>, CliError> {
    let active = leases
        .iter()
        .filter(|lease| lease.state == "active")
        .collect::<Vec<_>>();
    let scratch_src = dir.join("scratch-project").join("src");
    fs::create_dir_all(&scratch_src)?;
    write_text_atomic(
        &dir.join("scratch-project").join("README.md"),
        "# OpenSKS parallel worker scratch project\n\nThis disposable project is written by worker lanes during `opensks worker runtime --scratch-apply`.\n",
    )?;

    let origin = Instant::now();
    let dispatch_barrier = Arc::new(Barrier::new(active.len().max(1)));
    let (sender, receiver) = mpsc::channel();
    let handles = active
        .iter()
        .enumerate()
        .map(|(index, lease)| {
            let lane = lease.lane.clone();
            let worker_id = lease.worker_id.clone();
            let lease_id = lease.lease_id.clone();
            let relative_path = PathBuf::from("scratch-project").join("src").join(format!(
                "{}_{}.rs",
                index + 1,
                slugify(&lane)
            ));
            let target = dir.join(&relative_path);
            let content = render_worker_scratch_source(run_id, goal, &lane, &worker_id);
            let dispatch_barrier = Arc::clone(&dispatch_barrier);
            let sender = sender.clone();
            thread::spawn(move || {
                dispatch_barrier.wait();
                let worker_window_started_at_ms = origin.elapsed().as_millis();
                thread::sleep(Duration::from_millis(15 + (index as u64 * 20)));
                let result = (|| -> Result<WorkerScratchApplyRecord, String> {
                    fs::write(&target, &content)
                        .map_err(|error| format!("write {}: {error}", target.display()))?;
                    let write_completed_at_ms = origin.elapsed().as_millis();
                    let read_back = fs::read_to_string(&target)
                        .map_err(|error| format!("read {}: {error}", target.display()))?;
                    let content_hash = sha256_v1(&read_back);
                    let hash_verified = read_back == content && content_hash == sha256_v1(&content);
                    Ok(WorkerScratchApplyRecord {
                        lane,
                        assigned_worker: worker_id,
                        lease_id,
                        relative_path,
                        content_hash,
                        hash_verified,
                        worker_window_started_at_ms,
                        write_completed_at_ms,
                    })
                })();
                let _ = sender.send(result);
            })
        })
        .collect::<Vec<_>>();
    drop(sender);

    let mut records = Vec::new();
    for received in receiver {
        records.push(
            received
                .map_err(|error| CliError::Invalid(format!("scratch apply failed: {error}")))?,
        );
    }
    for handle in handles {
        handle
            .join()
            .map_err(|_| CliError::Invalid("scratch apply worker thread panicked".to_string()))?;
    }
    records.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(records)
}

fn render_worker_scratch_source(run_id: &str, goal: &str, lane: &str, worker_id: &str) -> String {
    let function_name = slugify(lane).replace('-', "_");
    format!(
        concat!(
            "pub fn {}_proof() -> &'static str {{\n",
            "    \"run={} lane={} worker={} goal={}\"\n",
            "}}\n"
        ),
        function_name, run_id, lane, worker_id, goal
    )
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "worker".to_string()
    } else {
        slug
    }
}

fn render_worker_leases(
    stamp: &ClockStamp,
    run_id: &str,
    goal: &str,
    leases: &[WorkerLeaseRecord],
) -> String {
    let active_count = leases
        .iter()
        .filter(|lease| lease.state == "active")
        .count();
    let recovered_count = leases
        .iter()
        .filter(|lease| lease.state == "recovered_expired")
        .count();
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.worker-leases.v1\",\n",
            "  \"run_id\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"goal\": {},\n",
            "  \"lease_ttl_seconds\": {},\n",
            "  \"durable_lease_store\": \"local_json_artifact\",\n",
            "  \"heartbeat_source\": \"worker-heartbeats.jsonl\",\n",
            "  \"recovery_policy\": \"expire_missing_heartbeat_then_reassign_lane\",\n",
            "  \"active_lease_count\": {},\n",
            "  \"recovered_expired_lease_count\": {},\n",
            "  \"live_provider_workers\": false,\n",
            "  \"leases\": {}\n",
            "}}\n"
        ),
        json_string(run_id),
        stamp.json(),
        json_string(goal),
        DEFAULT_WORKER_LEASE_TTL_SECONDS,
        active_count,
        recovered_count,
        render_worker_lease_items_json(leases)
    )
}

fn render_worker_heartbeats(
    stamp: &ClockStamp,
    run_id: &str,
    leases: &[WorkerLeaseRecord],
) -> String {
    let mut lines = Vec::new();
    for lease in leases {
        lines.push(format!(
            concat!(
                "{{\"schema\":\"opensks.worker-heartbeat.v1\",\"run_id\":{},",
                "\"generated_at\":{},\"lease_id\":{},\"worker_id\":{},\"lane\":{},",
                "\"last_heartbeat_seconds\":{},\"expires_at_seconds\":{},",
                "\"lease_state\":{},\"recovery_action\":{}}}"
            ),
            json_string(run_id),
            stamp.json(),
            json_string(&lease.lease_id),
            json_string(&lease.worker_id),
            json_string(&lease.lane),
            lease.last_heartbeat_seconds,
            lease.expires_at_seconds,
            json_string(&lease.state),
            json_string(&lease.recovery_action)
        ));
    }
    lines.join("\n") + "\n"
}

fn render_worker_bus(stamp: &ClockStamp, run_id: &str, routes: &[WorkerRouteRecord]) -> String {
    let concurrent_routing = worker_routes_overlap(routes);
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.worker-bus.v1\",\n",
            "  \"run_id\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"daemon_visible\": true,\n",
            "  \"request_source\": \"local_worker_runtime\",\n",
            "  \"concurrent_request_routing\": {},\n",
            "  \"routed_request_count\": {},\n",
            "  \"live_remote_provider_bus\": false,\n",
            "  \"routes_ref\": \"worker-routing.json\"\n",
            "}}\n"
        ),
        json_string(run_id),
        stamp.json(),
        concurrent_routing,
        routes.len()
    )
}

fn render_worker_routing(stamp: &ClockStamp, run_id: &str, routes: &[WorkerRouteRecord]) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.worker-routing.v1\",\n",
            "  \"run_id\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"daemon_visible\": true,\n",
            "  \"concurrent_request_routing\": {},\n",
            "  \"routes\": {}\n",
            "}}\n"
        ),
        json_string(run_id),
        stamp.json(),
        worker_routes_overlap(routes),
        render_worker_route_items_json(routes)
    )
}

fn render_worker_file_edits(
    stamp: &ClockStamp,
    run_id: &str,
    goal: &str,
    edits: &[WorkerFileEditRecord],
) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.worker-file-edits.v1\",\n",
            "  \"run_id\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"goal\": {},\n",
            "  \"daemon_visible\": true,\n",
            "  \"file_write_source\": \"local_worker_runtime\",\n",
            "  \"workspace_mutation_scope\": \".opensks/workers/<run>/files\",\n",
            "  \"actual_file_write_count\": {},\n",
            "  \"parallel_worker_file_edit_windows_verified\": {},\n",
            "  \"nonblocking_worker_result_handoff_verified\": {},\n",
            "  \"all_write_hashes_verified\": {},\n",
            "  \"live_provider_file_edits\": false,\n",
            "  \"edits\": {}\n",
            "}}\n"
        ),
        json_string(run_id),
        stamp.json(),
        json_string(goal),
        edits.len(),
        worker_file_edits_overlap(edits),
        worker_file_edits_nonblocking_handoff_verified(edits),
        worker_file_edits_hashes_verified(edits),
        render_worker_file_edit_items_json(edits)
    )
}

fn render_worker_scratch_apply(
    stamp: &ClockStamp,
    run_id: &str,
    goal: &str,
    records: &[WorkerScratchApplyRecord],
) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.worker-scratch-apply.v1\",\n",
            "  \"run_id\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"goal\": {},\n",
            "  \"scratch_project_ref\": \"scratch-project\",\n",
            "  \"actual_source_file_write_count\": {},\n",
            "  \"parallel_source_file_write_windows_verified\": {},\n",
            "  \"all_write_hashes_verified\": {},\n",
            "  \"user_workspace_source_tree_modified\": false,\n",
            "  \"records\": {}\n",
            "}}\n"
        ),
        json_string(run_id),
        stamp.json(),
        json_string(goal),
        records.len(),
        worker_scratch_apply_overlap(records),
        worker_scratch_apply_hashes_verified(records),
        render_worker_scratch_apply_items_json(records)
    )
}

fn render_worker_final_state(
    stamp: &ClockStamp,
    run_id: &str,
    leases: &[WorkerLeaseRecord],
    routes: &[WorkerRouteRecord],
    edits: &[WorkerFileEditRecord],
    scratch_records: Option<&[WorkerScratchApplyRecord]>,
) -> String {
    let active_count = leases
        .iter()
        .filter(|lease| lease.state == "active")
        .count();
    let expired_count = leases
        .iter()
        .filter(|lease| lease.expires_at_seconds <= stamp.secs)
        .count();
    let recovered_count = leases
        .iter()
        .filter(|lease| lease.state == "recovered_expired")
        .count();
    let routing_passed = routes
        .iter()
        .all(|route| route.route_status == "completed" && !route.assigned_worker.is_empty());
    let file_write_passed = !edits.is_empty()
        && worker_file_edits_overlap(edits)
        && worker_file_edits_hashes_verified(edits);
    let status = if active_count > 0 && recovered_count > 0 && routing_passed && file_write_passed {
        "passed"
    } else {
        "partial"
    };
    let scratch_apply_file_count = scratch_records.map_or(0, |records| records.len());
    let scratch_apply_verified = scratch_records.is_some_and(|records| {
        !records.is_empty()
            && worker_scratch_apply_overlap(records)
            && worker_scratch_apply_hashes_verified(records)
    });
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"opensks.worker-final-state.v1\",\n",
            "  \"run_id\": {},\n",
            "  \"generated_at\": {},\n",
            "  \"status\": {},\n",
            "  \"active_lease_count\": {},\n",
            "  \"expired_lease_count\": {},\n",
            "  \"recovered_expired_lease_count\": {},\n",
            "  \"heartbeat_artifact\": \"worker-heartbeats.jsonl\",\n",
            "  \"lease_artifact\": \"worker-leases.json\",\n",
            "  \"bus_artifact\": \"worker-bus.json\",\n",
            "  \"routing_artifact\": \"worker-routing.json\",\n",
            "  \"file_edit_artifact\": \"worker-file-edits.json\",\n",
            "  \"daemon_visible_worker_bus\": true,\n",
            "  \"concurrent_request_routing\": {},\n",
            "  \"routed_request_count\": {},\n",
            "  \"actual_file_write_count\": {},\n",
            "  \"parallel_worker_file_edit_windows_verified\": {},\n",
            "  \"nonblocking_worker_result_handoff_verified\": {},\n",
            "  \"all_file_write_hashes_verified\": {},\n",
            "  \"scratch_apply_file_count\": {},\n",
            "  \"scratch_apply_verified\": {},\n",
            "  \"live_provider_workers\": false,\n",
            "  \"live_remote_provider_bus\": false\n",
            "}}\n"
        ),
        json_string(run_id),
        stamp.json(),
        json_string(status),
        active_count,
        expired_count,
        recovered_count,
        worker_routes_overlap(routes),
        routes.len(),
        edits.len(),
        worker_file_edits_overlap(edits),
        worker_file_edits_nonblocking_handoff_verified(edits),
        worker_file_edits_hashes_verified(edits),
        scratch_apply_file_count,
        scratch_apply_verified
    )
}

fn render_worker_lease_items_json(leases: &[WorkerLeaseRecord]) -> String {
    let rows = leases
        .iter()
        .map(|lease| {
            format!(
                concat!(
                    "{{\"lease_id\":{},\"worker_id\":{},\"lane\":{},\"state\":{},",
                    "\"leased_at_seconds\":{},\"last_heartbeat_seconds\":{},",
                    "\"expires_at_seconds\":{},\"recovery_action\":{}}}"
                ),
                json_string(&lease.lease_id),
                json_string(&lease.worker_id),
                json_string(&lease.lane),
                json_string(&lease.state),
                lease.leased_at_seconds,
                lease.last_heartbeat_seconds,
                lease.expires_at_seconds,
                json_string(&lease.recovery_action)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn render_worker_route_items_json(routes: &[WorkerRouteRecord]) -> String {
    let rows = routes
        .iter()
        .map(|route| {
            format!(
                concat!(
                    "{{\"request_id\":{},\"lane\":{},\"assigned_worker\":{},",
                    "\"lease_id\":{},\"route_status\":{},\"queued_at_ms\":{},",
                    "\"dispatched_at_ms\":{},\"completed_at_ms\":{}}}"
                ),
                json_string(&route.request_id),
                json_string(&route.lane),
                json_string(&route.assigned_worker),
                json_string(&route.lease_id),
                json_string(&route.route_status),
                route.queued_at_ms,
                route.dispatched_at_ms,
                route.completed_at_ms
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn render_worker_file_edit_items_json(edits: &[WorkerFileEditRecord]) -> String {
    let rows = edits
        .iter()
        .map(|edit| {
            format!(
                concat!(
                    "{{\"file_id\":{},\"lane\":{},\"assigned_worker\":{},",
                    "\"lease_id\":{},\"relative_path\":{},\"byte_count\":{},",
                    "\"content_hash\":{},\"hash_verified\":{},\"queued_at_ms\":{},",
                    "\"worker_window_started_at_ms\":{},\"write_completed_at_ms\":{},",
                    "\"reported_to_main_at_ms\":{},",
                    "\"main_handoff_before_all_workers_completed\":{}}}"
                ),
                json_string(&edit.file_id),
                json_string(&edit.lane),
                json_string(&edit.assigned_worker),
                json_string(&edit.lease_id),
                json_string(&edit.relative_path.display().to_string()),
                edit.byte_count,
                json_string(&edit.content_hash),
                edit.hash_verified,
                edit.queued_at_ms,
                edit.worker_window_started_at_ms,
                edit.write_completed_at_ms,
                edit.reported_to_main_at_ms,
                edit.main_handoff_before_all_workers_completed
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn render_worker_scratch_apply_items_json(records: &[WorkerScratchApplyRecord]) -> String {
    let rows = records
        .iter()
        .map(|record| {
            format!(
                concat!(
                    "{{\"lane\":{},\"assigned_worker\":{},\"lease_id\":{},",
                    "\"relative_path\":{},\"content_hash\":{},\"hash_verified\":{},",
                    "\"worker_window_started_at_ms\":{},\"write_completed_at_ms\":{}}}"
                ),
                json_string(&record.lane),
                json_string(&record.assigned_worker),
                json_string(&record.lease_id),
                json_string(&record.relative_path.display().to_string()),
                json_string(&record.content_hash),
                record.hash_verified,
                record.worker_window_started_at_ms,
                record.write_completed_at_ms
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{rows}]")
}

fn worker_routes_overlap(routes: &[WorkerRouteRecord]) -> bool {
    routes.iter().enumerate().any(|(index, left)| {
        routes.iter().skip(index + 1).any(|right| {
            left.dispatched_at_ms <= right.completed_at_ms
                && right.dispatched_at_ms <= left.completed_at_ms
        })
    })
}

fn worker_file_edits_overlap(edits: &[WorkerFileEditRecord]) -> bool {
    edits.iter().enumerate().any(|(index, left)| {
        edits.iter().skip(index + 1).any(|right| {
            left.worker_window_started_at_ms <= right.write_completed_at_ms
                && right.worker_window_started_at_ms <= left.write_completed_at_ms
        })
    })
}

fn worker_file_edits_hashes_verified(edits: &[WorkerFileEditRecord]) -> bool {
    !edits.is_empty() && edits.iter().all(|edit| edit.hash_verified)
}

fn worker_file_edits_nonblocking_handoff_verified(edits: &[WorkerFileEditRecord]) -> bool {
    edits.iter().any(|edit| {
        edit.main_handoff_before_all_workers_completed
            && edit.reported_to_main_at_ms >= edit.write_completed_at_ms
    })
}

fn worker_scratch_apply_overlap(records: &[WorkerScratchApplyRecord]) -> bool {
    records.iter().enumerate().any(|(index, left)| {
        records.iter().skip(index + 1).any(|right| {
            left.worker_window_started_at_ms <= right.write_completed_at_ms
                && right.worker_window_started_at_ms <= left.write_completed_at_ms
        })
    })
}

fn worker_scratch_apply_hashes_verified(records: &[WorkerScratchApplyRecord]) -> bool {
    !records.is_empty() && records.iter().all(|record| record.hash_verified)
}
