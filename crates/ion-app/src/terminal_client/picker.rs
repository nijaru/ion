//! Modal picker custody and its one owned, cooperative background scan.
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::display_text::{fit_line_with_status, push_wrapped};
use anyhow::{Context, Result};
use ignore::WalkBuilder;
use ion_ai::ModelRef;
use ion_host::{SessionCatalog, SessionSummary};
use ion_terminal::{KeyCode, KeyEvent, Modifiers};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::navigation::{ForkPickerValue, session_items};

const MAX_FILES: usize = 20_000;
const MAX_ENTRIES: usize = 100_000;

pub(super) enum PickerValue {
    Session(PathBuf),
    Model(ModelRef),
    Fork(ForkPickerValue),
    Context(super::navigation::ContextPickerValue),
    File {
        path: String,
        start: usize,
        end: usize,
    },
}

pub(super) struct PickerItem {
    pub label: String,
    pub detail: Option<String>,
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

#[derive(Clone)]
struct FileRequest {
    cwd: PathBuf,
    start: usize,
    end: usize,
}

#[derive(Clone)]
struct SessionRequest {
    catalog: SessionCatalog,
    current_id: String,
}

#[derive(Clone)]
enum PickerRequest {
    Files(FileRequest),
    Sessions(SessionRequest),
}

impl PickerRequest {
    fn failure_label(&self) -> &'static str {
        match self {
            Self::Files(_) => "File discovery failed",
            Self::Sessions(_) => "Session discovery failed",
        }
    }
}

struct PickerJob {
    request: PickerRequest,
    stop: CancellationToken,
    task: JoinHandle<Result<PickerOutput>>,
}

enum PickerOutput {
    Cancelled,
    Files { paths: Vec<String>, limited: bool },
    Sessions(Vec<SessionSummary>),
}

#[derive(Default)]
pub(super) struct Pickers {
    active: Option<Picker>,
    notice: Option<String>,
    // The item index rendered as selected in the latest frame. Enter accepts only
    // that candidate, so a stale or clipped selection cannot be chosen blindly.
    rendered_selected: Option<usize>,
    // A dismissed worker still owns its handle. New requests replace this single
    // waiting slot, never spawn another scan before the old one is joined.
    waiting: Option<PickerRequest>,
    job: Option<PickerJob>,
}

impl Pickers {
    pub fn active(&self) -> Option<&Picker> {
        self.active.as_ref()
    }

    /// Render against the final budget, after composer and status allocation.
    pub fn rows(&mut self, width: usize, height: usize) -> Vec<String> {
        self.rendered_selected = None;
        let Some(picker) = &self.active else {
            return Vec::new();
        };
        let matching = picker.matches();
        let mut rows = vec![if self.loading() {
            format!("{} · {}", picker.title, self.loading_label())
        } else {
            format!("{} · {} match(es)", picker.title, matching.len())
        }];
        if let Some(notice) = &self.notice {
            push_wrapped(&mut rows, notice, width);
        }
        // At small sizes the selected result outranks metadata and notices:
        // Enter must not select a row clipped away by another row.
        let selected_row = usize::from(!matching.is_empty());
        rows.truncate(height.saturating_sub(selected_row));
        if let Some(detail) = matching
            .get(picker.selected)
            .and_then(|index| picker.items[*index].detail.as_deref())
        {
            let available = height.saturating_sub(rows.len() + selected_row);
            if available > 0 {
                let mut detail_rows = Vec::new();
                push_wrapped(&mut detail_rows, detail, width);
                detail_rows.truncate(available);
                rows.extend(detail_rows);
            }
        }
        let visible = height.saturating_sub(rows.len());
        let start = picker.selected.saturating_sub(visible.saturating_sub(1));
        let mut selected_visible = false;
        for (index, item) in matching.iter().enumerate().skip(start).take(visible) {
            let selected = index == picker.selected;
            let marker = if selected { '›' } else { ' ' };
            let (row, candidate_visible) = if width > 2 {
                let (label, omitted) = fit_line_with_status(&picker.items[*item].label, width - 2);
                // An omission marker is not candidate text. A literal ellipsis
                // filename, unlike a wholly clipped label, is still selectable.
                let visible = !label.is_empty() && !(omitted && label == "…");
                (format!("{marker} {label}"), visible)
            } else {
                (marker.to_string(), false)
            };
            selected_visible |= selected && candidate_visible;
            rows.push(row);
        }
        if selected_visible {
            self.rendered_selected = matching.get(picker.selected).copied();
        }
        rows
    }

