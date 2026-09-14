//! R6 measurement harness: bounded storage and output.
//!
//! Run it against the real writer, store and open path:
//!
//! ```sh
//! ION_MEASURE_TASKS=100000 cargo run --release -p ion-core --example storage_measure
//! ```
//!
//! It reports what the acceptance gate in `ROADMAP.md` asks for rather than
//! asserting it: resident memory, commit latency as the resident set grows,
//! database and WAL bytes, restart latency, cancellation latency on a tiny
//! active set, the cost of deep history inheritance and the cost of one large
//! output payload. Latency windows are reported at fractions of the run so
//! growth with resident size is visible instead of averaged away.
//!
//! Environment: `ION_MEASURE_TASKS` (default 2000), `ION_MEASURE_ROOT` (default
//! a fresh directory under the system temp directory), `ION_MEASURE_OUTPUT_BYTES`
//! (default 1 MiB), `ION_MEASURE_FORK_DEPTH` (default 32),
//! `ION_MEASURE_SEED_ENTRIES` (default 256, entries appended to the root before
//! the fork chain runs).
//!
//! This is a measurement tool, not a test: it asserts nothing, prints numbers and
//! is meant to be run by hand before and after a storage change.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ion_ai::{Content, Message, Role};
use ion_core::conversation::context::ContextControl;
use ion_core::{
    CloseMode, ConversationSpec, DriveOutcome, EntryKind, EntryRequest, InputBody, InputMode,
    InputRequest, InputSender, Session, TaskCompletion, TaskContext, TaskDriver, TaskFuture,
    TaskKind, TaskKindName, TaskRegistry, TaskRequest, TurnTemplate,
};
use serde_json::json;

/// A turn kind that settles immediately, so the harness measures the kernel's
/// commit, residency and reload paths rather than model or tool latency.
struct Noop;

impl TaskKind for Noop {
    fn execute<'a>(
        &'a self,
        _task: ion_core::RunningTask,
        _context: TaskContext,
    ) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::completed(json!("done"))) })
    }

    fn recover<'a>(&'a self, task: ion_core::RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(
        &'a self,
        _task: ion_core::RunningTask,
        _context: ion_core::AbortContext,
    ) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

/// A turn kind whose completion carries a large value, for output-size cost.
struct Bulky {
    bytes: usize,
}

impl TaskKind for Bulky {
    fn execute<'a>(
        &'a self,
        _task: ion_core::RunningTask,
        _context: TaskContext,
    ) -> TaskFuture<'a> {
        let bytes = self.bytes;
        Box::pin(async move {
            Ok(TaskCompletion::completed(json!({
                "blob": "x".repeat(bytes),
            })))
        })
    }

    fn recover<'a>(&'a self, task: ion_core::RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(
        &'a self,
        _task: ion_core::RunningTask,
        _context: ion_core::AbortContext,
    ) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

const KIND: &str = "measure";

fn kind() -> TaskKindName {
    TaskKindName::new(KIND).expect("task kind name")
}

fn registry(output_bytes: usize) -> TaskRegistry {
    let mut registry = TaskRegistry::new();
    registry
        .register(kind(), 1, Arc::new(Noop))
        .expect("register noop");
    registry
        .register(
            kind_name("bulky"),
            1,
            Arc::new(Bulky {
                bytes: output_bytes,
            }),
        )
        .expect("register bulky");
    registry
}

fn kind_name(name: &str) -> TaskKindName {
    TaskKindName::new(name).expect("task kind name")
}

fn driver(db: &Path, output_bytes: usize) -> TaskDriver {
    let session = Session::create(db).expect("create session");
    TaskDriver::new(session, registry(output_bytes)).with_turn_template(TurnTemplate::new(kind()))
}

fn reopen(db: &Path, output_bytes: usize) -> TaskDriver {
    let session = Session::open(db).expect("open session");
    TaskDriver::new(session, registry(output_bytes))
}

fn submission(target: ion_core::ConversationId, text: String) -> InputRequest {
    InputRequest {
        target,
        sender: InputSender::User,
        mode: InputMode::Submit,
        request_key: None,
        body: InputBody::Text(text),
    }
}

fn request(target: ion_core::ConversationId) -> TaskRequest {
    TaskRequest {
        conversation_id: target,
        kind: kind(),
        schema_version: 1,
        input: json!({}),
        dependencies: Vec::new(),
    }
}

fn rss_kb() -> u64 {
    let pid = std::process::id().to_string();
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output();
    match output {
        Ok(output) => String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .unwrap_or(0),
        Err(_) => 0,
    }
}

