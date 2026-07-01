use std::path::PathBuf;

use crate::CliError;

#[derive(Debug, Default)]
pub(crate) struct ConversationCommandOptions {
    pub(crate) workspace: Option<PathBuf>,
    pub(crate) conversation: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) filter: Option<String>,
    pub(crate) limit: Option<usize>,
    pub(crate) before_sequence: Option<i64>,
    pub(crate) after_sequence: Option<i64>,
    pub(crate) role: Option<String>,
    pub(crate) text: Option<String>,
    pub(crate) kind: Option<String>,
    pub(crate) state: Option<String>,
    pub(crate) payload: Option<String>,
    pub(crate) idempotency_key: Option<String>,
    pub(crate) settings: Option<String>,
    pub(crate) supervisor_id: Option<String>,
    pub(crate) lease_ttl_ms: Option<u64>,
    pub(crate) force: bool,
}

pub(crate) fn conversation_usage() -> &'static str {
    concat!(
        "usage: opensks conversation list --workspace <path> [--filter all|running|pinned|archived] [--limit N]\n",
        "       opensks conversation create --workspace <path> --title \"<title>\"\n",
        "       opensks conversation rename --workspace <path> --conversation <id> --title \"<title>\"\n",
        "       opensks conversation pin|unpin|archive|unarchive --workspace <path> --conversation <id>\n",
        "       opensks conversation delete --workspace <path> --conversation <id> --force\n",
        "       opensks conversation fork --workspace <path> --conversation <id> [--after-sequence S]\n",
        "       opensks conversation messages --workspace <path> --conversation <id> [--before-sequence S] [--limit N]\n",
        "       opensks conversation append --workspace <path> --conversation <id> --role user|assistant|system --text \"<text>\"\n",
        "       opensks conversation turn-start --workspace <path> --conversation <id> --text \"<text>\" [--idempotency-key <key>]\n",
        "       opensks conversation supervisor-tick --workspace <path> [--supervisor-id <id>] [--lease-ttl-ms N]\n",
        "       opensks conversation runs --workspace <path> --conversation <id>\n",
        "       opensks conversation timeline --workspace <path> --conversation <id> [--limit N]\n",
        "       opensks conversation timeline-append --workspace <path> --conversation <id> --kind <kind> [--state <state>] --payload '<json>'\n",
        "       opensks conversation receipt-event-append --workspace <path> --conversation <id> --kind git_commit_receipt|git_push_receipt|git_push_failed --idempotency-key <key> --payload '<json>'\n",
        "       opensks conversation settings-get --workspace <path> --conversation <id>\n",
        "       opensks conversation settings-set --workspace <path> --conversation <id> --settings '<json>'\n"
    )
}