    fn loading_label(&self) -> &'static str {
        match self.waiting.as_ref().or_else(|| {
            self.job
                .as_ref()
                .filter(|job| !job.stop.is_cancelled())
                .map(|job| &job.request)
        }) {
            Some(PickerRequest::Sessions(_)) => "discovering saved Sessions…",
            Some(PickerRequest::Files(_)) | None => "discovering project files…",
        }
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
        self.rendered_selected = None;
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
        self.waiting = Some(PickerRequest::Files(FileRequest {
            cwd: cwd.to_path_buf(),
            start,
            end,
        }));
        self.admit();
    }

    pub fn open_sessions(&mut self, catalog: SessionCatalog, current_id: String) {
        self.open(Picker {
            title: "Resume session",
            query: String::new(),
            selected: 0,
            items: Vec::new(),
        });
        self.waiting = Some(PickerRequest::Sessions(SessionRequest {
            catalog,
            current_id,
        }));
        self.admit();
    }

    fn admit(&mut self) {
        if self.job.is_some() {
            return;
        }
        let Some(request) = self.waiting.take() else {
            return;
        };
        let stop = CancellationToken::new();
        let worker_stop = stop.clone();
        let task = match &request {
            PickerRequest::Files(files) => {
                let cwd = files.cwd.clone();
                tokio::task::spawn_blocking(move || discover(&cwd, &worker_stop))
            }
            PickerRequest::Sessions(sessions) => {
                let catalog = sessions.catalog.clone();
                tokio::task::spawn_blocking(move || {
                    catalog.scan(&worker_stop).map(|sessions| {
                        sessions.map_or(PickerOutput::Cancelled, PickerOutput::Sessions)
                    })
                })
            }
        };
        self.job = Some(PickerJob {
            request,
            stop,
            task,
        });
    }

    // Await by reference: losing a select race leaves custody here, including
    // the completed result. Only remove the handle after its join has resolved.
    pub async fn complete(&mut self) -> Option<String> {
        let result = (&mut self.job.as_mut()?.task).await;
        let job = self.job.take().expect("joined picker work is still owned");
        let publish = !job.stop.is_cancelled();
        let notice = match result
            .context("picker worker stopped")
            .and_then(|result| result)
        {
            Err(error) => {
                if publish {
                    self.close();
                }
                Some(format!("{}: {error:#}", job.request.failure_label()))
            }
            Ok(PickerOutput::Cancelled) => None,
            Ok(PickerOutput::Files { paths, limited }) if publish => {
                let PickerRequest::Files(request) = job.request else {
                    unreachable!("file output has a file request")
                };
                let empty = paths.is_empty();
                if let Some(picker) = &mut self.active {
                    picker.items = paths
                        .into_iter()
                        .map(|path| PickerItem {
                            label: path.clone(),
                            detail: None,
                            value: PickerValue::File {
                                path,
                                start: request.start,
                                end: request.end,
                            },
                        })
                        .collect();
                    picker.selected = picker
                        .selected
                        .min(picker.matches().len().saturating_sub(1));
                }
                self.rendered_selected = None;
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
            Ok(PickerOutput::Sessions(sessions)) if publish => {
                let PickerRequest::Sessions(request) = job.request else {
                    unreachable!("Session output has a Session request")
                };
                let items = session_items(sessions, &request.current_id, SystemTime::now());
                let empty = items.is_empty();
                if let Some(picker) = &mut self.active {
                    picker.items = items;
                    picker.selected = picker
                        .selected
                        .min(picker.matches().len().saturating_sub(1));
                }
                self.rendered_selected = None;
                if empty {
                    self.close();
                }
                empty.then(|| "No saved Sessions for this working directory".into())
            }
            Ok(PickerOutput::Files { .. } | PickerOutput::Sessions(_)) => None,
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
                .context("picker worker stopped during exit");
            self.job = None;
            // Cancellation is normal; actual scan failures are not.
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
        self.rendered_selected = None;
        true
    }

    pub fn key(&mut self, key: KeyEvent) -> Option<PickerValue> {
        let loading = self.loading();
        let picker = self.active.as_mut().expect("picker is active");
        match key.code {
            KeyCode::Esc => self.close(),
            KeyCode::Char('c') if key.modifiers.contains(Modifiers::CONTROL) => self.close(),
            KeyCode::Up => {
                picker.selected = picker.selected.saturating_sub(1);
                self.rendered_selected = None;
            }
            KeyCode::Down => {
                picker.selected =
                    (picker.selected + 1).min(picker.matches().len().saturating_sub(1));
                self.rendered_selected = None;
            }
            KeyCode::Backspace => {
                picker.query.pop();
                picker.selected = 0;
                self.rendered_selected = None;
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(Modifiers::CONTROL)
                    && !key.modifiers.contains(Modifiers::ALT) =>
            {
                picker.query.push(ch);
                picker.selected = 0;
                self.rendered_selected = None;
            }
            KeyCode::Enter if !loading => {
                let index = picker.matches().get(picker.selected).copied();
                let value = index
                    .filter(|index| self.rendered_selected == Some(*index))
                    .map(|index| picker.items.remove(index).value);
                if value.is_some() {
                    self.close();
                }
                return value;
            }
            _ => {}
        }
        None
    }
}

fn discover(cwd: &Path, stop: &CancellationToken) -> Result<PickerOutput> {
    if stop.is_cancelled() {
        return Ok(PickerOutput::Cancelled);
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
            return Ok(PickerOutput::Cancelled);
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
            return Ok(PickerOutput::Cancelled);
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
    Ok(PickerOutput::Files { paths, limited })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;
    use tokio::{
        sync::oneshot,
        time::{Duration, timeout},
    };

    fn file_request() -> PickerRequest {
        PickerRequest::Files(FileRequest {
            cwd: PathBuf::new(),
            start: 2,
            end: 3,
        })
    }

    fn session_request(current_id: &str) -> PickerRequest {
        PickerRequest::Sessions(SessionRequest {
            catalog: SessionCatalog::new(PathBuf::new(), None),
            current_id: current_id.into(),
        })
    }

    // A held blocking step models filesystem I/O that cannot be interrupted.
    // This is test-owned work, not a production fault or timing hook.
    fn held_job(
        request: PickerRequest,
        title: &'static str,
        result: Result<PickerOutput>,
    ) -> (Pickers, oneshot::Sender<()>) {
        let (release, held) = oneshot::channel();
        let mut pickers = Pickers::default();
        pickers.open(Picker {
            title,
            query: "draft query".into(),
            selected: 0,
            items: Vec::new(),
        });
        pickers.job = Some(PickerJob {
            request,
            stop: CancellationToken::new(),
            task: tokio::task::spawn_blocking(move || {
                held.blocking_recv().unwrap();
                result
            }),
        });
        (pickers, release)
    }

    fn held_file_job(result: Result<PickerOutput>) -> (Pickers, oneshot::Sender<()>) {
        held_job(file_request(), "Choose file", result)
    }

    fn held_session_job(result: Result<PickerOutput>) -> (Pickers, oneshot::Sender<()>) {
        held_job(
            session_request("019abcdef0123456789abcdef01234567"),
            "Resume session",
            result,
        )
    }

    fn files(path: &str) -> Result<PickerOutput> {
        Ok(PickerOutput::Files {
            paths: vec![path.into()],
            limited: false,
        })
    }

    fn sessions(path: &str) -> Result<PickerOutput> {
        Ok(PickerOutput::Sessions(vec![SessionSummary {
            id: "019abcdef0123456789abcdef01234567".into(),
            path: path.into(),
            name: Some("saved task".into()),
            preview: Some("saved prompt".into()),
            model: None,
            turns: 2,
            updated: UNIX_EPOCH,
        }]))
    }

    fn open_labels(labels: &[&str]) -> Pickers {
        let mut pickers = Pickers::default();
        pickers.open(Picker {
            title: "Resume session",
            query: String::new(),
            selected: 0,
            items: labels
                .iter()
                .map(|label| PickerItem {
                    label: (*label).into(),
                    detail: None,
                    value: PickerValue::Session(PathBuf::from(label)),
                })
                .collect(),
        });
        pickers
    }

    #[test]
    fn confirmation_requires_the_selected_candidate_in_the_latest_frame() {
        let mut pickers = open_labels(&["alpha", "beta"]);
        assert!(pickers.rows(80, 0).is_empty());
        assert!(
            pickers
                .key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE))
                .is_none()
        );
        assert!(pickers.active().is_some());

        let marker_only = pickers.rows(2, 10);
        assert_eq!(&marker_only[1..], ["›", " "]);
        assert!(
            pickers
                .key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE))
                .is_none()
        );
        assert!(pickers.active().is_some());

        let ellipsis_only = pickers.rows(3, 10);
        assert_eq!(&ellipsis_only[1..], ["› …", "  …"]);
        assert!(
            pickers
                .key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE))
                .is_none()
        );
        assert!(pickers.active().is_some());

        let rows = pickers.rows(80, 10);
        assert!(rows.iter().any(|row| row == "› alpha"));
        assert!(matches!(
            pickers.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE)),
            Some(PickerValue::Session(path)) if path == Path::new("alpha")
        ));
        assert!(pickers.active().is_none());

        let mut pickers = open_labels(&["alpha", "beta"]);
        pickers.rows(80, 10);
        pickers.rows(1, 10);
        assert!(
            pickers
                .key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE))
                .is_none()
        );
        pickers.rows(80, 10);
        pickers.key(KeyEvent::new(KeyCode::Char('b'), Modifiers::NONE));
        assert!(
            pickers
                .key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE))
                .is_none()
        );
        assert_eq!(pickers.active().unwrap().query, "b");
        let rows = pickers.rows(80, 10);
        assert!(rows.iter().any(|row| row == "› beta"));
        assert!(matches!(
            pickers.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE)),
            Some(PickerValue::Session(path)) if path == Path::new("beta")
        ));
    }

    #[tokio::test]
    async fn dismissal_retains_join_and_coalesces_restarts_without_stale_publication() {
        let root = std::env::temp_dir().join(format!("ion-discovery-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("current.rs"), "current").unwrap();
        let (mut pickers, release) = held_file_job(files("stale.rs"));
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
    async fn session_results_cannot_replace_a_new_model_picker() {
        let (mut pickers, release) = held_session_job(sessions("/saved"));
        pickers.open(Picker {
            title: "Choose model",
            query: "model query".into(),
            selected: 0,
            items: Vec::new(),
        });
        assert!(pickers.job.as_ref().unwrap().stop.is_cancelled());
        release.send(()).unwrap();
        assert!(pickers.complete().await.is_none());
        assert_eq!(pickers.active().unwrap().title, "Choose model");
        assert_eq!(pickers.active().unwrap().query, "model query");
        assert!(pickers.active().unwrap().items.is_empty());
        assert!(!pickers.has_job());
        pickers.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn session_worker_joins_before_files_can_replace_it() {
        let root = std::env::temp_dir().join(format!("ion-discovery-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("current.rs"), "current").unwrap();
        let (mut pickers, release) = held_session_job(sessions("stale.sqlite"));
        let original = pickers.job.as_ref().unwrap().task.id();
        for query in ["first", "latest"] {
            pickers.open_files(&root, 7, 8, query.into());
            assert_eq!(pickers.job.as_ref().unwrap().task.id(), original);
        }
        release.send(()).unwrap();
        // The cancelled Session result cannot publish into the newer file picker.
        assert!(pickers.complete().await.is_none());
        assert_eq!(pickers.active().unwrap().title, "Choose file");
        assert!(pickers.active().unwrap().items.is_empty());
        assert!(pickers.complete().await.is_none());
        assert_eq!(pickers.active().unwrap().query, "latest");
        assert_eq!(pickers.active().unwrap().items[0].label, "current.rs");
        pickers.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn shutdown_waits_for_owned_work_and_reports_errors_even_after_dismissal() {
        for (request, title) in [
            (file_request(), "Choose file"),
            (
                session_request("019abcdef0123456789abcdef01234567"),
                "Resume session",
            ),
        ] {
            for failing in [false, true] {
                let result = if failing {
                    Err(anyhow::anyhow!("filesystem failure"))
                } else {
                    match request {
                        PickerRequest::Files(_) => files("late.rs"),
                        PickerRequest::Sessions(_) => sessions("/late.sqlite"),
                    }
                };
                let (mut pickers, release) = held_job(request.clone(), title, result);
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
    }

    #[tokio::test]
    async fn completion_preserves_session_query_and_selection_and_surfaces_failures() {
        let (mut pickers, release) = held_session_job(sessions("/current"));
        pickers.active.as_mut().unwrap().query.clear();
        for character in "saved".chars() {
            pickers.key(KeyEvent::new(KeyCode::Char(character), Modifiers::NONE));
        }
        pickers.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE));
        assert!(
            pickers.active().is_some(),
            "Enter cannot discard a pending query"
        );
        release.send(()).unwrap();
        assert!(pickers.complete().await.is_none());
        let picker = pickers.active().unwrap();
        assert_eq!(picker.query, "saved");
        assert_eq!(picker.selected, 0);
        assert!(picker.items[0].label.starts_with("[current]"));
        let detail = picker.items[0].detail.as_deref().unwrap();
        assert!(detail.contains("019abcdef0123456789abcdef01234567"));
        assert!(detail.contains("2 turn(s)"));
        let rows = pickers.rows(80, 10);
        assert!(rows.iter().any(|row| row.starts_with("› ")));
        let Some(PickerValue::Session(path)) =
            pickers.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE))
        else {
            panic!("no chosen Session")
        };
        assert_eq!(path, PathBuf::from("/current"));

        let (mut pickers, release) = held_session_job(Err(anyhow::anyhow!("read failed")));
        pickers.close();
        release.send(()).unwrap();
        let notice = pickers.complete().await.unwrap();
        assert!(notice.contains("Session discovery failed") && notice.contains("read failed"));
        assert!(pickers.active().is_none());
    }

    #[tokio::test]
    async fn completion_preserves_query_and_selection_and_surfaces_discovery_failures() {
        let (mut pickers, release) = held_file_job(files("draft query.rs"));
        pickers.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE));
        assert!(
            pickers.active().is_some(),
            "Enter cannot discard a pending query"
        );
        release.send(()).unwrap();
        assert!(pickers.complete().await.is_none());
        assert_eq!(pickers.active().unwrap().query, "draft query");
        assert_eq!(pickers.active().unwrap().selected, 0);
        pickers.rows(80, 10);
        let Some(PickerValue::File { path, start, end }) =
            pickers.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE))
        else {
            panic!("no chosen file")
        };
        assert_eq!((path.as_str(), start, end), ("draft query.rs", 2, 3));

        let (mut pickers, release) = held_file_job(Err(anyhow::anyhow!("read failed")));
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
        let PickerOutput::Files { paths, limited } = discover(&root, &stop).unwrap() else {
            panic!("cancelled unexpectedly")
        };
        assert_eq!(paths, ["visible.rs"]);
        assert!(!limited);
        stop.cancel();
        assert!(matches!(
            discover(&root, &stop).unwrap(),
            PickerOutput::Cancelled
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
