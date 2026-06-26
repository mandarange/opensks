#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalInputIntent {
    ShellCommand,
    AgentPrompt,
    Empty,
    Ambiguous,
}

pub fn classify_input_intent(input: &str) -> TerminalInputIntent {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return TerminalInputIntent::Empty;
    }
    if trimmed.starts_with('!') {
        return TerminalInputIntent::ShellCommand;
    }
    if trimmed.starts_with("/agent ") || trimmed == "/agent" {
        return TerminalInputIntent::AgentPrompt;
    }
    if starts_like_path(trimmed)
        || contains_shell_operator(trimmed)
        || starts_with_assignment(trimmed)
    {
        return TerminalInputIntent::ShellCommand;
    }

    let first = trimmed.split_whitespace().next().unwrap_or_default();
    if is_known_command(first) {
        return TerminalInputIntent::ShellCommand;
    }

    let words = trimmed.split_whitespace().count();
    if looks_like_agent_prompt(trimmed) {
        return TerminalInputIntent::AgentPrompt;
    }
    if words <= 1 {
        return TerminalInputIntent::Ambiguous;
    }
    if words >= 3 {
        return TerminalInputIntent::AgentPrompt;
    }
    TerminalInputIntent::Ambiguous
}

pub(crate) fn is_known_command(token: &str) -> bool {
    matches!(
        token,
        "git"
            | "cargo"
            | "npm"
            | "pnpm"
            | "yarn"
            | "make"
            | "just"
            | "task"
            | "cd"
            | "ls"
            | "cat"
            | "grep"
            | "rg"
            | "fd"
            | "python"
            | "python3"
            | "node"
            | "swift"
            | "xcodebuild"
            | "docker"
            | "kubectl"
            | "gh"
            | "open"
            | "code"
            | "vim"
            | "nvim"
    )
}

fn starts_like_path(input: &str) -> bool {
    input.starts_with("./")
        || input.starts_with("../")
        || input.starts_with('/')
        || input.starts_with('~')
}

fn contains_shell_operator(input: &str) -> bool {
    ["|", ">", "<", "&&", "||", ";"]
        .iter()
        .any(|operator| input.contains(operator))
}

fn starts_with_assignment(input: &str) -> bool {
    let Some(first) = input.split_whitespace().next() else {
        return false;
    };
    let Some((name, value)) = first.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && !value.is_empty()
        && name
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_uppercase() || ch.is_ascii_digit())
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_ascii_uppercase())
}

fn looks_like_agent_prompt(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    let has_prompt_keyword = [
        "왜",
        "고쳐",
        "분석",
        "추천",
        "설명",
        "해줘",
        "please",
        "fix",
        "explain",
        "why",
        "analyze",
        "recommend",
    ]
    .iter()
    .any(|keyword| lower.contains(keyword));
    input.contains('?') || has_prompt_keyword || contains_hangul(input)
}

fn contains_hangul(input: &str) -> bool {
    input
        .chars()
        .any(|ch| ('\u{ac00}'..='\u{d7a3}').contains(&ch))
}
