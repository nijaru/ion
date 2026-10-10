//! Modal picker custody and its one owned, cooperative filesystem traversal.
use std::path::{Path, PathBuf};

use crate::display_text::{fit_line, push_wrapped};
use anyhow::{Context, Result};
use ignore::WalkBuilder;
use ion_ai::{Message, ModelRef};
use ion_terminal::{KeyCode, KeyEvent, Modifiers};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const MAX_FILES: usize = 20_000;
const MAX_ENTRIES: usize = 100_000;

pub(super) enum PickerValue {
    Session(PathBuf),
    Model(ModelRef),
    ForkBefore {
        turn: u64,
        input: Message,
    },
    File {
        path: String,
        start: usize,
        end: usize,
    },
}

pub(super) struct PickerItem {
    pub label: String,
    pub value: PickerValue,
}

pub(super) struct Picker {
    pub title: &'static str,
    pub query: String,
    pub selected: usize,
    pub items: Vec<PickerItem>,
}

impl Picker {
    pub fn matches(&self) -> Vec<usize> {
        let query = self.query.to_ascii_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                item.label
                    .to_ascii_lowercase()
                    .contains(&query)
                    .then_some(index)
            })
            .collect()
    }
}

struct FileRequest {
    cwd: PathBuf,
    start: usize,
    end: usize,
}

struct DiscoveryJob {
    request: FileRequest,
    stop: CancellationToken,
    task: JoinHandle<Result<Discovery>>,
}

enum Discovery {
    Cancelled,
    Files { paths: Vec<String>, limited: bool },
}

#[derive(Default)]
pub(super) struct Pickers {
    active: Option<Picker>,
    notice: Option<String>,
    // A dismissed worker still owns its handle. New requests replace this single
    // waiting slot, never spawn another traversal before the old one is joined.
    waiting: Option<FileRequest>,
    job: Option<DiscoveryJob>,
}

impl Pickers {
    pub fn active(&self) -> Option<&Picker> {
        self.active.as_ref()
    }

    /// Render against the final budget, after composer and status allocation.
    pub fn rows(&self, width: usize, height: usize) -> Vec<String> {
        let Some(picker) = &self.active else {
            return Vec::new();
        };
        let matching = picker.matches();
        let mut rows = vec![if self.loading() {
            format!("{} · discovering project files…", picker.title)
        } else {
            format!("{} · {} match(es)", picker.title, matching.len())
        }];
        if let Some(notice) = &self.notice {
            push_wrapped(&mut rows, notice, width);
        }
        // At small sizes the selected result outranks metadata: Enter must not
        // select a row clipped away by the query composer or a long notice.
        rows.truncate(height.saturating_sub(usize::from(!matching.is_empty())));
        let visible = height.saturating_sub(rows.len());
        let start = picker.selected.saturating_sub(visible.saturating_sub(1));
        for (index, item) in matching.iter().enumerate().skip(start).take(visible) {
            let marker = if index == picker.selected { '›' } else { ' ' };
            let row = if width > 2 {
                format!(
                    "{marker} {}",
                    fit_line(&picker.items[*item].label, width - 2)
                )
            } else {
                marker.to_string()
            };
            rows.push(row);
        }
        rows
    }

    pub fn loading(&self) -> bool {
        self.waiting.is_some()
            || self
                .job
                .as_ref()
                .is_some_and(|job| !job.stop.is_cancelled())
    }

    pub fn has_job(&self) -> bool {
        self.job.is_some()
    }

    pub fn close(&mut self) {
        self.active = None;
        self.notice = None;
        self.waiting = None;
        if let Some(job) = &self.job {
            job.stop.cancel();
        }
    }

    pub fn open(&mut self, picker: Picker) {
        self.close();
        self.active = Some(picker);
    }

    pub fn open_files(&mut self, cwd: &Path, start: usize, end: usize, query: String) {
        self.open(Picker {
            title: "Choose file",
            query,
            selected: 0,
            items: Vec::new(),
        });
        self.waiting = Some(FileRequest {
            cwd: cwd.to_path_buf(),
            start,
            end,
        });
        self.admit();
    }

    fn admit(&mut self) {
        if self.job.is_some() {
            return;
        }
        if let Some(request) = self.waiting.take() {
            let stop = CancellationToken::new();
            let worker_stop = stop.clone();
            let cwd = request.cwd.clone();
            self.job = Some(DiscoveryJob {
                request,
                stop,
                task: tokio::task::spawn_blocking(move || discover(&cwd, &worker_stop)),
            });
        }
    }

