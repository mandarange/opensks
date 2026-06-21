use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn daemon_binary_streams_response_before_stdin_eof() {
    let workspace =
        std::env::temp_dir().join(format!("opensks-daemon-stream-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&workspace);
    std::fs::create_dir_all(&workspace).expect("workspace");

    let mut child = Command::new(env!("CARGO_BIN_EXE_opensks"))
        .args(["daemon", "--stdio", "--workspace"])
        .arg(&workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn daemon");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            let bytes = reader.read_line(&mut line).expect("read line");
            if bytes == 0 {
                break;
            }
            tx.send(line).expect("send line");
        }
    });

    let hello = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("hello before request");
    assert!(hello.contains("\"event_type\":\"engine_hello\""));
    assert!(!hello.contains(workspace.to_string_lossy().as_ref()));

    writeln!(
        stdin,
        "{{\"schema\":\"opensks.engine-request.v1\",\"id\":\"req-stream-health\",\"kind\":\"health\",\"protocol_version\":\"opensks.contracts.v1\",\"params\":{{}}}}"
    )
    .expect("write request");
    stdin.flush().expect("flush request");

    let health = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("health response before stdin eof");
    assert!(health.contains("\"request_id\":\"req-stream-health\""));
    assert!(health.contains("\"event_type\":\"engine_health\""));
    assert!(!health.contains(workspace.to_string_lossy().as_ref()));

    drop(stdin);
    let status = child.wait().expect("wait daemon");
    assert!(status.success());
    reader.join().expect("reader thread");
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn daemon_binary_routes_health_while_tail_subscription_is_pending() {
    let workspace =
        std::env::temp_dir().join(format!("opensks-daemon-tail-route-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&workspace);
    std::fs::create_dir_all(&workspace).expect("workspace");

    let mut child = Command::new(env!("CARGO_BIN_EXE_opensks"))
        .args(["daemon", "--stdio", "--workspace"])
        .arg(&workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn daemon");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            let bytes = reader.read_line(&mut line).expect("read line");
            if bytes == 0 {
                break;
            }
            tx.send(line).expect("send line");
        }
    });

    let hello = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("hello before request");
    assert!(hello.contains("\"event_type\":\"engine_hello\""));

    writeln!(
        stdin,
        "{{\"schema\":\"opensks.engine-request.v1\",\"id\":\"req-tail-slow\",\"kind\":\"subscribe_events\",\"protocol_version\":\"opensks.contracts.v1\",\"params\":{{\"run_id\":\"run-daemon-tail-route\",\"since_sequence\":0,\"tail_ms\":2000,\"poll_interval_ms\":50}}}}"
    )
    .expect("write tail request");
    stdin.flush().expect("flush tail");
    writeln!(
        stdin,
        "{{\"schema\":\"opensks.engine-request.v1\",\"id\":\"req-health-behind-tail\",\"kind\":\"health\",\"protocol_version\":\"opensks.contracts.v1\",\"params\":{{}}}}"
    )
    .expect("write health request");
    stdin.flush().expect("flush health");

    let mut saw_tail_complete_before_health = false;
    let mut saw_health = false;
    for _ in 0..8 {
        let line = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("health response while tail request is still pending");
        if line.contains("\"daemon:subscription-tail-complete\"") {
            saw_tail_complete_before_health = true;
        }
        if line.contains("\"request_id\":\"req-health-behind-tail\"")
            && line.contains("\"event_type\":\"engine_health\"")
        {
            saw_health = true;
            break;
        }
    }
    assert!(
        saw_health,
        "health response was not routed while tail was pending"
    );
    assert!(
        !saw_tail_complete_before_health,
        "tail completion arrived before the routed health response"
    );
    drop(stdin);
    let status = child.wait().expect("wait daemon");
    assert!(status.success());
    reader.join().expect("reader thread");
    let _ = std::fs::remove_dir_all(workspace);
}