fn bytes(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn total_bytes(db: &Path) -> u64 {
    bytes(db) + wal_bytes(db) + bytes(&PathBuf::from(format!("{}-shm", db.display())))
}

fn wal_bytes(db: &Path) -> u64 {
    bytes(&PathBuf::from(format!("{}-wal", db.display())))
}

/// Database, WAL and SHM bytes separately, so a reader never infers the WAL
/// from a total.
fn size_report(db: &Path) -> String {
    format!(
        "db={} wal={} shm={}",
        bytes(db),
        wal_bytes(db),
        bytes(&PathBuf::from(format!("{}-shm", db.display())))
    )
}

/// One latency window: sample count plus p50/p95/max in microseconds.
#[derive(Default)]
struct Window {
    samples: Vec<u64>,
}

impl Window {
    fn record(&mut self, elapsed: Duration) {
        self.samples.push(elapsed.as_micros() as u64);
    }

    fn report(&self, label: &str) -> String {
        if self.samples.is_empty() {
            return format!("{label}: no samples");
        }
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        let at = |fraction: f64| sorted[((sorted.len() - 1) as f64 * fraction) as usize];
        format!(
            "{label}: n={} p50={}us p95={}us max={}us",
            sorted.len(),
            at(0.50),
            at(0.95),
            sorted[sorted.len() - 1]
        )
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(default)
}

#[tokio::main]
async fn main() {
    let tasks = env_usize("ION_MEASURE_TASKS", 2000);
    let output_bytes = env_usize("ION_MEASURE_OUTPUT_BYTES", 1024 * 1024);
    let fork_depth = env_usize("ION_MEASURE_FORK_DEPTH", 32);
    let db = match std::env::var("ION_MEASURE_ROOT") {
        Ok(root) => {
            let dir = PathBuf::from(root);
            std::fs::create_dir_all(&dir).expect("measure root");
            dir.join("measure.sqlite3")
        }
        Err(_) => {
            let dir = std::env::temp_dir().join(format!("ion-measure-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("temp dir");
            dir.join("measure.sqlite3")
        }
    };
    for path in [db.clone(), PathBuf::from(format!("{}-wal", db.display()))] {
        let _ = std::fs::remove_file(path);
    }
    let _ = std::fs::remove_file(format!("{}.lock", db.display()));

    println!("database: {}", db.display());
    println!("scale: tasks={tasks} output_bytes={output_bytes} fork_depth={fork_depth}");
    println!("rss before run: {} kB", rss_kb());

    let driver = driver(&db, output_bytes);
    let root = driver.snapshot().await.root_conversation;

    // Phase 1: terminal turns, with latency windows at fractions of the run so
    // growth with resident size is visible.
    let mut windows: Vec<(usize, Window)> = Vec::new();
    let mut window = Window::default();
    let mut admit = Window::default();
    let mut settle = Window::default();
    let mut deadline = tasks / 10;
    let build_start = Instant::now();
    let mut peak_rss = rss_kb();
    for index in 0..tasks {
        if index >= deadline && deadline > 0 {
            windows.push((index, std::mem::take(&mut window)));
            deadline += tasks / 10;
        }
        let turn_started = Instant::now();
        let receipt = driver
            .submit_input(submission(root, format!("turn {index}")), request(root))
            .await
            .expect("submit");
        let turn = receipt.task_id.expect("turn root");
        admit.record(turn_started.elapsed());
        let settle_started = Instant::now();
        match driver.drive_task(turn).await.expect("drive") {
            DriveOutcome::Settled(_) => {}
            DriveOutcome::Interrupted(interruption) => {
                panic!("unexpected interruption: {interruption:?}")
            }
        }
        settle.record(settle_started.elapsed());
        window.record(turn_started.elapsed());
        if index % 512 == 0 {
            peak_rss = peak_rss.max(rss_kb());
        }
    }
    windows.push((tasks, window));
    let build = build_start.elapsed();
    let summary = driver.summary().await;
    println!("--- build ---");
    println!(
        "turns: {} in {:?} ({:.0} turns/sec)",
        tasks,
        build,
        tasks as f64 / build.as_secs_f64()
    );
    for (at, window) in &windows {
        println!("  [{} turns] {}", at, window.report("submit+drive"));
    }
    println!("{}", admit.report("  admission only"));
    println!("{}", settle.report("  settlement only"));
    // `summary` is the bounded surface: counting with `snapshot` would clone the
    // whole resident state and inflate the RSS reading below.
    println!(
        "resident records: conversations={} entries={} inputs={} tasks={}",
        summary.conversations,
        summary.entries,
        summary.inputs,
        summary.tasks.pending + summary.tasks.running + summary.tasks.terminal
    );
    // A complete transcript read in bounded pages. Generation reads this way,
    // so the total must scale with the transcript, not with its square.
    {
        const PAGE: usize = 64;
        let started = Instant::now();
        let mut cursor = None;
        let mut pages = 0usize;
        let mut read = 0usize;
        loop {
            let page = driver
                .conversation_entries(root, cursor, PAGE)
                .await
                .expect("page");
            pages += 1;
            read += page.entries.len();
            match page.next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        let elapsed = started.elapsed();
        println!(
            "full transcript read in pages of {PAGE}: {read} entries, {pages} pages, {elapsed:?} ({:.1} us/page)",
            elapsed.as_micros() as f64 / pages as f64
        );
    }
    println!("rss after build: {} kB", rss_kb());
    println!(
        "bytes after build: {} (db={})",
        total_bytes(&db),
        bytes(&db)
    );
    let half = tasks / 2;
    if half > 0 {
        println!("--- tail latency over a large resident set ---");
        let mut tail = Window::default();
        for index in 0..64 {
            let started = Instant::now();
            let receipt = driver
                .submit_input(submission(root, format!("tail {index}")), request(root))
                .await
                .expect("submit");
            let turn = receipt.task_id.expect("turn root");
            driver.drive_task(turn).await.expect("drive");
            tail.record(started.elapsed());
        }
        println!("{}", tail.report("submit+drive"));
    }

    // Phase 2: restart. Reopening reconstructs resident state, so measure it
    // against the same database the latency windows just grew.
    println!("--- restart ---");
    let before_close = rss_kb();
    driver.close(CloseMode::Graceful).await;
    drop(driver);
    let opened = Instant::now();
    let reopened = reopen(&db, output_bytes);
    let open_elapsed = opened.elapsed();
    let after_open = rss_kb();
    println!(
        "open: {open_elapsed:?} (rss before close {before_close} kB, after open {after_open} kB)"
    );
    println!(
        "bytes after close: {} (db={})",
        total_bytes(&db),
        bytes(&db)
    );

    // Phase 3: cancellation latency with a tiny active set over that history.
    println!("--- cancellation with a tiny active set ---");
    let mut cancel = Window::default();
    for _ in 0..16 {
        let receipt = reopened
            .submit_input(submission(root, "cancel me".to_owned()), request(root))
            .await
            .expect("submit");
        let turn = receipt.task_id.expect("turn root");
        // Admitted, never driven: cancelling a pending turn is the durable
        // cancellation mark plus the abort invocation's settlement.
        let started = Instant::now();
        reopened.cancel_turn(turn).await.expect("cancel");
        let settled = tokio::time::timeout(Duration::from_secs(30), reopened.wait_task(turn))
            .await
            .unwrap_or_else(|_| panic!("cancelled turn {turn:?} never settled"));
        assert!(
            settled.is_ok_and(|record| matches!(
                record.status,
                ion_core::TaskStatus::Terminal(ion_core::TaskOutcome {
                    kind: ion_core::TaskOutcomeKind::Aborted,
                    ..
                })
            )),
            "a cancelled turn settles aborted"
        );
        cancel.record(started.elapsed());
    }
    println!("{}", cancel.report("cancel_turn (pending)"));

    // Phase 4: deep inheritance on its own database. Conversation creation is a
    // session-level command, and each fork inherits its parent's whole history,
    // so per-level cost grows with depth.
    println!("--- deep fork ---");
    let fork_db = db.with_extension("forks.sqlite3");
    let _ = std::fs::remove_file(&fork_db);
    let _ = std::fs::remove_file(format!("{fork_db:?}.lock"));
    let mut fork_session = Session::create(&fork_db).expect("fork session");
    let fork_root = fork_session.root_conversation();
    let mut cutoff = append_note(&mut fork_session, fork_root, 0);
    let mut parent = fork_root;
    let seeded = env_usize("ION_MEASURE_SEED_ENTRIES", 256);
    // Spreading the same total number of appends over K conversations separates
    // per-conversation history work from whole-resident-set work: if cost per
    // append drops as K rises, a per-conversation scan dominates.
    let seed_conversations = env_usize("ION_MEASURE_SEED_CONVERSATIONS", 1).max(1);
    let mut seed_targets = vec![fork_root];
    for _ in 1..seed_conversations {
        seed_targets.push(
            fork_session
                .create_conversation(ConversationSpec::independent())
                .expect("seed conversation")
                .conversation_id,
        );
    }
    // A plain append is one commit that clones the resident maps and scans no
    // tasks and no history, so what remains is map-clone cost and storage.
    let mut seeding = Vec::new();
    let mut seed_window = Window::default();
    let mut seed_deadline = seeded / 4;
    for index in 0..seeded {
        if index >= seed_deadline && seed_deadline > 0 {
            seeding.push((index, std::mem::take(&mut seed_window)));
            seed_deadline += seeded / 4;
        }
        let started = Instant::now();
        let target = seed_targets[index % seed_targets.len()];
        let receipt = append_note(&mut fork_session, target, index + 1);
        if target == fork_root {
            cutoff = receipt;
        }
        seed_window.record(started.elapsed());
    }
    seeding.push((seeded, seed_window));
    println!("seeded {seeded} entries across {seed_conversations} conversation(s) before forking");
    for (at, window) in &seeding {
        println!(
            "  [{} entries] {}",
            at,
            window.report("append_entry commit")
        );
    }
    let mut fork = Window::default();
    let mut fork_windows: Vec<(usize, Window)> = Vec::new();
    let mut fork_window = Window::default();
    let mut fork_deadline = (fork_depth / 8).max(1);
    let mut fork_rss = rss_kb();
    for level in 0..fork_depth {
        if level >= fork_deadline {
            fork_windows.push((level, std::mem::take(&mut fork_window)));
            fork_deadline += fork_deadline.max(1);
        }
        let started = Instant::now();
        let receipt = fork_session
            .create_conversation(ConversationSpec::fork(parent, cutoff))
            .expect("fork");
        let elapsed = started.elapsed();
        fork.record(elapsed);
        fork_window.record(elapsed);
        cutoff = append_note(&mut fork_session, receipt.conversation_id, level);
        parent = receipt.conversation_id;
        fork_rss = fork_rss.max(rss_kb());
        if level % 8 == 7 || level + 1 == fork_depth {
            println!(
                "  [depth {}] rss {} kB, bytes {}",
                level + 1,
                fork_rss,
                total_bytes(&fork_db)
            );
        }
    }
    fork_windows.push((fork_depth, fork_window));
    for (at, window) in &fork_windows {
        println!("  [depth <={at}] {}", window.report("fork"));
    }
    println!(
        "{}",
        fork.report("create_conversation (fork at inherited history)")
    );
    drop(fork_session);
    let fork_open = Instant::now();
    let reopened_forks = Session::open(&fork_db).expect("reopen forks");
    println!(
        "reopen with {} deep inheriting conversations: {:?} (rss {} kB)",
        fork_depth,
        fork_open.elapsed(),
        rss_kb()
    );
    drop(reopened_forks);

    // Phase 5: one large output payload, and what it costs to reopen with it.
    println!("--- large output ---");
    let bulky = kind_name("bulky");
    let started = Instant::now();
    let receipt = reopened
        .submit_input(
            submission(root, "large output".to_owned()),
            TaskRequest {
                conversation_id: root,
                kind: bulky.clone(),
                schema_version: 1,
                input: json!({}),
                dependencies: Vec::new(),
            },
        )
        .await
        .expect("submit bulk");
    let turn = receipt.task_id.expect("turn root");
    match reopened.drive_task(turn).await.expect("drive") {
        DriveOutcome::Settled(_) => println!(
            "one {output_bytes}-byte output settled in {:?}",
            started.elapsed()
        ),
        DriveOutcome::Interrupted(interruption) => panic!("interrupted: {interruption:?}"),
    }
    reopened.close(CloseMode::Graceful).await;
    drop(reopened);
    let open_started = Instant::now();
    let reopened_again = reopen(&db, output_bytes);
    println!(
        "open alone with the large payload: {:?}",
        open_started.elapsed()
    );
    println!("bytes at end: {} ({})", total_bytes(&db), size_report(&db));
    println!("rss at end: {} kB", rss_kb());
    reopened_again.close(CloseMode::Graceful).await;
}

/// Append one plain user entry, returning its id for use as the next cutoff.
fn append_note(
    session: &mut Session,
    conversation: ion_core::ConversationId,
    index: usize,
) -> ion_core::EntryId {
    let text = format!("note {index}");
    session
        .append_entry(EntryRequest {
            conversation_id: conversation,
            kind: EntryKind::new("user").expect("entry kind"),
            data: json!({"text": text}),
            projection: vec![Message {
                role: Role::User,
                content: vec![Content::Text(text)],
                provider_replay: None,
            }],
            context: ContextControl::none(),
        })
        .expect("append entry")
        .entry_id
}
