//! Modal choices and cooperative discovery shared with non-modal completion.
//! One retained scan joins before its replacement can start.
use super::paint::{Prepared, Visibility};
use crate::presentation_style::{Role, Theme};
use ratatui::text::Line;
use std::path::PathBuf;
use std::time::SystemTime;

use super::file_discovery::{MAX_ENTRIES, MAX_PATHS, discover};
use crate::display_text::{fit_line_with_status, push_wrapped};
#[cfg(test)]
use crate::keybindings::KeyBindings;
use crate::terminal_completion::FileScope;
use anyhow::{Context, Result};
use ion_ai::ModelRef;
use ion_host::{SessionCatalog, SessionSummary};
use ion_terminal::KeyEvent;
#[cfg(test)]
use ion_terminal::{KeyCode, Modifiers};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::navigation::{ForkPickerValue, session_items};
use super::session_search::{SessionQuery, SessionSearch};

pub(super) enum PickerValue {
    Session {
        path: PathBuf,
        search: SessionSearch,
    },
    Model(ModelRef),
    Reasoning(ion_ai::Reasoning),
    Login(String),
    Setting(super::configuration::Setting),
    Themes,
    Theme(String),
    ThemeAction(super::theme_selection::ThemeAction),
    CompletionDelivery(ion_core::CompletionDelivery),
    Task(super::tasks::TaskChoice),
    Mcp(super::mcp::McpChoice),
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
    pub fn is_theme(&self) -> bool {
        self.items.iter().any(|item| {
            matches!(
                item.value,
                PickerValue::Theme(_) | PickerValue::ThemeAction(_)
            )
        })
    }

    pub fn matches(&self, named_only: bool, models: Option<&[ModelRef]>) -> Vec<usize> {
        let query = self.query.to_ascii_lowercase();
        let mut sessions = None;
        let matching: Vec<_> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let matching = if let PickerValue::Session { search, .. } = &item.value {
                    (!named_only || search.named())
                        && sessions
                            .get_or_insert_with(|| SessionQuery::new(&self.query))
                            .matches(search)
                } else {
                    !named_only && item.label.to_ascii_lowercase().contains(&query)
                };
                matching.then_some(index)
            })
            .collect();
        if let Some(models) = models {
            return models.iter().filter_map(|identity| {
                matching.iter().copied().find(|index| {
                    matches!(&self.items[*index].value, PickerValue::Model(model) if model == identity)
                })
            }).collect();
        }
        matching
    }
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
    Files(FileScope),
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

pub(super) enum PickerOutput {
    Cancelled,
    Files { paths: Vec<String>, limited: bool },
    Sessions(Vec<SessionSummary>),
}

pub(super) struct ModelScope {
    pub identities: Vec<ModelRef>,
    pub scoped: bool,
}

#[derive(Default)]
pub(super) struct Pickers {
    active: Option<Picker>,
    notice: Option<String>,
    visibility: Visibility<PickerCandidate>,
    // A dismissed worker still owns its handle. New requests replace this single
    // waiting slot, never spawn another scan before the old one is joined.
    waiting: Option<PickerRequest>,
    job: Option<PickerJob>,
    // A scope-keyed metadata snapshot belongs to the current completion episode,
    // not a draft range. Only the editor derives replacement ranges.
    files: Option<(FileScope, Vec<String>)>,
    sessions: Option<SessionRequest>,
    // Presentation-only state; never captured by discovery jobs or persisted.
    named_only: bool,
    pub(super) models: Option<ModelScope>,
}

impl Pickers {
    fn matching(&self) -> Vec<usize> {
        self.active.as_ref().map_or_else(Vec::new, |picker| {
            picker.matches(
                self.named_only,
                self.models
                    .as_ref()
                    .filter(|models| models.scoped)
                    .map(|models| models.identities.as_slice()),
            )
        })
    }

    pub fn active(&self) -> Option<&Picker> {
        self.active.as_ref()
    }

