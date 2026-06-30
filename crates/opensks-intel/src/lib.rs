//! Project Intelligence + Freshness data plane (PR-041).
//!
//! This crate computes a deterministic [`FreshnessStamp`] for a workspace, the
//! [`FreshnessCheck`] comparison that guarantees *stale is never reported as
//! fresh*, a paged [`CodegraphQuery`], and the [`Glossary`] / [`Architecture`]
//! readers. The CLI layer (`opensks-cli`) wires these to the `intel` verb; no
//! domain logic lives in the binary root.
//!
//! # Determinism of the three hashes
//!
//! - **`head_hash`** — `git rev-parse HEAD` (`None` outside a repo / unborn
//!   HEAD). The committed tip is exact and deterministic.
//! - **`worktree_hash`** — an FNV-1a digest over the *sorted* set of tracked and
//!   modified working-tree paths, each paired with the FNV content hash of its
//!   current bytes. Because the file's *content* feeds the digest, any in-place
//!   edit (even one that leaves `git status` text unchanged) flips the hash. The
//!   path set is sorted so the digest is independent of filesystem iteration
//!   order. Outside a repo, a deterministic sorted directory walk is used.
//! - **`index_hash`** — the codegraph index `workspace_fingerprint` (a digest of
//!   every record's content hash). A re-index that changes any symbol flips it.
//!
//! # Stale-is-never-fresh invariant
//!
//! [`freshness_check`] sets `fresh: true` only when **every** provided stamp
//! equals the current value. An empty comparison (no stamps provided) is treated
//! as *not fresh*, and the first divergence (HEAD → worktree → index order) is
//! reported. There is no code path that returns `fresh: true` while any provided
//! stamp differs from current.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use opensks_codegraph::CodeGraph;
use opensks_contracts::{
    Architecture, ArchitectureRecord, CodegraphQuery, CodegraphRecordView, FreshnessCheck,
    FreshnessCurrent, FreshnessStamp, Glossary, GlossaryTerm, INTEL_ARCHITECTURE_SCHEMA,
    INTEL_CODEGRAPH_SCHEMA, INTEL_FRESHNESS_CHECK_SCHEMA, INTEL_FRESHNESS_SCHEMA,
    INTEL_GLOSSARY_SCHEMA, StaleReason, TriWikiRecordKind,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IntelError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("codegraph error: {0}")]
    CodeGraph(#[from] opensks_codegraph::CodeGraphError),
    #[error("triwiki error: {0}")]
    TriWiki(#[from] opensks_triwiki::TriWikiError),
}

/// A previously recorded freshness stamp to compare against the current state.
///
/// A `None` field means "the caller did not provide this stamp". Per the
/// stale-is-never-fresh invariant, a provided field that does not match current
/// is reported stale; an all-`None` comparison is *not fresh*.
#[derive(Debug, Clone, Default)]
pub struct StampedFreshness {
    pub head_hash: Option<String>,
    pub worktree_hash: Option<String>,
    pub index_hash: Option<String>,
}

// --- freshness --------------------------------------------------------------

/// Compute the current freshness stamp for `workspace`.
pub fn freshness(workspace: &Path) -> Result<FreshnessStamp, IntelError> {
    freshness_with_index_hash(workspace, index_hash(workspace)?)
}

fn freshness_with_index_hash(
    workspace: &Path,
    index_hash: String,
) -> Result<FreshnessStamp, IntelError> {
    let in_repo = in_repo(workspace);
    let head_hash = if in_repo {
        head_commit(workspace)
    } else {
        None
    };
    let worktree_hash = worktree_hash(workspace, in_repo)?;
    Ok(FreshnessStamp {
        schema: INTEL_FRESHNESS_SCHEMA.to_string(),
        head_hash,
        worktree_hash,
        index_hash,
        in_repo,
    })
}

/// Compare a previously stamped freshness against the current workspace state.
///
/// `fresh` is `true` only when every provided stamp equals current; any
/// divergence (or an all-`None` comparison) yields `fresh: false`. The first
/// divergence in HEAD → worktree → index order is the `stale_reason`.
pub fn freshness_check(
    workspace: &Path,
    stamped: &StampedFreshness,
) -> Result<FreshnessCheck, IntelError> {
    let current = freshness(workspace)?;

    // Evaluate divergence in a fixed priority order. A provided stamp that does
    // not equal current is the (first) stale reason. We never set fresh:true
    // unless at least one stamp was provided AND all provided stamps matched.
    let mut provided = false;
    let mut stale_reason = None;

    if let Some(head) = &stamped.head_hash {
        provided = true;
        if Some(head) != current.head_hash.as_ref() {
            stale_reason = Some(StaleReason::HeadChanged);
        }
    }
    if stale_reason.is_none() {
        if let Some(worktree) = &stamped.worktree_hash {
            provided = true;
            if worktree != &current.worktree_hash {
                stale_reason = Some(StaleReason::WorktreeChanged);
            }
        }
    }
    if stale_reason.is_none() {
        if let Some(index) = &stamped.index_hash {
            provided = true;
            if index != &current.index_hash {
                stale_reason = Some(StaleReason::IndexChanged);
            }
        }
    }

    // Paramount invariant: an empty comparison is NOT fresh, and any divergence
    // is NOT fresh. Only an all-matched, non-empty comparison is fresh.
    let fresh = provided && stale_reason.is_none();
    let stale_reason = if fresh { None } else { stale_reason };

    Ok(FreshnessCheck {
        schema: INTEL_FRESHNESS_CHECK_SCHEMA.to_string(),
        fresh,
        stale_reason,
        current: FreshnessCurrent {
            head_hash: current.head_hash,
            worktree_hash: current.worktree_hash,
            index_hash: current.index_hash,
        },
    })
}

// --- codegraph query (paged) ------------------------------------------------

/// Run a paged codegraph query. Loads the persisted index when present (so a
/// large graph is not rebuilt), else builds it once. `total` is the true match
/// count; `records` is the `[offset, offset + limit)` slice. The current
/// freshness stamp is attached.
pub fn codegraph_query(
    workspace: &Path,
    query: &str,
    limit: usize,
    offset: usize,
) -> Result<CodegraphQuery, IntelError> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(CodegraphQuery {
            schema: INTEL_CODEGRAPH_SCHEMA.to_string(),
            total: 0,
            limit,
            offset: 0,
            records: Vec::new(),
            freshness: freshness(workspace)?,
        });
    }

    let graph = load_or_build_graph(workspace)?;
    let mut hits = graph.query(query);
    // Deterministic ordering for stable paging across calls: by (path, line,
    // name). `query` already returns BTreeMap-ordered records, but we sort
    // explicitly so paging never depends on internal map iteration order.
    hits.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.name.cmp(&b.name))
    });
    let total = hits.len();
    let records = hits
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|record| CodegraphRecordView {
            path: record.path,
            symbol: record.name,
            kind: kind_label(&record.kind),
            line: record.line,
        })
        .collect();
    let freshness = freshness_with_index_hash(workspace, graph.to_index().workspace_fingerprint)?;
    Ok(CodegraphQuery {
        schema: INTEL_CODEGRAPH_SCHEMA.to_string(),
        total,
        limit,
        offset,
        records,
        freshness,
    })
}

