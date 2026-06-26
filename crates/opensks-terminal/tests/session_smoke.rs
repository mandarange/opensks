#![cfg(not(windows))]

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opensks_contracts::TerminalEnvPolicy;
use opensks_terminal::{
    TerminalRuntime, TerminalRuntimeError, TerminalSessionConfig, TerminalSessionStatus,
};

#[test]
fn echo_session_writes_output_and_local_artifacts() {
    let workspace = temp_workspace("echo");
    let runtime = TerminalRuntime::new(&workspace);
    let handle = runtime
        .start_session(config("term-echo", &workspace))
        .expect("start terminal session");

    runtime
        .write_input(&handle.session_id, "echo opensks-terminal-ok\n")
        .expect("write input");

    let output = read_until(&runtime, &handle.session_id, "opensks-terminal-ok");
    assert!(output.contains("opensks-terminal-ok"));

    let snapshot = runtime.stop(&handle.session_id).expect("stop terminal");
    assert_eq!(snapshot.status, TerminalSessionStatus::Exited);

    let session_dir = workspace
        .join(".opensks")
        .join("runtime")
        .join("terminal")
        .join("sessions")
        .join("term-echo");
    assert!(session_dir.join("session.json").exists());
    assert!(session_dir.join("events.jsonl").exists());
    assert!(session_dir.join("blocks.jsonl").exists());
    assert!(session_dir.join("output.raw").exists());

    let block_jsonl = fs::read_to_string(session_dir.join("blocks.jsonl")).expect("blocks jsonl");
    assert!(block_jsonl.contains("stdout_digest"));
    assert!(!block_jsonl.contains("opensks-terminal-ok\nopensks-terminal-ok"));
}

#[test]
fn cwd_is_applied_to_shell_process() {
    let workspace = temp_workspace("pwd");
    let runtime = TerminalRuntime::new(&workspace);
    let handle = runtime
        .start_session(config("term-pwd", &workspace))
        .expect("start terminal session");

    runtime
        .write_input(&handle.session_id, "pwd\n")
        .expect("write input");

    let output = read_until(
        &runtime,
        &handle.session_id,
        workspace.to_string_lossy().as_ref(),
    );
    assert!(output.contains(workspace.to_string_lossy().as_ref()));
    let _ = runtime.stop(&handle.session_id);
}

#[test]
fn invalid_cwd_returns_error() {
    let workspace = temp_workspace("invalid");
    let file = workspace.join("not-a-dir");
    fs::write(&file, "x").expect("write file");
    let runtime = TerminalRuntime::new(&workspace);
    let mut config = config("term-invalid", &workspace);
    config.cwd = file;

    let error = runtime.start_session(config).expect_err("invalid cwd");
    assert!(matches!(error, TerminalRuntimeError::InvalidCwd { .. }));
}

fn config(session_id: &str, workspace: &PathBuf) -> TerminalSessionConfig {
    TerminalSessionConfig {
        session_id: session_id.to_string(),
        cwd: workspace.clone(),
        shell: Some(PathBuf::from("/bin/sh")),
        cols: 80,
        rows: 24,
        env_policy: TerminalEnvPolicy::Minimal,
    }
}

fn read_until(runtime: &TerminalRuntime, session_id: &str, needle: &str) -> String {
    let mut output = String::new();
    for _ in 0..120 {
        for chunk in runtime.read_available(session_id).expect("read output") {
            output.push_str(&chunk.decoded_lossy);
        }
        if output.contains(needle) {
            return output;
        }
        thread::sleep(Duration::from_millis(25));
    }
    output
}

fn temp_workspace(name: &str) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_millis();
    let path = std::env::temp_dir().join(format!("opensks-terminal-{name}-{millis}"));
    fs::create_dir_all(&path).expect("create temp workspace");
    path
}
