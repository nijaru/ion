//! A bounded private diagnostic artifact, drained independently of protocol IO.
use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::ChildStderr,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const PREFIX_BYTES: usize = 64 * 1024;

#[derive(Default)]
struct State {
    total: u64,
    retained: usize,
    eof: bool,
    error: Option<String>,
}

pub(super) struct Log {
    path: PathBuf,
    state: Mutex<State>,
}

impl Log {
    pub fn snapshot(&self) -> Option<Value> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.total > 0 || state.error.is_some()).then(|| {
            json!({
                "path": self.path,
                "total_bytes": state.total,
                "retained_bytes": state.retained,
                "omitted_bytes": state.total.saturating_sub(state.retained as u64),
                "eof": state.eof,
                "capture_error": state.error,
            })
        })
    }

    pub fn notice(&self, name: &str) -> Option<String> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.total > 0 || state.error.is_some()).then(|| {
            format!(
                "MCP server {name} stderr: {} ({} retained of {} read bytes{})",
                self.path.display(),
                state.retained,
                state.total,
                state
                    .error
                    .as_ref()
                    .map(|error| format!("; capture failed: {error}"))
                    .unwrap_or_default(),
            )
        })
    }

    fn error(&self, error: impl std::fmt::Display) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.error.get_or_insert_with(|| error.to_string());
    }
}

impl Drop for Log {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.total == 0 && state.error.is_none() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Before spawn this removes an unused file; after spawn nonempty artifacts remain.
pub(super) struct Destination {
    file: Option<File>,
    path: PathBuf,
}

impl Destination {
    pub fn create() -> io::Result<Self> {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|error| {
            io::Error::other(format!("cannot name MCP stderr capture: {error}"))
        })?;
        let path = std::env::temp_dir().join(format!(
            "ion-mcp-stderr-{}-{:032x}.log",
            std::process::id(),
            u128::from_ne_bytes(nonce),
        ));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        Ok(Self {
            file: Some(file),
            path,
        })
    }

    pub fn start(mut self, reader: ChildStderr) -> Capture {
        let file =
            tokio::fs::File::from_std(self.file.take().expect("unused diagnostic destination"));
        let log = Arc::new(Log {
            path: self.path.clone(),
            state: Mutex::new(State::default()),
        });
        let stop = CancellationToken::new();
        let worker = tokio::spawn(drain(reader, file, log.clone(), stop.clone()));
        Capture {
            log,
            stop,
            worker: Some(worker),
            result: None,
        }
    }
}

impl Drop for Destination {
    fn drop(&mut self) {
        if self.file.is_some() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub(super) struct Capture {
    pub log: Arc<Log>,
    stop: CancellationToken,
    worker: Option<JoinHandle<()>>,
    result: Option<Result<(), String>>,
}

impl Capture {
    /// Called after the root is reaped; inherited writers cannot hold shutdown open.
    pub async fn finish(&mut self) -> Result<(), String> {
        if let Some(result) = &self.result {
            return result.clone();
        }
        let worker = self.worker.as_mut().expect("unjoined stderr reader");
        let joined = match tokio::time::timeout(Duration::from_millis(100), &mut *worker).await {
            Ok(result) => result,
            Err(_) => {
                self.stop.cancel();
                worker.await
            }
        };
        self.worker.take();
        if let Err(error) = joined {
            self.log.error(format!("stderr reader failed: {error}"));
        }
        let error = self
            .log
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .error
            .clone();
        let result = error.map_or(Ok(()), Err);
        self.result = Some(result.clone());
        result
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // Controlled teardown awaits finish; Drop can request stop, not join it.
        self.stop.cancel();
    }
}

async fn drain(
    mut reader: ChildStderr,
    mut file: tokio::fs::File,
    log: Arc<Log>,
    stop: CancellationToken,
) {
    let mut bytes = [0; 8192];
    let mut retained = 0;
    let mut writable = true;
    loop {
        let read = tokio::select! {
            biased;
            () = stop.cancelled() => break,
            read = reader.read(&mut bytes) => read,
        };
        let count = match read {
            Ok(0) => {
                log.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .eof = true;
                break;
            }
            Ok(count) => count,
            Err(error) => {
                log.error(error);
                break;
            }
        };
        {
            let mut state = log
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.total = state.total.saturating_add(count as u64);
        }
        let keep = count.min(PREFIX_BYTES - retained);
        if writable && keep > 0 {
            let written = async {
                file.write_all(&bytes[..keep]).await?;
                file.flush().await
            }
            .await;
            match written {
                Ok(()) => retained += keep,
                Err(error) => {
                    log.error(error);
                    writable = false;
                    // A failed write may have stored a prefix; report only known file bytes.
                    if let Ok(metadata) = file.metadata().await {
                        retained = (metadata.len().min(PREFIX_BYTES as u64)) as usize;
                    }
                }
            }
            log.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retained = retained;
        }
    }
}