// --- glossary + architecture ------------------------------------------------

/// Surface glossary terms from the TriWiki record store, with freshness.
pub fn glossary(workspace: &Path) -> Result<Glossary, IntelError> {
    let mut terms: Vec<GlossaryTerm> = opensks_triwiki::load_records(workspace)?
        .into_iter()
        .filter(|record| record.kind == TriWikiRecordKind::Glossary)
        .map(|record| GlossaryTerm {
            term: record.title,
            definition: record.body,
            refs: record.evidence_refs,
        })
        .collect();
    terms.sort_by(|a, b| a.term.cmp(&b.term));
    let freshness = freshness(workspace)?;
    Ok(Glossary {
        schema: INTEL_GLOSSARY_SCHEMA.to_string(),
        terms,
        freshness,
    })
}

/// Surface architecture records from the TriWiki record store, with freshness.
pub fn architecture(workspace: &Path) -> Result<Architecture, IntelError> {
    let mut records: Vec<ArchitectureRecord> = opensks_triwiki::load_records(workspace)?
        .into_iter()
        .filter(|record| record.kind == TriWikiRecordKind::Architecture)
        .map(|record| ArchitectureRecord {
            id: record.id,
            title: record.title,
            detail: record.body,
            refs: record.evidence_refs,
        })
        .collect();
    records.sort_by(|a, b| a.id.cmp(&b.id));
    let freshness = freshness(workspace)?;
    Ok(Architecture {
        schema: INTEL_ARCHITECTURE_SCHEMA.to_string(),
        records,
        freshness,
    })
}

