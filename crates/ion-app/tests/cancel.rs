use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::Command,
    thread,
    time::Duration,
};

fn respond(mut connection: TcpStream, finish_reason: &str) {
    connection
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("request timeout");
    let mut request = Vec::new();
    let header_end = loop {
        let mut chunk = [0; 4096];
        let count = connection.read(&mut chunk).expect("request read");
        assert!(count > 0, "request headers ended early");
        request.extend_from_slice(&chunk[..count]);
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break end + 4;
        }
        assert!(request.len() < 64 * 1024, "request headers too large");
    };
    let headers = std::str::from_utf8(&request[..header_end]).expect("request headers UTF-8");
    let length: usize = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length: ")
                .and_then(|value| value.trim().parse().ok())
        })
        .expect("request content length");
    while request.len() - header_end < length {
        let mut chunk = [0; 4096];
        let count = connection.read(&mut chunk).expect("request body read");
        assert!(count > 0, "request body ended early");
        request.extend_from_slice(&chunk[..count]);
    }
    let body = format!(
        "data: {{\"model\":\"synthetic\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"ready\"}},\"finish_reason\":\"{finish_reason}\"}}]}}\n\ndata: [DONE]\n\n"
    );
    write!(connection, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
        .expect("provider response");
}

#[test]
fn headless_cancel_settles_incomplete_turn_and_allows_next_turn() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let state = directory.path().join("state");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&state).expect("state");
    std::fs::create_dir(&workspace).expect("workspace");
    let listener = TcpListener::bind("127.0.0.1:0").expect("provider listener");
    let endpoint = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let server = thread::spawn(move || {
        for finish_reason in ["length", "stop"] {
            let (connection, _) = listener.accept().expect("provider accept");
            respond(connection, finish_reason);
        }
    });
    let base = [
        "--state",
        state.to_str().expect("state path"),
        "--workspace",
        workspace.to_str().expect("workspace path"),
        "--endpoint",
        endpoint.as_str(),
    ];
    let model = [
        "--model",
        "synthetic",
        "--model-input-limit",
        "8192",
        "--model-output-limit",
        "1024",
    ];
    let first = Command::new(env!("CARGO_BIN_EXE_ion"))
        .arg("run")
        .args(base)
        .args(model)
        .arg("first")
        .output()
        .expect("first run");
    assert!(!first.status.success());
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).expect("first JSON");
    assert_eq!(first["exit"], "Parked(IncompleteResponse)");
    let turn = first["turn"].as_i64().expect("turn id").to_string();
    let cancelled = Command::new(env!("CARGO_BIN_EXE_ion"))
        .arg("cancel")
        .args(base)
        .args(["--turn", turn.as_str()])
        .output()
        .expect("cancel");
    assert!(
        cancelled.status.success(),
        "{}",
        String::from_utf8_lossy(&cancelled.stderr)
    );
    let cancelled: serde_json::Value =
        serde_json::from_slice(&cancelled.stdout).expect("cancel JSON");
    assert_eq!(
        cancelled["exit"],
        "Settled(Cancelled { unresolved_attempts: [] })"
    );
    let second = Command::new(env!("CARGO_BIN_EXE_ion"))
        .arg("run")
        .args(base)
        .args(model)
        .arg("second")
        .output()
        .expect("second run");
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).expect("second JSON");
    assert!(
        second["exit"]
            .as_str()
            .unwrap()
            .starts_with("Settled(Completed")
    );
    server.join().expect("provider thread");
}