    pub fn take_active(&mut self) -> Option<Picker> {
        let active = self.active.take();
        self.close();
        active
    }

    pub fn selected_value(&self) -> Option<&PickerValue> {
        let picker = self.active.as_ref()?;
        let index = *self.matching().get(picker.selected)?;
        Some(&picker.items[index].value)
    }

    pub fn controls(&self, keys: &crate::keybindings::KeyBindings) -> Option<String> {
        use crate::keybindings::KeyAction;
        let picker = self.active.as_ref()?;
        let mut controls = format!(
            "{} select · {} closes",
            keys.hint(KeyAction::Confirm),
            keys.hint(KeyAction::Cancel)
        );
        if picker
            .items
            .iter()
            .any(|item| matches!(item.value, PickerValue::Setting(_)))
        {
            controls.push_str(&format!(
                " · {} changes settings",
                keys.hint(KeyAction::Toggle)
            ));
        }
        if self.sessions.is_some() {
            controls.push_str(&format!(
                " · {}: {}",
                keys.hint(KeyAction::NamedSessions),
                if self.named_only {
                    "all Sessions"
                } else {
                    "named only"
                },
            ));
        }
        if let Some(models) = &self.models {
            controls.push_str(&format!(
                " · {}: {}",
                keys.hint(KeyAction::Scope),
                if models.scoped {
                    "all models"
                } else {
                    "scoped models"
                },
            ));
        }
        if let Some(request) = &self.sessions
            && matches!(request.browser, SessionBrowser::Launch)
        {
            controls.push_str(&format!(
                " · {}: {}",
                keys.hint(KeyAction::Scope),
                if request.all_workspaces {
                    "current folder"
                } else {
                    "all folders"
                }
            ));
        }
        Some(controls)
    }

