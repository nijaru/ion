use super::*;
use std::{fs, time::Duration};

fn python(source: &str) -> Command {
    let mut command = Command::new("python3");
    command.args(["-c", source]);
    command
}

#[tokio::test]
async fn replies_are_trimmed_literal_utf8_and_failures_are_value_free() {
    let value = capture(
        python("import sys;sys.stderr.write('PRIVATE_CANARY');print('  ${LITERAL} !literal  ')"),
        128,
        Instant::now() + Duration::from_secs(10),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(value, "${LITERAL} !literal");
    for source in [
        "import sys;print('PRIVATE_CANARY');sys.exit(7)",
        "import sys;sys.stdout.buffer.write(b'\\xffPRIVATE_CANARY')",
        "import sys;sys.stdout.buffer.write(b'\\0PRIVATE_CANARY')",
        "print(' ')",
        "print('PRIVATE_CANARY' * 30)",
    ] {
        let error = capture(
            python(source),
            128,
            Instant::now() + Duration::from_secs(10),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(!format!("{error:#}").contains("PRIVATE_CANARY"));
    }
    let error = capture(
        python("print('PRIVATE_CANARY')"),
        128,
        Instant::now(),
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("timed out"));
}

#[tokio::test]
async fn deadline_reaps_an_exited_root_when_a_descendant_holds_stdout() {
    let root = std::env::temp_dir().join(format!("ion-mcp-helper-pipe-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let path = root.join("pid");
    let mut command = python(
        "import os,sys,time\npid=os.fork()\nif pid==0:time.sleep(60);os._exit(0)\nopen(sys.argv[1],'w').write(str(os.getpid()))\nos._exit(0)",
    );
    command.arg(&path);
    let task = tokio::spawn(capture(
        command,
        128,
        Instant::now() + Duration::from_secs(10),
        CancellationToken::new(),
    ));
    let pid = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(text) = fs::read_to_string(&path)
                && let Ok(pid) = text.parse::<i32>()
            {
                break Pid::from_raw(pid).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(11)).await;
    let error = task.await.unwrap().unwrap_err();
    tokio::time::resume();
    assert!(error.to_string().contains("timed out"));
    assert!(
        rustix::process::test_kill_process(pid).is_err(),
        "expired capture joins its reserved root"
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn dropping_a_capture_cancels_and_reaps_its_started_root() {
    let root = std::env::temp_dir().join(format!("ion-mcp-helper-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let path = root.join("pid");
    let mut command =
        python("import os,sys,time;open(sys.argv[1],'w').write(str(os.getpid()));time.sleep(60)");
    command.arg(&path);
    let task = tokio::spawn(capture(
        command,
        128,
        Instant::now() + Duration::from_secs(10),
        CancellationToken::new(),
    ));
    let pid = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(text) = fs::read_to_string(&path)
                && let Ok(pid) = text.parse::<i32>()
            {
                break Pid::from_raw(pid).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(5), async {
        while rustix::process::test_kill_process(pid).is_ok() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("retained helper cleanup reaps the cancelled root");
    fs::remove_dir_all(root).unwrap();
}