    // Await by reference: losing a select race leaves custody here, including
    // the completed result. Only remove the handle after its join has resolved.
    pub async fn complete(&mut self) -> Option<String> {
        let result = (&mut self.job.as_mut()?.task).await;
        let job = self.job.take().expect("joined discovery is still owned");
        let publish = !job.stop.is_cancelled();
        let notice = match result
            .context("file discovery worker stopped")
            .and_then(|result| result)
        {
            Err(error) => {
                if publish {
                    self.close();
                }
                Some(format!("File discovery failed: {error:#}"))
            }
            Ok(Discovery::Cancelled) => None,
            Ok(Discovery::Files { paths, limited }) if publish => {
                let empty = paths.is_empty();
                if let Some(picker) = &mut self.active {
                    picker.items = paths
                        .into_iter()
                        .map(|path| PickerItem {
                            label: path.clone(),
                            value: PickerValue::File {
                                path,
                                start: job.request.start,
                                end: job.request.end,
                            },
                        })
                        .collect();
                    picker.selected = picker
                        .selected
                        .min(picker.matches().len().saturating_sub(1));
                }
                if empty {
                    self.close();
                }
                if limited {
                    Some(format!(
                        "File discovery limited to {MAX_FILES} files or {MAX_ENTRIES} entries; results may be incomplete"
                    ))
                } else if empty {
                    Some("No project files available for completion".into())
                } else {
                    None
                }
            }
            Ok(Discovery::Files { .. }) => None,
        };
        if publish && self.active.is_some() {
            self.notice = notice.clone();
        }
        self.admit();
        notice
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        self.close();
        if let Some(job) = &mut self.job {
            let result = (&mut job.task)
                .await
                .context("file discovery worker stopped during exit");
            self.job = None;
            // Cancellation is normal; actual traversal failures are not.
            result??;
        }
        Ok(())
    }

    pub fn insert(&mut self, text: &str) -> bool {
        let Some(picker) = &mut self.active else {
            return false;
        };
        picker
            .query
            .extend(text.chars().filter(|ch| !ch.is_control()));
        picker.selected = 0;
        true
    }

    pub fn key(&mut self, key: KeyEvent) -> Option<PickerValue> {
        let loading = self.loading();
        let picker = self.active.as_mut().expect("picker is active");
        match key.code {
            KeyCode::Esc => self.close(),
            KeyCode::Char('c') if key.modifiers.contains(Modifiers::CONTROL) => self.close(),
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => {
                picker.selected =
                    (picker.selected + 1).min(picker.matches().len().saturating_sub(1))
            }
            KeyCode::Backspace => {
                picker.query.pop();
                picker.selected = 0;
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(Modifiers::CONTROL)
                    && !key.modifiers.contains(Modifiers::ALT) =>
            {
                picker.query.push(ch);
                picker.selected = 0;
            }
            KeyCode::Enter if !loading => {
                let index = picker.matches().get(picker.selected).copied();
                let value = index.map(|index| picker.items.remove(index).value);
                self.close();
                return value;
            }
            _ => {}
        }
        None
    }
}

fn discover(cwd: &Path, stop: &CancellationToken) -> Result<Discovery> {
    if stop.is_cancelled() {
        return Ok(Discovery::Cancelled);
    }
    let mut walker = WalkBuilder::new(cwd)
        .follow_links(false)
        .require_git(false)
        .build();
    let mut paths = Vec::new();
    let mut visited = 0;
    let limited = loop {
        // An in-flight filesystem call cannot be killed. Check before and after
        // each traversal step; cap admission even for trees containing no files.
        if stop.is_cancelled() {
            return Ok(Discovery::Cancelled);
        }
        if paths.len() >= MAX_FILES || visited >= MAX_ENTRIES {
            break true;
        }
        let entry = walker.next();
        // Preserve errors even when dismissal raced with this traversal step.
        let entry = match entry {
            Some(entry) => entry.context("traverse project files")?,
            None => break false,
        };
        if let Some(error) = entry.error() {
            return Err(anyhow::anyhow!("project ignore rules: {error}"));
        }
        if stop.is_cancelled() {
            return Ok(Discovery::Cancelled);
        }
        visited += 1;
        if entry.file_type().is_some_and(|kind| kind.is_file()) {
            let relative = entry
                .path()
                .strip_prefix(cwd)
                .context("project file escaped traversal root")?;
            let path = relative
                .to_str()
                .with_context(|| format!("project file path is not valid UTF-8: {relative:?}"))?;
            paths.push(path.to_owned());
        }
    };
    paths.sort();
    Ok(Discovery::Files { paths, limited })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        sync::oneshot,
        time::{Duration, timeout},
    };

    // A held blocking step models filesystem I/O that cannot be interrupted.
    // This is test-owned work, not a production fault or timing hook.
    fn held_job(result: Result<Discovery>) -> (Pickers, oneshot::Sender<()>) {
        let (release, held) = oneshot::channel();
        let mut pickers = Pickers::default();
        pickers.open(Picker {
            title: "Choose file",
            query: "draft query".into(),
            selected: 0,
            items: Vec::new(),
        });
        pickers.job = Some(DiscoveryJob {
            request: FileRequest {
                cwd: PathBuf::new(),
                start: 2,
                end: 3,
            },
            stop: CancellationToken::new(),
            task: tokio::task::spawn_blocking(move || {
                held.blocking_recv().unwrap();
                result
            }),
        });
        (pickers, release)
    }