    /// Render against the final budget, after composer and status allocation.
    pub fn rows(&mut self, width: usize, height: usize, theme: &Theme) -> PickerFrame {
        self.visibility.invalidate();
        let Some(picker) = &self.active else {
            return PickerFrame {
                rows: Vec::new(),
                candidate: None,
            };
        };
        let matching = self.matching();
        let title = if let Some(models) = &self.models {
            format!(
                "{} · {}",
                picker.title,
                if models.scoped {
                    "scoped"
                } else {
                    "all models"
                }
            )
        } else if self.named_only {
            format!("{} · named only", picker.title)
        } else {
            picker.title.to_owned()
        };
        let title = if self.loading() {
            format!("{title} · {}", self.loading_label())
        } else {
            format!("{title} · {} match(es)", matching.len())
        };
        let mut rows = vec![Line::styled(
            crate::display_text::fit_line(&title, width),
            theme.style(Role::Heading),
        )];
        if let Some(notice) = &self.notice {
            let mut wrapped = Vec::new();
            push_wrapped(&mut wrapped, notice, width);
            rows.extend(
                wrapped
                    .into_iter()
                    .map(|row| Line::styled(row, theme.style(Role::Secondary))),
            );
        }
        // At small sizes the selected result outranks metadata and notices:
        // Confirmation must not select a row clipped away by another row.
        let selected_row = usize::from(!matching.is_empty());
        rows.truncate(height.saturating_sub(selected_row));
        if picker.is_theme() {
            use ratatui::text::Span;
            let mut sample = Vec::new();
            for roles in [
                &[
                    ("Heading", Role::Heading),
                    ("warning", Role::Warning),
                    ("error", Role::Error),
                ][..],
                &[
                    ("Code", Role::Code),
                    ("keyword", Role::SyntaxKeyword),
                    ("string", Role::SyntaxString),
                ],
                &[
                    ("Added", Role::Added),
                    ("removed", Role::Removed),
                    ("link", Role::Link),
                ],
            ] {
                let mut spans = Vec::new();
                for (index, (label, role)) in roles.iter().enumerate() {
                    if index > 0 {
                        spans.push(Span::raw(" · "));
                    }
                    spans.push(Span::styled(*label, theme.style(*role)));
                }
                crate::display_text::push_styled(&mut sample, "", "", Line::from(spans), width);
            }
            let available = height.saturating_sub(rows.len() + selected_row);
            rows.extend(sample.into_iter().take(available));
        }
        if let Some(detail) = matching
            .get(picker.selected)
            .and_then(|index| picker.items[*index].detail.as_deref())
        {
            let available = height.saturating_sub(rows.len() + selected_row);
            if available > 0 {
                let mut detail_rows = Vec::new();
                push_wrapped(&mut detail_rows, detail, width);
                detail_rows.truncate(available);
                rows.extend(
                    detail_rows
                        .into_iter()
                        .map(|row| Line::styled(row, theme.style(Role::Secondary))),
                );
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
            rows.push(Line::styled(
                row,
                theme.style(if selected { Role::Selected } else { Role::Text }),
            ));
        }
        let candidate = selected_visible.then(|| {
            self.visibility.prepare(PickerCandidate::of(
                &picker.items[matching[picker.selected]],
            ))
        });
        PickerFrame { rows, candidate }
    }

    fn loading_label(&self) -> &'static str {
        match self.waiting.as_ref().or_else(|| {
            self.job
                .as_ref()
                .filter(|job| !job.stop.is_cancelled())
                .map(|job| &job.request)
        }) {
            Some(PickerRequest::Sessions(_)) => "discovering saved Sessions…",
            Some(PickerRequest::Files(_)) | None => "discovering files…",
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
        self.visibility.invalidate();
        self.waiting = None;
        self.files = None;
        self.sessions = None;
        self.named_only = false;
        self.models = None;
        if let Some(job) = &self.job {
            job.stop.cancel();
        }
    }

    pub fn open(&mut self, picker: Picker) {
        self.close();
        self.active = Some(picker);
    }

    pub fn files(&self, scope: &FileScope) -> Option<&[String]> {
        self.files
            .as_ref()
            .filter(|(root, _)| root == scope)
            .map(|(_, paths)| paths.as_slice())
    }

    pub fn request_files(&mut self, scope: &FileScope) {
        if self.files(scope).is_some()
            || matches!(&self.waiting, Some(PickerRequest::Files(request)) if request == scope)
            || self.job.as_ref().is_some_and(|job| {
                !job.stop.is_cancelled()
                    && matches!(&job.request, PickerRequest::Files(request) if request == scope)
            })
        {
            return;
        }
        self.close();
        self.waiting = Some(PickerRequest::Files(scope.clone()));
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
            false,
        );
    }

    fn load_sessions(&mut self, request: SessionRequest, query: String, named_only: bool) {
        self.open(Picker {
            title: if matches!(request.browser, SessionBrowser::Active(_)) {
                "Resume session"
            } else if request.all_workspaces {
                "Resume session · all folders"
            } else {
                "Resume session · current folder"
            },
            query,
            selected: 0,
            items: Vec::new(),
        });
        self.sessions = Some(request.clone());
        self.named_only = named_only;
        self.waiting = Some(PickerRequest::Sessions(request));
        self.admit();
    }

    pub fn invalidate_visibility(&mut self) {
        self.visibility.invalidate();
    }

