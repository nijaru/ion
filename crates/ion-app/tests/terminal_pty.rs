#![cfg(unix)]

use std::{
    fs::File,
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use nix::fcntl::{FcntlArg, FdFlag, fcntl};
use nix::pty::{Winsize, openpty};

#[test]
fn chat_rejects_nonterminal_before_creating_session_state() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let state = directory.path().join("state");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&state).expect("state");
    std::fs::create_dir(&workspace).expect("workspace");
    let result = Command::new(env!("CARGO_BIN_EXE_ion"))
        .args([
            "chat",
            "--state",
            state.to_str().expect("utf8 state"),
            "--workspace",
            workspace.to_str().expect("utf8 workspace"),
            "--endpoint",
            "http://127.0.0.1:9999/v1/chat/completions",
            "--model",
            "synthetic",
            "--model-input-limit",
            "8192",
            "--model-output-limit",
            "1024",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("run without terminal");
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("requires a terminal"));
    assert!(
        std::fs::read_dir(&state)
            .expect("state directory")
            .next()
            .is_none()
    );
}

#[test]
fn terminal_hangup_does_not_request_turn_cancellation() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let state = directory.path().join("state");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&state).expect("state");
    std::fs::create_dir(&workspace).expect("workspace");
    let listener = TcpListener::bind("127.0.0.1:0").expect("provider listener");
    let endpoint = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().expect("address")
    );
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let (accepted_sender, accepted_receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut connection = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(error) => panic!("provider accept: {error}"),
            }
        };
        connection
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        let mut chunk = [0u8; 4096];
        assert!(connection.read(&mut chunk).expect("provider request") > 0);
        accepted_sender.send(()).expect("accepted notice");
        thread::sleep(Duration::from_secs(3));
    });
    let pty = openpty(
        Some(&Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }),
        None,
    )
    .expect("pty");
    fcntl(&pty.master, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).expect("master close on exec");
    let slave = File::from(pty.slave);
    let stdout = slave.try_clone().expect("stdout");
    let stderr = slave.try_clone().expect("stderr");
    let mut child = Command::new(env!("CARGO_BIN_EXE_ion"))
        .args([
            "chat",
            "--state",
            state.to_str().expect("state path"),
            "--workspace",
            workspace.to_str().expect("workspace path"),
            "--endpoint",
            &endpoint,
            "--model",
            "synthetic",
            "--model-input-limit",
            "8192",
            "--model-output-limit",
            "1024",
            "hangup probe",
        ])
        .env("TERM", "xterm-256color")
        .env("OPENAI_API_KEY", "synthetic-key")
        .stdin(Stdio::from(slave))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("spawn ion");
    let mut master = File::from(pty.master);
    let hangup = Arc::new(AtomicBool::new(false));
    let close_on_output = Arc::clone(&hangup);
    let reader = thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        let mut query_tail = Vec::new();
        while let Ok(length) = master.read(&mut chunk) {
            if length == 0 {
                break;
            }
            query_tail.extend_from_slice(&chunk[..length]);
            if query_tail
                .windows(b"\x1b[6n".len())
                .any(|part| part == b"\x1b[6n")
            {
                master.write_all(b"\x1b[2;1R").expect("cursor reply");
                query_tail.clear();
            }
            if close_on_output.load(Ordering::SeqCst) {
                break;
            }
            if query_tail.len() > 16 {
                query_tail.drain(..query_tail.len() - 16);
            }
        }
    });
    accepted_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("provider request");
    hangup.store(true, Ordering::SeqCst);
    reader.join().expect("pty reader");
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if child.try_wait().expect("child status").is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().expect("kill hung child");
            panic!("hangup did not close frontend");
        }
        thread::sleep(Duration::from_millis(20));
    }
    server.join().expect("provider server");
    let output = Command::new(env!("CARGO_BIN_EXE_ion"))
        .args(["inspect", "--state", state.to_str().expect("state path")])
        .output()
        .expect("inspect session");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let snapshot: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("snapshot JSON");
    assert_eq!(
        snapshot["unfinished_turn"]["cancellation"]["requested"], false,
        "{snapshot}"
    );
}