// --- hashing internals ------------------------------------------------------

/// The codegraph index hash: load a persisted index if present, else use the
/// empty graph fingerprint.
///
/// `freshness()` is used on chat startup, so it must not trigger a full
/// workspace codegraph build. Explicit codegraph query paths may still build
/// when no persisted index exists and pass their in-memory fingerprint through
/// `freshness_with_index_hash`.
fn index_hash(workspace: &Path) -> Result<String, IntelError> {
    match opensks_codegraph::read_index(workspace)? {
        Some(graph) => Ok(graph.to_index().workspace_fingerprint),
        None => Ok(CodeGraph::default().to_index().workspace_fingerprint),
    }
}

/// Load the persisted codegraph index, or build it once when absent.
fn load_or_build_graph(workspace: &Path) -> Result<CodeGraph, IntelError> {
    match opensks_codegraph::read_index(workspace)? {
        Some(graph) => Ok(graph),
        None => Ok(CodeGraph::index_workspace(workspace)?),
    }
}

/// Content-addressed working-tree digest. In a repo, hashes the sorted set of
/// tracked + modified paths (from `git ls-files` plus `git status --porcelain`)
/// each paired with the FNV content hash of its current bytes, so any in-place
/// edit flips the digest. Outside a repo, a deterministic sorted directory walk
/// of source-bearing files is used.
fn worktree_hash(workspace: &Path, in_repo: bool) -> Result<String, IntelError> {
    let mut entries: Vec<(String, String)> = Vec::new();
    if in_repo {
        let mut paths = tracked_and_modified_paths(workspace);
        paths.sort();
        paths.dedup();
        for relative in paths {
            let absolute = workspace.join(&relative);
            // A deleted-but-tracked path contributes a stable "deleted" marker so
            // a deletion still flips the digest deterministically.
            let content = match fs::read(&absolute) {
                Ok(bytes) => fnv1a64(&bytes),
                Err(_) => "deleted".to_string(),
            };
            entries.push((relative, content));
        }
    } else {
        let mut files = Vec::new();
        collect_walk(workspace, &mut files);
        files.sort();
        for absolute in files {
            let relative = relative_path(workspace, &absolute);
            let content = match fs::read(&absolute) {
                Ok(bytes) => fnv1a64(&bytes),
                Err(_) => "unreadable".to_string(),
            };
            entries.push((relative, content));
        }
    }
    entries.sort();
    Ok(digest_pairs(&entries))
}

/// `git ls-files` (tracked) unioned with `git status --porcelain` (modified /
/// untracked / deleted) — the working-tree path universe whose content we hash.
fn tracked_and_modified_paths(workspace: &Path) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(output) = git(workspace, &["ls-files", "-z"]) {
        paths.extend(
            output
                .split('\0')
                .filter(|item| !item.is_empty())
                .map(|item| item.to_string()),
        );
    }
    if let Some(output) = git(workspace, &["status", "--porcelain", "-z", "--"]) {
        // `--porcelain -z` records are `XY <path>\0` (renames carry a second
        // NUL-separated path). We take any field that is not a 2-char status
        // code prefix, stripping the leading "XY " from status fields.
        for field in output.split('\0') {
            if field.is_empty() {
                continue;
            }
            // A status field is `XY path` (3+ chars, third char is a space).
            if field.len() >= 3 && field.as_bytes()[2] == b' ' {
                paths.push(field[3..].to_string());
            } else {
                // A bare path field (the rename source/destination tail).
                paths.push(field.to_string());
            }
        }
    }
    paths
}

