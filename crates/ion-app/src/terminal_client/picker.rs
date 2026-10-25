//! Modal choices and cooperative discovery shared with non-modal completion.
//! One retained scan joins before its replacement can start.
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

const MAX_PATHS: usize = 20_000;
const MAX_ENTRIES: usize = 100_000;

pub(super) enum PickerValue {
    Session(PathBuf),
    Model(ModelRef),
    Reasoning(ion_ai::Reasoning),
    Login(String),
    Setting(super::configuration::Setting),
    Task(super::tasks::TaskChoice),
    Fork(ForkPickerValue),
    Context(super::navigation::ContextPickerValue),
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
}

/// Startup can choose a workspace before admission; a live binding stays in its
/// captured workspace. Do not offer cross-directory choices it cannot publish.
#[derive(Clone)]
pub(super) enum SessionBrowser {
    Launch,
    Active(String),
}

#[derive(Clone)]
struct SessionRequest {
    catalog: SessionCatalog,
    browser: SessionBrowser,
    all_workspaces: bool,
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
    // A project-path snapshot belongs to the current file-completion episode,
    // not a draft range. Only the editor derives replacement ranges.
    files: Option<(PathBuf, Vec<String>)>,
    sessions: Option<SessionRequest>,
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
        self.files = None;
        self.sessions = None;
        if let Some(job) = &self.job {
            job.stop.cancel();
        }
    }

    pub fn open(&mut self, picker: Picker) {
        self.close();
        self.active = Some(picker);
    }

    pub fn files(&self, cwd: &Path) -> Option<&[String]> {
        self.files
            .as_ref()
            .filter(|(root, _)| root == cwd)
            .map(|(_, paths)| paths.as_slice())
    }

    pub fn request_files(&mut self, cwd: &Path) {
        if self.files(cwd).is_some()
            || matches!(&self.waiting, Some(PickerRequest::Files(request)) if request.cwd == cwd)
            || self.job.as_ref().is_some_and(|job| {
                !job.stop.is_cancelled()
                    && matches!(&job.request, PickerRequest::Files(request) if request.cwd == cwd)
            })
        {
            return;
        }
        self.close();
        self.waiting = Some(PickerRequest::Files(FileRequest {
            cwd: cwd.to_path_buf(),
        }));
        self.admit();
    }

    pub fn cancel_files(&mut self) {
        self.files = None;
        if matches!(&self.waiting, Some(PickerRequest::Files(_))) {
            self.waiting = None;
        }
        if let Some(job) = &self.job
            && matches!(job.request, PickerRequest::Files(_))
        {
            job.stop.cancel();
        }
    }

    pub fn open_sessions(&mut self, catalog: SessionCatalog, browser: SessionBrowser) {
        self.load_sessions(
            SessionRequest {
                catalog,
                browser,
                all_workspaces: false,
            },
            String::new(),
        );
    }

    fn load_sessions(&mut self, request: SessionRequest, query: String) {
        self.open(Picker {
            title: if matches!(request.browser, SessionBrowser::Active(_)) {
                "Resume session"
            } else if request.all_workspaces {
                "Resume session · all folders · Tab: current folder"
            } else {
                "Resume session · current folder · Tab: all folders"
            },
            query,
            selected: 0,
            items: Vec::new(),
        });
        self.sessions = Some(request.clone());
        self.waiting = Some(PickerRequest::Sessions(request));
        self.admit();
    }

    pub fn invalidate_visibility(&mut self) {
        self.rendered_selected = None;
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
                let all_workspaces = sessions.all_workspaces;
                tokio::task::spawn_blocking(move || {
                    let result = if all_workspaces {
                        catalog.scan_all(&worker_stop)
                    } else {
                        catalog.scan(&worker_stop)
                    };
                    result.map(|sessions| {
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
                if publish && let PickerRequest::Files(request) = &job.request {
                    // Remember the failed episode so drawing cannot turn a
                    // filesystem error into an automatic retry loop.
                    self.files = Some((request.cwd.clone(), Vec::new()));
                }
                Some(format!("{}: {error:#}", job.request.failure_label()))
            }
            Ok(PickerOutput::Cancelled) => None,
            Ok(PickerOutput::Files { paths, limited }) if publish => {
                let PickerRequest::Files(request) = job.request else {
                    unreachable!("file output has a file request")
                };
                let empty = paths.is_empty();
                self.files = Some((request.cwd, paths));
                if limited {
                    Some(format!(
                        "File discovery limited to {MAX_PATHS} paths or {MAX_ENTRIES} entries; results may be incomplete"
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
                let items = session_items(
                    sessions,
                    match &request.browser {
                        SessionBrowser::Launch => None,
                        SessionBrowser::Active(id) => Some(id.as_str()),
                    },
                    request.all_workspaces,
                    SystemTime::now(),
                );
                let empty = items.is_empty();
                if let Some(picker) = &mut self.active {
                    picker.items = items;
                    picker.selected = picker
                        .selected
                        .min(picker.matches().len().saturating_sub(1));
                }
                self.rendered_selected = None;
                if empty && matches!(request.browser, SessionBrowser::Active(_)) {
                    self.close();
                    Some("No saved Sessions for this working directory".into())
                } else {
                    empty.then(|| if request.all_workspaces {
                        "No saved coding Sessions · Esc exits".into()
                    } else {
                        "No saved Sessions for this working directory · Tab: all folders · Esc exits".into()
                    })
                }
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
        if key.code == KeyCode::Tab
            && key.modifiers.is_empty()
            && let Some(mut request) = self.sessions.clone()
            && matches!(request.browser, SessionBrowser::Launch)
        {
            request.all_workspaces = !request.all_workspaces;
            let query = self
                .active
                .as_ref()
                .expect("Session browser is active")
                .query
                .clone();
            self.load_sessions(request, query);
            return None;
        }
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
            KeyCode::Enter | KeyCode::Char(' ')
                if !loading
                    && (key.code == KeyCode::Enter
                        || (key.modifiers.is_empty()
                            && picker.query.is_empty()
                            && picker
                                .items
                                .iter()
                                .any(|item| matches!(item.value, PickerValue::Setting(_))))) =>
            {
                let index = picker.matches().get(picker.selected).copied();
                let index = index.filter(|index| self.rendered_selected == Some(*index));
                if let Some(index) = index
                    && let PickerValue::Setting(setting) = picker.items[index].value
                {
                    self.rendered_selected = None;
                    return Some(PickerValue::Setting(setting));
                }
                let value = index.map(|index| picker.items.remove(index).value);
                if value.is_some() {
                    self.close();
                }
                return value;
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(Modifiers::CONTROL)
                    && !key.modifiers.contains(Modifiers::ALT) =>
            {
                picker.query.push(ch);
                picker.selected = 0;
                self.rendered_selected = None;
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
        if paths.len() >= MAX_PATHS || visited >= MAX_ENTRIES {
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
        if entry.depth() > 0
            && entry
                .file_type()
                .is_some_and(|kind| kind.is_file() || kind.is_dir())
        {
            let relative = entry
                .path()
                .strip_prefix(cwd)
                .context("project file escaped traversal root")?;
            let path = relative
                .to_str()
                .with_context(|| format!("project file path is not valid UTF-8: {relative:?}"))?;
            paths.push(if entry.file_type().is_some_and(|kind| kind.is_dir()) {
                format!("{path}/")
            } else {
                path.to_owned()
            });
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
        })
    }

    fn session_request(current_id: &str) -> PickerRequest {
        PickerRequest::Sessions(SessionRequest {
            catalog: SessionCatalog::new(PathBuf::new(), None),
            browser: SessionBrowser::Active(current_id.into()),
            all_workspaces: false,
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
        let (mut pickers, release) = held_job(file_request(), "Choose file", result);
        pickers.active = None;
        (pickers, release)
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
            cwd: Some("/workspace".into()),
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
        pickers.cancel_files();
        assert!(pickers.active().is_none());
        assert!(pickers.job.as_ref().unwrap().stop.is_cancelled());
        for cwd in [root.join("superseded"), root.clone()] {
            pickers.request_files(&cwd);
            assert_eq!(pickers.job.as_ref().unwrap().task.id(), original);
        }
        release.send(()).unwrap();
        assert!(pickers.complete().await.is_none());
        assert!(pickers.active().is_none());
        assert!(pickers.files(&root).is_none());
        assert!(pickers.complete().await.is_none());
        assert_eq!(pickers.files(&root).unwrap(), ["current.rs"]);
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
        for cwd in [root.join("superseded"), root.clone()] {
            pickers.request_files(&cwd);
            assert_eq!(pickers.job.as_ref().unwrap().task.id(), original);
        }
        release.send(()).unwrap();
        // The cancelled Session result cannot publish into file completion.
        assert!(pickers.complete().await.is_none());
        assert!(pickers.active().is_none());
        assert!(pickers.files(&root).is_none());
        assert!(pickers.complete().await.is_none());
        assert_eq!(pickers.files(&root).unwrap(), ["current.rs"]);
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
    async fn session_scope_switch_keeps_query_and_joins_before_replacement() {
        let (mut pickers, release) = held_session_job(sessions("/stale"));
        let PickerRequest::Sessions(request) = &pickers.job.as_ref().unwrap().request else {
            unreachable!()
        };
        let mut request = request.clone();
        request.browser = SessionBrowser::Launch;
        pickers.sessions = Some(request);
        pickers.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE));
        assert_eq!(pickers.active().unwrap().query, "draft query");
        assert!(pickers.sessions.as_ref().unwrap().all_workspaces);
        assert!(pickers.job.as_ref().unwrap().stop.is_cancelled());
        assert!(pickers.waiting.is_some());
        release.send(()).unwrap();
        assert!(pickers.complete().await.is_none());
        assert!(pickers.waiting.is_none());
        assert!(
            pickers.active().unwrap().items.is_empty(),
            "old scope was published"
        );
        pickers.complete().await;
        assert!(
            pickers.active().is_some(),
            "an empty scope must still allow Tab"
        );
        assert_eq!(pickers.active().unwrap().query, "draft query");
        assert!(
            pickers
                .key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE))
                .is_none()
        );
        pickers.shutdown().await.unwrap();
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
    async fn file_discovery_publishes_metadata_and_does_not_retry_failed_episodes() {
        let (mut pickers, release) = held_file_job(files("draft query.rs"));
        release.send(()).unwrap();
        assert!(pickers.complete().await.is_none());
        assert!(pickers.active().is_none());
        assert_eq!(pickers.files(Path::new("")).unwrap(), ["draft query.rs"]);

        let (mut pickers, release) = held_file_job(Err(anyhow::anyhow!("read failed")));
        release.send(()).unwrap();
        assert!(pickers.complete().await.unwrap().contains("read failed"));
        pickers.request_files(Path::new(""));
        assert!(
            !pickers.has_job(),
            "drawing a failed episode must not retry I/O"
        );
        assert_eq!(pickers.files(Path::new("")).unwrap(), Vec::<String>::new());
        pickers.shutdown().await.unwrap();
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