    fn files(path: &str) -> Result<Discovery> {
        Ok(Discovery::Files {
            paths: vec![path.into()],
            limited: false,
        })
    }

    #[tokio::test]
    async fn dismissal_retains_join_and_coalesces_restarts_without_stale_publication() {
        let root = std::env::temp_dir().join(format!("ion-discovery-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("current.rs"), "current").unwrap();
        let (mut pickers, release) = held_job(files("stale.rs"));
        let original = pickers.job.as_ref().unwrap().task.id();
        // Cancelling the completion future must not remove/detach its handle.
        assert!(
            timeout(Duration::from_millis(10), pickers.complete())
                .await
                .is_err()
        );
        pickers.key(KeyEvent::new(KeyCode::Esc, Modifiers::NONE));
        assert!(pickers.active().is_none());
        assert!(pickers.job.as_ref().unwrap().stop.is_cancelled());
        for query in ["first", "latest"] {
            pickers.open_files(&root, 7, 8, query.into());
            assert_eq!(pickers.job.as_ref().unwrap().task.id(), original);
        }
        release.send(()).unwrap();
        assert!(pickers.complete().await.is_none());
        assert!(pickers.active().unwrap().items.is_empty());
        assert_eq!(pickers.active().unwrap().query, "latest");
        assert!(pickers.complete().await.is_none());
        assert_eq!(pickers.active().unwrap().items[0].label, "current.rs");
        pickers.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn shutdown_waits_for_owned_work_and_reports_errors_even_after_dismissal() {
        for failing in [false, true] {
            let result = if failing {
                Err(anyhow::anyhow!("filesystem failure"))
            } else {
                files("late.rs")
            };
            let (mut pickers, release) = held_job(result);
            pickers.close();
            assert!(
                timeout(Duration::from_millis(10), pickers.shutdown())
                    .await
                    .is_err()
            );
            assert!(pickers.has_job());
            release.send(()).unwrap();
            let result = pickers.shutdown().await;
            assert_eq!(result.is_err(), failing);
            if let Err(error) = result {
                assert!(format!("{error:#}").contains("filesystem failure"));
            }
            assert!(!pickers.has_job());
            assert!(pickers.active().is_none());
        }
    }

    #[tokio::test]
    async fn completion_preserves_query_and_selection_and_surfaces_discovery_failures() {
        let (mut pickers, release) = held_job(files("draft query.rs"));
        pickers.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE));
        assert!(
            pickers.active().is_some(),
            "Enter cannot discard a pending query"
        );
        release.send(()).unwrap();
        assert!(pickers.complete().await.is_none());
        assert_eq!(pickers.active().unwrap().query, "draft query");
        assert_eq!(pickers.active().unwrap().selected, 0);
        let Some(PickerValue::File { path, start, end }) =
            pickers.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE))
        else {
            panic!("no chosen file")
        };
        assert_eq!((path.as_str(), start, end), ("draft query.rs", 2, 3));

        let (mut pickers, release) = held_job(Err(anyhow::anyhow!("read failed")));
        pickers.close();
        release.send(()).unwrap();
        assert!(pickers.complete().await.unwrap().contains("read failed"));
        assert!(pickers.active().is_none());
    }

    #[test]
    fn traversal_honors_cancellation_errors_and_symlink_policy() {
        let root = std::env::temp_dir().join(format!("ion-discovery-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("visible.rs"), "visible").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("visible.rs"), root.join("link.rs")).unwrap();
        let stop = CancellationToken::new();
        let Discovery::Files { paths, limited } = discover(&root, &stop).unwrap() else {
            panic!("cancelled unexpectedly")
        };
        assert_eq!(paths, ["visible.rs"]);
        assert!(!limited);
        stop.cancel();
        assert!(matches!(
            discover(&root, &stop).unwrap(),
            Discovery::Cancelled
        ));
        assert!(discover(&root.join("missing"), &CancellationToken::new()).is_err());
        // Linux permits raw-byte filenames; APFS rejects this fixture at creation.
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStringExt;
            // Lossy completion could select a different valid UTF-8 filename.
            let name = std::ffi::OsString::from_vec(b"invalid-\xff.rs".to_vec());
            std::fs::write(root.join(name), "invalid path").unwrap();
            let error = discover(&root, &CancellationToken::new())
                .err()
                .expect("non-UTF-8 filenames must not be completed lossily");
            assert!(format!("{error:#}").contains("not valid UTF-8"));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