/// Deterministic directory walk for non-repo workspaces. Mirrors the codegraph
/// ignore set so the digest is stable and excludes runtime/data-plane churn.
fn collect_walk(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(
                name.as_ref(),
                ".git"
                    | ".opensks"
                    | ".sneakoscope"
                    | ".omc"
                    | ".github"
                    | "target"
                    | "node_modules"
                    | ".build"
                    | "runtime"
            ) {
                continue;
            }
            collect_walk(&path, files);
        } else if path.is_file() {
            files.push(path);
        }
    }
}

/// True when `workspace` resolves to a Git repository.
fn in_repo(workspace: &Path) -> bool {
    matches!(
        git(workspace, &["rev-parse", "--is-inside-work-tree"]),
        Some(output) if output.trim() == "true"
    )
}

/// The committed `HEAD` object id, or `None` for an unborn HEAD.
fn head_commit(workspace: &Path) -> Option<String> {
    git(workspace, &["rev-parse", "HEAD"]).map(|output| output.trim().to_string())
}

/// Run a read-only git command; `None` on any failure (missing git, nonzero
/// exit, non-repo). Callers treat `None` as "no signal", never as an error.
fn git(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

fn kind_label(kind: &opensks_contracts::CodeGraphNodeKind) -> String {
    match kind {
        opensks_contracts::CodeGraphNodeKind::File => "file",
        opensks_contracts::CodeGraphNodeKind::Symbol => "symbol",
        opensks_contracts::CodeGraphNodeKind::Import => "import",
        opensks_contracts::CodeGraphNodeKind::Test => "test",
        opensks_contracts::CodeGraphNodeKind::Route => "route",
    }
    .to_string()
}

fn relative_path(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// FNV-1a digest over `(path, content_hash)` pairs with field/record separators,
/// matching the byte-mixing style used elsewhere in the workspace.
fn digest_pairs(entries: &[(String, String)]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for (path, content) in entries {
        for byte in path.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0x1f; // unit separator between path and content
        hash = hash.wrapping_mul(0x100000001b3);
        for byte in content.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0x1e; // record separator between entries
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

fn fnv1a64(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn temp_workspace(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "opensks-intel-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("workspace");
        root
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?} failed");
    }

    fn init_repo(name: &str) -> PathBuf {
        let dir = temp_workspace(name);
        run_git(&dir, &["init"]);
        run_git(&dir, &["config", "user.email", "intel@example.test"]);
        run_git(&dir, &["config", "user.name", "Intel Test"]);
        fs::write(dir.join("src/lib.rs"), "pub fn alpha() {}\n").expect("seed");
        run_git(&dir, &["add", "."]);
        run_git(&dir, &["commit", "-m", "initial"]);
        dir
    }

    #[test]
    fn freshness_stamp_is_produced_in_repo() {
        let dir = init_repo("stamp");
        let stamp = freshness(&dir).expect("stamp");
        assert!(stamp.in_repo);
        assert!(stamp.head_hash.is_some());
        assert!(stamp.worktree_hash.starts_with("fnv1a64:"));
        assert!(stamp.index_hash.starts_with("fnv1a64:"));
    }

    #[test]
    fn freshness_stamp_head_is_null_outside_repo() {
        let dir = temp_workspace("no-repo");
        fs::write(dir.join("src/lib.rs"), "pub fn beta() {}\n").expect("write");
        let stamp = freshness(&dir).expect("stamp");
        assert!(!stamp.in_repo);
        assert!(stamp.head_hash.is_none());
        assert!(stamp.worktree_hash.starts_with("fnv1a64:"));
    }

    #[test]
    fn freshness_without_persisted_index_uses_empty_index_fingerprint() {
        let dir = temp_workspace("freshness-no-index-build");
        fs::write(dir.join("src/lib.rs"), "pub fn expensive_marker() {}\n").expect("write");

        let stamp = freshness(&dir).expect("stamp");
        let empty_index_hash = CodeGraph::default().to_index().workspace_fingerprint;
        let built_index_hash = CodeGraph::index_workspace(&dir)
            .expect("index")
            .to_index()
            .workspace_fingerprint;

        assert_eq!(stamp.index_hash, empty_index_hash);
        assert_ne!(
            stamp.index_hash, built_index_hash,
            "freshness must not build a full codegraph when no persisted index exists"
        );
    }

    #[test]
    fn matching_stamp_is_fresh() {
        let dir = init_repo("matching");
        let stamp = freshness(&dir).expect("stamp");
        let stamped = StampedFreshness {
            head_hash: stamp.head_hash.clone(),
            worktree_hash: Some(stamp.worktree_hash.clone()),
            index_hash: Some(stamp.index_hash.clone()),
        };
        let check = freshness_check(&dir, &stamped).expect("check");
        assert!(check.fresh);
        assert!(check.stale_reason.is_none());
    }

    #[test]
    fn working_tree_change_is_never_fresh_and_reports_worktree_changed() {
        let dir = init_repo("worktree-change");
        let stamp = freshness(&dir).expect("stamp");
        let stamped = StampedFreshness {
            head_hash: stamp.head_hash.clone(),
            worktree_hash: Some(stamp.worktree_hash.clone()),
            index_hash: Some(stamp.index_hash.clone()),
        };
        // Edit a tracked file in place: HEAD and the codegraph index hash are
        // unchanged, but the working-tree content moved.
        fs::write(dir.join("src/lib.rs"), "pub fn alpha() {}\n// edited\n").expect("edit");
        let check = freshness_check(&dir, &stamped).expect("check");
        assert!(!check.fresh, "a working-tree change must NEVER be fresh");
        assert_eq!(check.stale_reason, Some(StaleReason::WorktreeChanged));
    }

    #[test]
    fn index_change_reports_index_changed() {
        let dir = init_repo("index-change");
        // Persist an index so a later code change can diverge it.
        let graph = CodeGraph::index_workspace(&dir).expect("index");
        opensks_codegraph::write_index(&dir, &graph).expect("write");
        let stamp = freshness(&dir).expect("stamp");
        // Re-index after a symbol change to move the index_hash, then compare
        // only the index stamp so the index reason is isolated.
        fs::write(dir.join("src/lib.rs"), "pub fn gamma_renamed() {}\n").expect("edit");
        let regraph = CodeGraph::index_workspace(&dir).expect("reindex");
        opensks_codegraph::write_index(&dir, &regraph).expect("rewrite");
        let stamped = StampedFreshness {
            head_hash: None,
            worktree_hash: None,
            index_hash: Some(stamp.index_hash.clone()),
        };
        let check = freshness_check(&dir, &stamped).expect("check");
        assert!(!check.fresh);
        assert_eq!(check.stale_reason, Some(StaleReason::IndexChanged));
    }

    #[test]
    fn head_change_reports_head_changed() {
        let dir = init_repo("head-change");
        let stamp = freshness(&dir).expect("stamp");
        // A new commit moves HEAD. Compare only HEAD to isolate the reason.
        fs::write(
            dir.join("src/lib.rs"),
            "pub fn alpha() {}\npub fn two() {}\n",
        )
        .expect("edit");
        run_git(&dir, &["add", "."]);
        run_git(&dir, &["commit", "-m", "second"]);
        let stamped = StampedFreshness {
            head_hash: stamp.head_hash.clone(),
            worktree_hash: None,
            index_hash: None,
        };
        let check = freshness_check(&dir, &stamped).expect("check");
        assert!(!check.fresh);
        assert_eq!(check.stale_reason, Some(StaleReason::HeadChanged));
    }

    #[test]
    fn unknown_stamp_is_not_fresh() {
        let dir = init_repo("unknown");
        let stamped = StampedFreshness {
            head_hash: Some("deadbeef-not-a-real-head".to_string()),
            worktree_hash: Some("fnv1a64:0000000000000000".to_string()),
            index_hash: Some("fnv1a64:0000000000000000".to_string()),
        };
        let check = freshness_check(&dir, &stamped).expect("check");
        assert!(!check.fresh, "an unknown stamp must never be fresh");
        assert!(check.stale_reason.is_some());
    }

    #[test]
    fn empty_stamp_is_not_fresh() {
        let dir = init_repo("empty");
        let check = freshness_check(&dir, &StampedFreshness::default()).expect("check");
        assert!(
            !check.fresh,
            "a comparison with no provided stamp is never fresh"
        );
        assert!(check.stale_reason.is_none());
    }

    /// Property-style: across a matrix of divergences from the true current
    /// stamp, a stale stamp is NEVER reported fresh. Only the exact-match stamp
    /// is fresh.
    #[test]
    fn stale_stamp_is_never_reported_fresh() {
        let dir = init_repo("property");
        let current = freshness(&dir).expect("stamp");
        let bad = "fnv1a64:ffffffffffffffff".to_string();

        // Every combination where at least one provided stamp is wrong must be
        // not-fresh; the all-correct combination must be fresh.
        for head_wrong in [false, true] {
            for worktree_wrong in [false, true] {
                for index_wrong in [false, true] {
                    let stamped = StampedFreshness {
                        head_hash: Some(if head_wrong {
                            bad.clone()
                        } else {
                            current.head_hash.clone().unwrap()
                        }),
                        worktree_hash: Some(if worktree_wrong {
                            bad.clone()
                        } else {
                            current.worktree_hash.clone()
                        }),
                        index_hash: Some(if index_wrong {
                            bad.clone()
                        } else {
                            current.index_hash.clone()
                        }),
                    };
                    let check = freshness_check(&dir, &stamped).expect("check");
                    let any_wrong = head_wrong || worktree_wrong || index_wrong;
                    if any_wrong {
                        assert!(
                            !check.fresh,
                            "stale stamp reported fresh: head={head_wrong} worktree={worktree_wrong} index={index_wrong}"
                        );
                        assert!(check.stale_reason.is_some());
                    } else {
                        assert!(check.fresh, "exact-match stamp must be fresh");
                    }
                }
            }
        }
    }

    #[test]
    fn codegraph_query_paging_reports_true_total_and_slices() {
        let dir = temp_workspace("paging");
        // Five matching symbols spread over two files, all containing "sym".
        fs::write(
            dir.join("src/a.rs"),
            "pub fn sym_one() {}\npub fn sym_two() {}\npub fn sym_three() {}\n",
        )
        .expect("a");
        fs::write(
            dir.join("src/b.rs"),
            "pub fn sym_four() {}\npub fn sym_five() {}\n",
        )
        .expect("b");

        let page1 = codegraph_query(&dir, "sym", 2, 0).expect("page1");
        assert_eq!(page1.total, 5, "true total across all pages");
        assert_eq!(page1.limit, 2);
        assert_eq!(page1.offset, 0);
        assert_eq!(page1.records.len(), 2, "first page is a 2-record slice");

        let page2 = codegraph_query(&dir, "sym", 2, 2).expect("page2");
        assert_eq!(page2.total, 5);
        assert_eq!(page2.records.len(), 2);

        let page3 = codegraph_query(&dir, "sym", 2, 4).expect("page3");
        assert_eq!(page3.total, 5);
        assert_eq!(page3.records.len(), 1, "last page is the remainder");

        // Pages are disjoint and ordered: no record repeats across pages.
        let mut all: Vec<_> = page1
            .records
            .iter()
            .chain(page2.records.iter())
            .chain(page3.records.iter())
            .map(|record| (record.path.clone(), record.line, record.symbol.clone()))
            .collect();
        let before = all.len();
        all.sort();
        all.dedup();
        assert_eq!(before, all.len(), "paged slices must not overlap");

        // An offset past the end is an empty slice but the true total stands.
        let past = codegraph_query(&dir, "sym", 2, 99).expect("past");
        assert_eq!(past.total, 5);
        assert!(past.records.is_empty());

        // The freshness stamp rides along.
        assert!(page1.freshness.index_hash.starts_with("fnv1a64:"));
    }

    #[test]
    fn blank_codegraph_query_returns_empty_page_without_building_index() {
        let dir = temp_workspace("blank-query");
        fs::write(dir.join("src/a.rs"), "pub fn sym_one() {}\n").expect("a");

        let page = codegraph_query(&dir, "   \n", 50, 25).expect("blank page");

        assert_eq!(page.total, 0);
        assert_eq!(page.limit, 50);
        assert_eq!(
            page.offset, 0,
            "blank queries always reset to the empty first page"
        );
        assert!(page.records.is_empty());
        assert!(
            !opensks_codegraph::index_path(&dir).exists(),
            "blank queries must not build or persist a codegraph index"
        );
        assert!(page.freshness.index_hash.starts_with("fnv1a64:"));
    }
}
