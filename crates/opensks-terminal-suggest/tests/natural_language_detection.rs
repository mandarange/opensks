use opensks_terminal_suggest::{TerminalInputIntent, TerminalSuggestionEngine};

#[test]
fn classifies_natural_language_as_agent_prompt() {
    let engine = TerminalSuggestionEngine::new(Default::default());
    assert_eq!(
        engine.classify_input_intent("왜 cargo test가 실패해?"),
        TerminalInputIntent::AgentPrompt
    );
    assert_eq!(
        engine.classify_input_intent("/agent 실패 분석해줘"),
        TerminalInputIntent::AgentPrompt
    );
}

#[test]
fn classifies_forced_shell_and_shell_shapes() {
    let engine = TerminalSuggestionEngine::new(Default::default());
    assert_eq!(
        engine.classify_input_intent("!왜"),
        TerminalInputIntent::ShellCommand
    );
    assert_eq!(
        engine.classify_input_intent("git status"),
        TerminalInputIntent::ShellCommand
    );
    assert_eq!(
        engine.classify_input_intent("./script.sh"),
        TerminalInputIntent::ShellCommand
    );
    assert_eq!(
        engine.classify_input_intent("RUST_LOG=debug cargo test"),
        TerminalInputIntent::ShellCommand
    );
    assert_eq!(
        engine.classify_input_intent("cargo test && cargo fmt"),
        TerminalInputIntent::ShellCommand
    );
}

#[test]
fn classifies_short_unknown_token_as_ambiguous() {
    let engine = TerminalSuggestionEngine::new(Default::default());
    assert_eq!(
        engine.classify_input_intent("wat"),
        TerminalInputIntent::Ambiguous
    );
}