pub(crate) fn parse_conversation_options(
    args: &[String],
) -> Result<ConversationCommandOptions, CliError> {
    let mut options = ConversationCommandOptions::default();
    let mut idx = 0;
    while idx < args.len() {
        let flag = args[idx].as_str();
        match flag {
            "--workspace" => {
                options.workspace = Some(PathBuf::from(conversation_flag_value(args, idx, flag)?));
                idx += 2;
            }
            "--conversation" => {
                options.conversation = Some(conversation_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--title" => {
                options.title = Some(conversation_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--filter" => {
                options.filter = Some(conversation_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--limit" => {
                options.limit = Some(conversation_parse_usize(args, idx, flag)?);
                idx += 2;
            }
            "--before-sequence" => {
                options.before_sequence = Some(conversation_parse_i64(args, idx, flag)?);
                idx += 2;
            }
            "--after-sequence" => {
                options.after_sequence = Some(conversation_parse_i64(args, idx, flag)?);
                idx += 2;
            }
            "--role" => {
                options.role = Some(conversation_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--text" => {
                options.text = Some(conversation_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--kind" => {
                options.kind = Some(conversation_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--state" => {
                options.state = Some(conversation_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--payload" => {
                options.payload = Some(conversation_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--idempotency-key" => {
                options.idempotency_key =
                    Some(conversation_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--settings" => {
                options.settings = Some(conversation_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--supervisor-id" => {
                options.supervisor_id = Some(conversation_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--lease-ttl-ms" => {
                let raw = conversation_flag_value(args, idx, flag)?;
                options.lease_ttl_ms = Some(
                    raw.parse::<u64>()
                        .map_err(|_| CliError::Usage(conversation_usage().to_string()))?,
                );
                idx += 2;
            }
            "--force" => {
                options.force = true;
                idx += 1;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown conversation argument `{other}`\n\n{}",
                    conversation_usage()
                )));
            }
        }
    }
    Ok(options)
}

pub(crate) fn require_conversation_field<'a>(
    value: Option<&'a str>,
    flag: &str,
) -> Result<&'a str, CliError> {
    value.ok_or_else(|| {
        CliError::Usage(format!(
            "conversation command requires `{flag}`\n\n{}",
            conversation_usage()
        ))
    })
}

pub(crate) fn parse_conversation_filter(
    value: Option<&str>,
) -> Result<opensks_contracts::ConversationFilter, CliError> {
    match value.unwrap_or("all") {
        "all" => Ok(opensks_contracts::ConversationFilter::All),
        "running" => Ok(opensks_contracts::ConversationFilter::Running),
        "pinned" => Ok(opensks_contracts::ConversationFilter::Pinned),
        "archived" => Ok(opensks_contracts::ConversationFilter::Archived),
        other => Err(CliError::Usage(format!(
            "unknown conversation filter `{other}`\n\n{}",
            conversation_usage()
        ))),
    }
}

pub(crate) fn parse_conversation_role(
    value: Option<&str>,
) -> Result<opensks_contracts::MessageRole, CliError> {
    match require_conversation_field(value, "--role")? {
        "system" => Ok(opensks_contracts::MessageRole::System),
        "user" => Ok(opensks_contracts::MessageRole::User),
        "assistant" => Ok(opensks_contracts::MessageRole::Assistant),
        "tool" => Ok(opensks_contracts::MessageRole::Tool),
        "event" => Ok(opensks_contracts::MessageRole::Event),
        other => Err(CliError::Usage(format!(
            "unknown conversation role `{other}`\n\n{}",
            conversation_usage()
        ))),
    }
}

pub(crate) fn parse_timeline_kind(
    value: Option<&str>,
) -> Result<opensks_contracts::TimelineItemKind, CliError> {
    let raw = require_conversation_field(value, "--kind")?;
    serde_json::from_value(serde_json::Value::String(raw.to_string())).map_err(|_| {
        CliError::Usage(format!(
            "unknown conversation timeline kind `{raw}`\n\n{}",
            conversation_usage()
        ))
    })
}

fn conversation_flag_value<'a>(
    args: &'a [String],
    idx: usize,
    flag: &str,
) -> Result<&'a str, CliError> {
    args.get(idx + 1).map(String::as_str).ok_or_else(|| {
        CliError::Usage(format!(
            "conversation flag `{flag}` requires a value\n\n{}",
            conversation_usage()
        ))
    })
}

fn conversation_parse_usize(args: &[String], idx: usize, flag: &str) -> Result<usize, CliError> {
    conversation_flag_value(args, idx, flag)?
        .parse::<usize>()
        .map_err(|_| {
            CliError::Usage(format!(
                "conversation flag `{flag}` expects a non-negative integer\n\n{}",
                conversation_usage()
            ))
        })
}

fn conversation_parse_i64(args: &[String], idx: usize, flag: &str) -> Result<i64, CliError> {
    conversation_flag_value(args, idx, flag)?
        .parse::<i64>()
        .map_err(|_| {
            CliError::Usage(format!(
                "conversation flag `{flag}` expects an integer\n\n{}",
                conversation_usage()
            ))
        })
}
