use std::fs;
use std::path::Path;

use crate::{CliError, CliOutput, write_text_atomic};

pub fn run_graph_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    let subcommand = args
        .first()
        .ok_or_else(|| CliError::Usage(graph_usage().to_string()))?;
    match subcommand.as_str() {
        "templates" => {
            let written = opensks_graph::write_default_templates(cwd)
                .map_err(|error| CliError::Invalid(format!("write graph templates: {error}")))?;
            Ok(CliOutput {
                stdout: format!(
                    "wrote default pipeline graph templates\ntemplates: {}\n",
                    written.len()
                ),
            })
        }
        "compile" => {
            let template_id = args
                .get(1)
                .map(String::as_str)
                .unwrap_or("single-model-safe");
            let graph = opensks_graph::default_templates()
                .into_iter()
                .find(|graph| graph.id == template_id)
                .ok_or_else(|| {
                    CliError::Usage(format!(
                        "unknown graph template `{template_id}`\n\n{}",
                        graph_usage()
                    ))
                })?;
            let plan = opensks_graph::compile_graph(&graph);
            let dir = cwd.join(".opensks").join("pipelines").join("compiled");
            fs::create_dir_all(&dir)?;
            let artifact = dir.join(format!("{template_id}.plan.json"));
            write_text_atomic(
                &artifact,
                &(serde_json::to_string_pretty(&plan).map_err(|error| {
                    CliError::Invalid(format!("serialize compiled graph plan: {error}"))
                })? + "\n"),
            )?;
            let error_count = plan
                .diagnostics
                .iter()
                .filter(|item| item.severity == opensks_contracts::DiagnosticSeverity::Error)
                .count();
            Ok(CliOutput {
                stdout: format!(
                    "compiled pipeline graph\nid: {}\nplan_hash: {}\ndiagnostics_errors: {}\nartifact: {}\n",
                    plan.graph_id,
                    plan.plan_hash,
                    error_count,
                    artifact.display()
                ),
            })
        }
        "plan" => {
            let request = parse_graph_plan_request(&args[1..])?;
            let mut planned = opensks_graph::plan_graph_from_objective(&request);
            let graph_dir = cwd.join(".opensks").join("pipelines").join("objective");
            let compiled_dir = cwd.join(".opensks").join("pipelines").join("compiled");
            let graph_artifact = graph_dir.join(format!("{}.graph.json", planned.graph.id));
            let plan_artifact = compiled_dir.join(format!("{}.plan.json", planned.graph.id));
            let receipt_artifact =
                graph_dir.join(format!("{}.objective-plan-receipt.json", planned.graph.id));
            planned.receipt.graph_ref = Some(workspace_relative_display(cwd, &graph_artifact));
            planned.receipt.compiled_plan_ref =
                Some(workspace_relative_display(cwd, &plan_artifact));
            write_text_atomic(
                &graph_artifact,
                &(serde_json::to_string_pretty(&planned.graph).map_err(|error| {
                    CliError::Invalid(format!("serialize objective graph: {error}"))
                })? + "\n"),
            )?;
            write_text_atomic(
                &plan_artifact,
                &(serde_json::to_string_pretty(&planned.compiled_plan).map_err(|error| {
                    CliError::Invalid(format!("serialize objective compiled plan: {error}"))
                })? + "\n"),
            )?;
            write_text_atomic(
                &receipt_artifact,
                &(serde_json::to_string_pretty(&planned.receipt).map_err(|error| {
                    CliError::Invalid(format!("serialize objective plan receipt: {error}"))
                })? + "\n"),
            )?;
            let error_count = planned
                .compiled_plan
                .diagnostics
                .iter()
                .filter(|item| item.severity == opensks_contracts::DiagnosticSeverity::Error)
                .count();
            Ok(CliOutput {
                stdout: format!(
                    "planned objective pipeline graph\nid: {}\nplan_hash: {}\ndiagnostics_errors: {}\nwork_templates: {}\nreceipt: {}\nartifact: {}\n",
                    planned.graph.id,
                    planned.compiled_plan.plan_hash,
                    error_count,
                    planned.compiled_plan.work_templates.len(),
                    receipt_artifact.display(),
                    plan_artifact.display()
                ),
            })
        }
        other => Err(CliError::Usage(format!(
            "unknown graph subcommand `{other}`\n\n{}",
            graph_usage()
        ))),
    }
}

pub fn graph_usage() -> &'static str {
    concat!(
        "usage: opensks graph templates\n",
        "       opensks graph compile [single-model-safe|balanced-multi-model|extreme-parallel|image-heavy-product-build|research-report]\n",
        "       opensks graph plan [--max-parallelism <n>] [--roles <n>] [--image] [--research] [--no-worktree] <objective>\n"
    )
}

fn parse_graph_plan_request(
    args: &[String],
) -> Result<opensks_contracts::ObjectivePlanRequest, CliError> {
    let mut max_parallelism = None;
    let mut role_count = None;
    let mut include_image_lane = false;
    let mut include_research_lane = false;
    let mut require_git_worktree = true;
    let mut objective = Vec::new();
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--max-parallelism" => {
                max_parallelism = Some(parse_graph_plan_u32(
                    args.get(idx + 1),
                    "--max-parallelism",
                )?);
                idx += 2;
            }
            "--roles" => {
                role_count = Some(parse_graph_plan_u32(args.get(idx + 1), "--roles")?);
                idx += 2;
            }
            "--image" => {
                include_image_lane = true;
                idx += 1;
            }
            "--research" => {
                include_research_lane = true;
                idx += 1;
            }
            "--no-worktree" => {
                require_git_worktree = false;
                idx += 1;
            }
            other if other.starts_with("--") => {
                return Err(CliError::Usage(format!(
                    "unknown graph plan argument `{other}`\n\n{}",
                    graph_usage()
                )));
            }
            _ => {
                objective.extend(args[idx..].iter().cloned());
                break;
            }
        }
    }
    let objective = objective.join(" ").trim().to_string();
    if objective.is_empty() {
        return Err(CliError::Usage(graph_usage().to_string()));
    }
    let mut request = opensks_contracts::ObjectivePlanRequest::new(objective);
    if let Some(value) = max_parallelism {
        request.max_parallelism = value;
    }
    if let Some(value) = role_count {
        request.role_count = value;
    }
    request.include_image_lane = include_image_lane;
    request.include_research_lane = include_research_lane;
    request.require_git_worktree = require_git_worktree;
    request.evidence_refs = vec!["cli:graph-plan".to_string()];
    Ok(request)
}

fn parse_graph_plan_u32(value: Option<&String>, flag: &str) -> Result<u32, CliError> {
    value
        .ok_or_else(|| {
            CliError::Usage(format!(
                "flag `{flag}` requires a value\n\n{}",
                graph_usage()
            ))
        })?
        .parse::<u32>()
        .map_err(|_| {
            CliError::Usage(format!(
                "flag `{flag}` must be a number\n\n{}",
                graph_usage()
            ))
        })
}

fn workspace_relative_display(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}