    pub fn observe_paint(&mut self, candidate: Option<Prepared<PickerCandidate>>) {
        if let Some(candidate) = candidate {
            self.visibility.observe(candidate);
        } else {
            self.visibility.invalidate();
        }
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
                let scope = files.clone();
                tokio::task::spawn_blocking(move || discover(&scope, &worker_stop))
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
                    self.files = Some((request.clone(), Vec::new()));
                }
                Some(format!("{}: {error:#}", job.request.failure_label()))
            }
            Ok(PickerOutput::Cancelled) => None,
            Ok(PickerOutput::Files { paths, limited }) if publish => {
                let PickerRequest::Files(request) = job.request else {
                    unreachable!("file output has a file request")
                };
                let empty = paths.is_empty();
                self.files = Some((request, paths));
                if limited {
                    Some(format!(
                        "File discovery limited to {MAX_PATHS} paths or {MAX_ENTRIES} entries; results may be incomplete"
                    ))
                } else if empty {
                    Some("No files available for completion".into())
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
                    picker.selected = picker.selected.min(
                        picker
                            .matches(self.named_only, None)
                            .len()
                            .saturating_sub(1),
                    );
                }
                self.visibility.invalidate();
                if empty && matches!(request.browser, SessionBrowser::Active(_)) {
                    self.close();
                    Some("No saved Sessions for this working directory".into())
                } else {
                    empty.then(|| {
                        if request.all_workspaces {
                            "No saved coding Sessions".into()
                        } else {
                            "No saved Sessions for this working directory".into()
                        }
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
        self.visibility.invalidate();
        true
    }

    pub fn move_selection(&mut self, down: bool) {
        let matching = self.matching();
        if let Some(picker) = &mut self.active {
            picker.selected = if down {
                (picker.selected + 1).min(matching.len().saturating_sub(1))
            } else {
                picker.selected.saturating_sub(1)
            };
        }
        self.visibility.invalidate();
    }

    pub fn key(
        &mut self,
        key: KeyEvent,
        keys: &crate::keybindings::KeyBindings,
    ) -> Option<PickerValue> {
        use crate::keybindings::{self, KeyAction, PICKER};
        let action = keys.action(PICKER, key);
        if action == Some(KeyAction::Scope) {
            let selected = self.selected_value().and_then(|value| match value {
                PickerValue::Model(model) => Some(model.clone()),
                _ => None,
            });
            if let Some(models) = &mut self.models {
                models.scoped = !models.scoped;
                let matching = self.matching();
                let picker = self.active.as_mut().expect("model picker is active");
                picker.selected = matching
                    .iter()
                    .position(|index| {
                        matches!(&picker.items[*index].value,
                        PickerValue::Model(model) if Some(model) == selected.as_ref())
                    })
                    .unwrap_or(0);
                self.visibility.invalidate();
                return None;
            }
        }
        if action == Some(KeyAction::Scope)
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
            self.load_sessions(request, query, self.named_only);
            return None;
        }
        if action == Some(KeyAction::NamedSessions) && self.sessions.is_some() {
            let picker = self.active.as_mut().expect("Session browser is active");
            let selected = picker
                .matches(self.named_only, None)
                .get(picker.selected)
                .copied();
            self.named_only = !self.named_only;
            let matching = picker.matches(self.named_only, None);
            // Keep the same Session if it survives; filtering must not silently
            // turn a selection into a different conversation.
            picker.selected = selected
                .and_then(|selected| matching.iter().position(|index| *index == selected))
                .unwrap_or(0);
            self.visibility.invalidate();
            return None;
        }
        let loading = self.loading();
        let matching = self.matching();
        let picker = self.active.as_mut().expect("picker is active");
        match action {
            Some(KeyAction::Cancel | KeyAction::Interrupt) => self.close(),
            Some(KeyAction::SelectUp | KeyAction::SelectDown) => {
                self.move_selection(action == Some(KeyAction::SelectDown));
            }
            Some(KeyAction::Backspace) => {
                picker.query.pop();
                picker.selected = 0;
                self.visibility.invalidate();
            }
            Some(KeyAction::Confirm | KeyAction::Toggle)
                if !loading
                    && (action == Some(KeyAction::Confirm)
                        || (picker.query.is_empty()
                            && picker
                                .items
                                .iter()
                                .any(|item| matches!(item.value, PickerValue::Setting(_))))) =>
            {
                let index = matching.get(picker.selected).copied();
                let index = index.filter(|index| {
                    self.visibility.painted().is_some_and(|painted| {
                        *painted == PickerCandidate::of(&picker.items[*index])
                    })
                });
                if let Some(index) = index {
                    let setting = match picker.items[index].value {
                        PickerValue::Setting(setting) => Some(PickerValue::Setting(setting)),
                        PickerValue::Themes => Some(PickerValue::Themes),
                        PickerValue::CompletionDelivery(policy) => {
                            Some(PickerValue::CompletionDelivery(policy))
                        }
                        _ => None,
                    };
                    if setting.is_some() {
                        self.visibility.invalidate();
                        return setting;
                    }
                }
                let value = index.map(|index| picker.items.remove(index).value);
                if value.is_some() {
                    self.close();
                }
                return value;
            }
            None | Some(KeyAction::Toggle) => {
                if let Some(ch) = keybindings::printable(key) {
                    picker.query.push(ch);
                    picker.selected = 0;
                    self.visibility.invalidate();
                }
            }
            _ => {}
        }
        None
    }
}

pub(super) struct PickerFrame {
    pub rows: Vec<Line<'static>>,
    pub candidate: Option<Prepared<PickerCandidate>>,
}

/// The exact retained typed choice, not a filtered display ordinal or provider
/// name. The pointer is compared only, never dereferenced. Its vector stays
/// frozen until replacement/removal, which revokes the visibility episode.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct PickerCandidate(*const PickerItem);

impl PickerCandidate {
    fn of(item: &PickerItem) -> Self {
        Self(std::ptr::from_ref(item))
    }
}

#[cfg(test)]
impl Pickers {
    pub(super) fn painted_rows(
        &mut self,
        width: usize,
        height: usize,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let frame = self.rows(width, height, theme);
        self.observe_paint(frame.candidate);
        frame.rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::UNIX_EPOCH;
    use tokio::{
        sync::oneshot,
        time::{Duration, timeout},
    };

    fn file_request() -> PickerRequest {
        PickerRequest::Files(FileScope::Project(PathBuf::new()))
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
                    value: PickerValue::Login((*label).into()),
                })
                .collect(),
        });
        pickers
    }

    #[test]
    fn scoped_model_matches_deduplicate_routes_and_keep_preference_order() {
        let identity = |model: &str| ModelRef {
            provider: "fixture".into(),
            model: model.into(),
        };
        let mut picker = Picker {
            title: "Choose model",
            query: String::new(),
            selected: 0,
            items: ["first", "second", "first", "outside"]
                .into_iter()
                .map(|model| PickerItem {
                    label: format!("fixture/{model}"),
                    detail: None,
                    value: PickerValue::Model(identity(model)),
                })
                .collect(),
        };
        let scope = [identity("second"), identity("first")];
        assert_eq!(picker.matches(false, Some(&scope)), [1, 0]);
        assert_eq!(picker.matches(false, None), [0, 1, 2, 3]);
        picker.query = "first".into();
        assert_eq!(picker.matches(false, Some(&scope)), [0]);
        picker.query = "outside".into();
        assert!(picker.matches(false, Some(&scope)).is_empty());
        assert_eq!(picker.matches(false, None), [3]);
        picker.query.clear();
        picker.selected = 2;
        let mut pickers = Pickers::default();
        pickers.open(picker);
        pickers.models = Some(ModelScope {
            identities: scope.into(),
            scoped: false,
        });
        let keys = KeyBindings::default();
        pickers.painted_rows(100, 10, &Theme::default());
        pickers.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), &keys);
        assert!(
            matches!(pickers.selected_value(), Some(PickerValue::Model(model)) if model == &identity("first")),
            "deduplication must retain the selected identity, not its disappearing duplicate row index"
        );
        assert!(
            pickers
                .key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), &keys)
                .is_none()
        );
    }

    #[test]
    fn confirmation_requires_the_selected_candidate_in_the_latest_frame() {
        let mut pickers = open_labels(&["alpha", "beta"]);
        assert!(pickers.painted_rows(80, 0, &Theme::default()).is_empty());
        assert!(
            pickers
                .key(
                    KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
                    &KeyBindings::default()
                )
                .is_none()
        );
        assert!(pickers.active().is_some());

        let marker_only = pickers.painted_rows(2, 10, &Theme::default());
        assert_eq!(
            marker_only[1..]
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["›", " "]
        );
        assert!(
            pickers
                .key(
                    KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
                    &KeyBindings::default()
                )
                .is_none()
        );
        assert!(pickers.active().is_some());

        let ellipsis_only = pickers.painted_rows(3, 10, &Theme::default());
        assert_eq!(
            ellipsis_only[1..]
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["› …", "  …"]
        );
        assert!(
            pickers
                .key(
                    KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
                    &KeyBindings::default()
                )
                .is_none()
        );
        assert!(pickers.active().is_some());

        let rows = pickers.painted_rows(80, 10, &Theme::default());
        assert!(rows.iter().any(|row| row.to_string() == "› alpha"));
        assert!(matches!(
            pickers.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), &KeyBindings::default()),
            Some(PickerValue::Login(value)) if value == "alpha"
        ));
        assert!(pickers.active().is_none());

        let mut pickers = open_labels(&["alpha", "beta"]);
        pickers.painted_rows(80, 10, &Theme::default());
        pickers.painted_rows(1, 10, &Theme::default());
        assert!(
            pickers
                .key(
                    KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
                    &KeyBindings::default()
                )
                .is_none()
        );
        pickers.painted_rows(80, 10, &Theme::default());
        pickers.key(
            KeyEvent::new(KeyCode::Char('b'), Modifiers::NONE),
            &KeyBindings::default(),
        );
        assert!(
            pickers
                .key(
                    KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
                    &KeyBindings::default()
                )
                .is_none()
        );
        assert_eq!(pickers.active().unwrap().query, "b");
        let rows = pickers.painted_rows(80, 10, &Theme::default());
        assert!(rows.iter().any(|row| row.to_string() == "› beta"));
        assert!(matches!(
            pickers.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), &KeyBindings::default()),
            Some(PickerValue::Login(value)) if value == "beta"
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
        for scope in [
            FileScope::Project(root.join("superseded")),
            FileScope::Directory(root.clone()),
        ] {
            pickers.request_files(&scope);
            assert_eq!(pickers.job.as_ref().unwrap().task.id(), original);
        }
        release.send(()).unwrap();
        assert!(pickers.complete().await.is_none());
        assert!(pickers.active().is_none());
        assert!(pickers.files(&FileScope::Directory(root.clone())).is_none());
        assert!(pickers.complete().await.is_none());
        assert_eq!(
            pickers.files(&FileScope::Directory(root.clone())).unwrap(),
            ["current.rs"]
        );
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
            pickers.request_files(&FileScope::Project(cwd));
            assert_eq!(pickers.job.as_ref().unwrap().task.id(), original);
        }
        release.send(()).unwrap();
        // The cancelled Session result cannot publish into file completion.
        assert!(pickers.complete().await.is_none());
        assert!(pickers.active().is_none());
        assert!(pickers.files(&FileScope::Project(root.clone())).is_none());
        assert!(pickers.complete().await.is_none());
        assert_eq!(
            pickers.files(&FileScope::Project(root.clone())).unwrap(),
            ["current.rs"]
        );
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

    #[test]
    fn session_search_matches_independent_metadata_fields_not_painted_labels() {
        let summaries = [
            (
                "00000000-0000-4000-8000-123456789abc",
                Some("Beta Fix"),
                "/work/AlphaProject",
                "needle haystack",
            ),
            (
                "11111111-1111-4111-8111-abcdef012345",
                None,
                "/work/Other",
                "B e t a",
            ),
            (
                "22222222-2222-4222-8222-987654321abc",
                Some("École4 API2"),
                "/work/café",
                "tab\tline\nend",
            ),
        ]
        .into_iter()
        .map(|(id, name, cwd, preview)| SessionSummary {
            id: id.into(),
            path: id.into(),
            name: name.map(str::to_owned),
            cwd: Some(cwd.into()),
            preview: Some(preview.into()),
            model: None,
            turns: 1,
            updated: UNIX_EPOCH,
        })
        .collect();
        let mut picker = Picker {
            title: "Resume session",
            query: String::new(),
            selected: 0,
            items: session_items(
                summaries,
                Some("00000000-0000-4000-8000-123456789abc"),
                false,
                UNIX_EPOCH,
            ),
        };
        for (query, expected) in [
            ("alpha beta", vec![0]),
            ("123456789abc", vec![0]),
            ("btfx alphprj", vec![0]),
            ("\"beta fix\" needle", vec![0]),
            ("\"beta needle\"", vec![]),
            ("\"needle    haystack\"", vec![0]),
            ("beta", vec![0]), // Preview characters do not acquire fuzzy matching.
            ("[current]", vec![]),
            ("needle missing", vec![]),
            ("\"beta", vec![]),
            ("ÉCL4 CAFÉ", vec![2]),
            ("api4", vec![2]),
            ("2api", vec![2]),
            ("4ecole", vec![]),
            ("\"tab line\"", vec![2]),
            ("tabline", vec![]),
            ("\"\" \" \"", vec![0, 1, 2]),
            ("", vec![0, 1, 2]),
        ] {
            picker.query = query.into();
            assert_eq!(picker.matches(false, None), expected, "{query}");
        }
        picker.query = "alpha beta".into();
        assert_eq!(picker.matches(true, None), [0]);
        picker.query = "other".into();
        assert_eq!(picker.matches(false, None), [1]);
        assert!(picker.matches(true, None).is_empty());
    }

    #[tokio::test]
    async fn named_sessions_filter_metadata_without_rescanning_or_accepting_stale_rows() {
        let request = session_request("current");
        let summaries = [
            ("/unnamed", None),
            ("/named", Some("Task")),
            ("/blank", Some(" \t")),
        ]
        .into_iter()
        .map(|(path, name)| SessionSummary {
            id: path.into(),
            path: path.into(),
            cwd: Some("/workspace".into()),
            name: name.map(str::to_owned),
            // A preview mentioning names must not make an unnamed Session named.
            preview: Some("saved named prompt".into()),
            model: None,
            turns: 1,
            updated: UNIX_EPOCH,
        })
        .collect();
        let (mut pickers, release) = held_job(
            request.clone(),
            "Resume session",
            Ok(PickerOutput::Sessions(summaries)),
        );
        let PickerRequest::Sessions(request) = request else {
            unreachable!()
        };
        pickers.sessions = Some(request);
        pickers.active.as_mut().unwrap().query = "saved".into();
        let keys = KeyBindings::default();
        let toggle = KeyEvent::new(KeyCode::Char('n'), Modifiers::CONTROL);
        let enter = KeyEvent::new(KeyCode::Enter, Modifiers::NONE);
        let worker = pickers.job.as_ref().unwrap().task.id();
        pickers.key(toggle, &keys);
        assert_eq!(pickers.job.as_ref().unwrap().task.id(), worker);
        release.send(()).unwrap();
        assert!(pickers.complete().await.is_none());
        assert!(
            pickers.painted_rows(100, 10, &Theme::default())[0]
                .to_string()
                .contains("1 match(es)")
        );
        assert_eq!(pickers.active().unwrap().query, "saved");
        assert!(!pickers.has_job());

        pickers.key(toggle, &keys);
        assert!(
            pickers.painted_rows(100, 10, &Theme::default())[0]
                .to_string()
                .contains("3 match(es)")
        );
        assert_eq!(
            pickers.active().unwrap().selected,
            1,
            "selected Session survives"
        );
        pickers.key(KeyEvent::new(KeyCode::Up, Modifiers::NONE), &keys);
        pickers.painted_rows(100, 10, &Theme::default());
        pickers.key(toggle, &keys);
        assert!(
            pickers.key(enter, &keys).is_none(),
            "old rows authorize nothing"
        );
        pickers.painted_rows(100, 10, &Theme::default());
        assert!(
            matches!(pickers.key(enter, &keys), Some(PickerValue::Session { path, .. }) if path == Path::new("/named"))
        );
        assert!(pickers.active().is_none());
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
        pickers.key(
            KeyEvent::new(KeyCode::Char('n'), Modifiers::CONTROL),
            &KeyBindings::default(),
        );
        pickers.key(
            KeyEvent::new(KeyCode::Tab, Modifiers::NONE),
            &KeyBindings::default(),
        );
        assert_eq!(pickers.active().unwrap().query, "draft query");
        assert!(pickers.sessions.as_ref().unwrap().all_workspaces);
        assert!(
            pickers.painted_rows(100, 10, &Theme::default())[0]
                .to_string()
                .contains("named only")
        );
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
            pickers.painted_rows(100, 10, &Theme::default())[0]
                .to_string()
                .contains("named only")
        );
        assert!(
            pickers
                .key(
                    KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
                    &KeyBindings::default()
                )
                .is_none()
        );
        pickers.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn completion_preserves_session_query_and_selection_and_surfaces_failures() {
        let (mut pickers, release) = held_session_job(sessions("/current"));
        pickers.active.as_mut().unwrap().query.clear();
        for character in "saved".chars() {
            pickers.key(
                KeyEvent::new(KeyCode::Char(character), Modifiers::NONE),
                &KeyBindings::default(),
            );
        }
        pickers.key(
            KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
            &KeyBindings::default(),
        );
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
        let rows = pickers.painted_rows(80, 10, &Theme::default());
        assert!(rows.iter().any(|row| row.to_string().starts_with("› ")));
        let Some(PickerValue::Session { path, .. }) = pickers.key(
            KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
            &KeyBindings::default(),
        ) else {
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
        assert_eq!(
            pickers.files(&FileScope::Project(PathBuf::new())).unwrap(),
            ["draft query.rs"]
        );

        let (mut pickers, release) = held_file_job(Err(anyhow::anyhow!("read failed")));
        release.send(()).unwrap();
        assert!(pickers.complete().await.unwrap().contains("read failed"));
        pickers.request_files(&FileScope::Project(PathBuf::new()));
        assert!(
            !pickers.has_job(),
            "drawing a failed episode must not retry I/O"
        );
        assert_eq!(
            pickers.files(&FileScope::Project(PathBuf::new())).unwrap(),
            Vec::<String>::new()
        );
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
        let PickerOutput::Files { paths, limited } =
            discover(&FileScope::Project(root.clone()), &stop).unwrap()
        else {
            panic!("cancelled unexpectedly")
        };
        assert_eq!(paths, ["visible.rs"]);
        assert!(!limited);
        stop.cancel();
        assert!(matches!(
            discover(&FileScope::Project(root.clone()), &stop).unwrap(),
            PickerOutput::Cancelled
        ));
        assert!(
            discover(
                &FileScope::Project(root.join("missing")),
                &CancellationToken::new()
            )
            .is_err()
        );
        // Linux permits raw-byte filenames; APFS rejects this fixture at creation.
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStringExt;
            // Lossy completion could select a different valid UTF-8 filename.
            let name = std::ffi::OsString::from_vec(b"invalid-\xff.rs".to_vec());
            std::fs::write(root.join(name), "invalid path").unwrap();
            let error = discover(&FileScope::Project(root.clone()), &CancellationToken::new())
                .err()
                .expect("non-UTF-8 filenames must not be completed lossily");
            assert!(format!("{error:#}").contains("not valid UTF-8"));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