#[test]
fn inline_chat_keeps_paste_as_a_draft_and_restores_terminal() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let state = directory.path().join("state");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&state).expect("state");
    std::fs::create_dir(&workspace).expect("workspace");
    let listener = TcpListener::bind("127.0.0.1:0").expect("provider listener");
    let endpoint = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().expect("address")
    );
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let (request_sender, request_receiver) = mpsc::channel();
    let (finish_sender, finish_receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut connection = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("provider accept: {error}"),
            }
        };
        connection
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("provider read timeout");
        let mut request = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let size = connection.read(&mut chunk).expect("provider request");
            assert!(size > 0, "provider request ended early");
            request.extend_from_slice(&chunk[..size]);
            if let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .expect("content length");
                if request.len() >= header_end + 4 + length {
                    break;
                }
            }
            assert!(request.len() < 1024 * 1024, "bounded request");
        }
        request_sender.send(request).expect("request channel");
        let first = "data: {\"model\":\"synthetic\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"test answer\"},\"finish_reason\":null}]}\n\n";
        let last = "data: {\"model\":\"synthetic\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: {\"model\":\"synthetic\",\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n";
        write!(connection, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{first}", first.len() + last.len()).expect("provider first delta");
        connection.flush().expect("flush first delta");
        finish_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("terminal displayed provisional text");
        connection
            .write_all(last.as_bytes())
            .expect("provider terminal");
    });

    let pty = openpty(
        Some(&Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }),
        None,
    )
    .expect("pty");
    let mut slave = File::from(pty.slave);
    slave
        .write_all(b"shell-old-one\r\nshell-old-two\r\n$ ")
        .expect("preexisting terminal lines");
    let stdout = slave.try_clone().expect("stdout");
    let stderr = slave.try_clone().expect("stderr");
    let mut child = Command::new(env!("CARGO_BIN_EXE_ion"))
        .args([
            "chat",
            "--state",
            state.to_str().expect("utf8 state"),
            "--workspace",
            workspace.to_str().expect("utf8 workspace"),
            "--endpoint",
            &endpoint,
            "--model",
            "synthetic",
            "--model-input-limit",
            "8192",
            "--model-output-limit",
            "1024",
        ])
        .env("TERM", "xterm-256color")
        .env("OPENAI_API_KEY", "synthetic-key")
        .stdin(Stdio::from(slave))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("spawn ion");

    let mut writer = File::from(pty.master);
    let mut reader = writer.try_clone().expect("reader");
    let mut responder = writer.try_clone().expect("cursor responder");
    let (sender, receiver) = mpsc::channel();
    let reader_task = thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        let mut query_tail = Vec::new();
        while let Ok(length) = reader.read(&mut chunk) {
            if length == 0 || sender.send(chunk[..length].to_vec()).is_err() {
                break;
            }
            query_tail.extend_from_slice(&chunk[..length]);
            if query_tail
                .windows(b"\x1b[6n".len())
                .any(|part| part == b"\x1b[6n")
            {
                // The shell prompt was on row three; Ion's separating
                // newline places the new inline region on row four.
                responder.write_all(b"\x1b[4;1R").expect("cursor reply");
                query_tail.clear();
            }
            if query_tail.len() > 16 {
                query_tail.drain(..query_tail.len() - 16);
            }
        }
    });
    let mut output = Vec::new();
    wait_for(&receiver, &mut output, b"Ion ready", Duration::from_secs(5));
    writer
        .write_all(b"\x1b[200~line one\nline two\x1b[201~")
        .expect("paste");
    wait_for(&receiver, &mut output, b"line two", Duration::from_secs(5));
    assert!(
        output
            .windows(b"line one".len())
            .any(|part| part == b"line one")
    );
    assert!(
        request_receiver.try_recv().is_err(),
        "paste must not submit"
    );
    writer.write_all(b"\r").expect("submit");
    let request = request_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("submitted provider request");
    assert!(
        request
            .windows(b"line one\\nline two".len())
            .any(|part| part == b"line one\\nline two")
    );
    wait_for(
        &receiver,
        &mut output,
        b"test answer",
        Duration::from_secs(5),
    );
    assert!(
        !output
            .windows(b"Completed".len())
            .any(|part| part == b"Completed"),
        "text must appear before the provider's terminal event"
    );
    finish_sender.send(()).expect("finish provider response");
    wait_for(&receiver, &mut output, b"Completed", Duration::from_secs(5));
    writer.write_all(b"\x04").expect("quit");

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().expect("kill hung child");
            panic!("inline chat did not quit");
        }
        thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "child exited: {status}");
    server.join().expect("provider server");
    drop(writer);
    reader_task.join().expect("reader thread");
    while let Ok(chunk) = receiver.try_recv() {
        output.extend(chunk);
    }
    assert!(
        output
            .windows(b"\x1b[?2004l".len())
            .any(|part| part == b"\x1b[?2004l")
    );
    let mut screen = vt100::Parser::new(24, 80, 0);
    screen.process(&output);
    let first_rows: Vec<_> = screen.screen().rows(0, 80).collect();
    assert!(first_rows[0].contains("shell-old-one"), "{first_rows:?}");
    assert!(first_rows[1].contains("shell-old-two"), "{first_rows:?}");
}

fn wait_for(
    receiver: &mpsc::Receiver<Vec<u8>>,
    output: &mut Vec<u8>,
    needle: &[u8],
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    while !output.windows(needle.len()).any(|part| part == needle) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let chunk = receiver
            .recv_timeout(remaining)
            .expect("terminal output before timeout");
        output.extend(chunk);
    }
}
