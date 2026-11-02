//! One retained native theme task, one explicit action, and a coalesced preview.
use super::Frontend;
use crate::themes::{Files, Saved, Selection, Themes};
use anyhow::{Context, Result, ensure};
use tokio::task::JoinHandle;

#[cfg(test)]
mod tests;

pub(super) enum Request {
    Catalogue,
    Choose(String),
    Save(String),
}

enum Work {
    Action { epoch: u64, request: Request },
    Preview { revision: u64, name: String },
    Poll { revision: u64, name: String },
}

pub(super) enum Completion {
    Catalogue {
        epoch: u64,
        result: Result<Vec<String>>,
    },
    Chosen {
        epoch: u64,
        name: String,
        result: Result<Selection>,
    },
    Saved {
        epoch: u64,
        name: String,
        result: Result<Saved>,
    },
    Preview {
        revision: u64,
        result: Result<Selection>,
    },
    Poll {
        revision: u64,
        name: String,
        result: Result<Selection>,
    },
}

impl Work {
    fn run(self, files: Files) -> Completion {
        match self {
            Self::Action {
                epoch,
                request: Request::Catalogue,
            } => Completion::Catalogue {
                epoch,
                result: files.names(),
            },
            Self::Action {
                epoch,
                request: Request::Choose(name),
            } => {
                let result = files.load(&name);
                Completion::Chosen {
                    epoch,
                    name,
                    result,
                }
            }
            Self::Action {
                epoch,
                request: Request::Save(name),
            } => {
                let result = files.save(&name);
                Completion::Saved {
                    epoch,
                    name,
                    result,
                }
            }
            Self::Preview { revision, name } => Completion::Preview {
                revision,
                result: files.load(&name),
            },
            Self::Poll { revision, name } => {
                let result = files.load(&name);
                Completion::Poll {
                    revision,
                    name,
                    result,
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Inspection,
    Action,
    Save,
}

struct Job {
    kind: JobKind,
    task: JoinHandle<Completion>,
}

#[derive(Default)]
pub(super) struct ThemeIo {
    epoch: u64,
    revision: u64,
    job: Option<Job>,
    waiting: Option<(u64, Request)>,
    preview: Option<String>,
    cleanup_error: Option<anyhow::Error>,
}

impl ThemeIo {
    pub(super) fn has_job(&self) -> bool {
        self.job.is_some()
    }
    pub(super) fn has_action(&self) -> bool {
        self.waiting.is_some()
            || self
                .job
                .as_ref()
                .is_some_and(|job| job.kind != JobKind::Inspection)
    }
    pub(super) fn current_epoch(&self, epoch: u64) -> bool {
        self.epoch == epoch
    }
    pub(super) fn current_revision(&self, revision: u64) -> bool {
        self.revision == revision
    }

    pub(super) fn invalidate(&mut self) {
        self.epoch += 1;
        self.revision += 1;
        self.preview = None;
        // A confirmed save remains owned even if its menu is dismissed before
        // dispatch. Dismissal only revokes read results, not a requested effect.
        if !matches!(self.waiting, Some((_, Request::Save(_)))) {
            self.waiting = None;
        }
    }

    pub(super) fn request(&mut self, request: Request, themes: &Themes) -> Result<()> {
        ensure!(
            !self.has_action(),
            "Wait for the requested theme operation to finish"
        );
        self.waiting = Some((self.epoch, request));
        self.start_next(themes);
        Ok(())
    }

    pub(super) fn preview(&mut self, name: Option<String>, themes: &Themes) {
        self.revision += 1;
        self.preview = name;
        self.start_next(themes);
    }

    pub(super) fn poll(&mut self, themes: &Themes) {
        if self.job.is_none()
            && self.waiting.is_none()
            && self.preview.is_none()
            && let Some(name) = themes.watch_name()
        {
            self.start(
                Work::Poll {
                    revision: self.revision,
                    name: name.into(),
                },
                themes,
            );
        }
    }

    pub(super) fn start_next(&mut self, themes: &Themes) {
        if self.job.is_some() {
            return;
        }
        let work = if let Some((epoch, request)) = self.waiting.take() {
            Some(Work::Action { epoch, request })
        } else {
            self.preview.take().map(|name| Work::Preview {
                revision: self.revision,
                name,
            })
        };
        if let Some(work) = work {
            self.start(work, themes);
        }
    }

    fn start(&mut self, work: Work, themes: &Themes) {
        let kind = match work {
            Work::Action {
                request: Request::Save(_),
                ..
            } => JobKind::Save,
            Work::Action { .. } => JobKind::Action,
            _ => JobKind::Inspection,
        };
        let files = themes.files();
        self.job = Some(Job {
            kind,
            task: tokio::task::spawn_blocking(move || work.run(files)),
        });
    }

    pub(super) async fn complete(&mut self) -> Result<Completion> {
        let result = (&mut self.job.as_mut().expect("pending theme task").task).await;
        let kind = self.job.take().expect("joined theme task").kind;
        result.with_context(|| match kind {
            JobKind::Save => "theme save worker failed; preference publication may be unknown",
            _ => "theme read worker failed",
        })
    }
}

impl Frontend {
    #[cfg(test)]
    pub(super) async fn finish_theme_io(&mut self) -> Result<()> {
        let result = self.theme_io.complete().await?;
        self.apply_theme_completion(result);
        self.theme_io.start_next(&self.themes);
        Ok(())
    }

    pub(super) async fn shutdown_theme_io(&mut self) -> Result<()> {
        self.dismiss_theme_picker();
        self.pickers.close();
        self.theme_io.start_next(&self.themes);
        // Also finish any admitted save that was waiting behind a native read.
        while self.theme_io.has_job() {
            let error = match self.theme_io.complete().await {
                Ok(Completion::Saved {
                    result: Err(error), ..
                }) => Some(error.context("theme was not saved before shutdown")),
                Ok(completion) => {
                    self.apply_theme_completion(completion);
                    None
                }
                Err(error) => Some(error),
            };
            if let Some(error) = error {
                self.theme_io.cleanup_error = Some(match self.theme_io.cleanup_error.take() {
                    Some(previous) => {
                        previous.context(format!("additional theme cleanup failure: {error:#}"))
                    }
                    None => error,
                });
            }
            self.theme_io.start_next(&self.themes);
        }
        match self.theme_io.cleanup_error.take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
