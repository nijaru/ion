//! Human inspection over Host custody. No client-owned task lifecycle or cache.
use anyhow::{Context, Result};
use ion_host::{BackgroundJobStatus, BackgroundOwner, BackgroundRunSnapshot};

use super::{Frontend, Picker, PickerValue, picker::PickerItem};

mod render;
use render::{activity, end_label, result, state, status};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TaskChoice {
    Open(uuid::Uuid),
    Progress(uuid::Uuid),
    Result(uuid::Uuid),
    Stop(uuid::Uuid),
    Remove(uuid::Uuid),
    List,
}

pub(super) fn apply(ui: &mut Frontend, owner: &BackgroundOwner, choice: TaskChoice) -> Result<()> {
    match choice {
        TaskChoice::List => open(ui, owner),
        TaskChoice::Open(id) => ui.pickers.open(action_picker(owner, id)?),
        TaskChoice::Progress(id) => ui.print(status(&owned(owner, id)?)),
        TaskChoice::Result(id) => ui.print(result(&owned(owner, id)?)?),
        TaskChoice::Stop(id) => {
            owner.request_stop(id)?;
            ui.status =
                "Stop requested; cleanup must still finish. Effects are not rolled back.".into();
            ui.print(ui.status.clone());
            ui.pickers.open(action_picker(owner, id)?);
        }
        TaskChoice::Remove(id) => {
            let report = owner
                .take(id)
                .context("task is still finishing or is no longer retained")?;
            let run = match report {
                Ok(run) => run,
                Err(failure) => {
                    ui.print(result(&failure.run)?);
                    return Err(failure.error.into());
                }
            };
            ui.status = "Removed from task list; saved Session facts remain".into();
            ui.print(format!("{}\n\n{}", result(&run)?, ui.status));
        }
    }
    Ok(())
}

fn owned(owner: &BackgroundOwner, id: uuid::Uuid) -> Result<BackgroundRunSnapshot> {
    owner
        .run(id)
        .context("task is no longer retained by this application")
}

fn item(job: &BackgroundJobStatus) -> PickerItem {
    PickerItem {
        label: format!(
            "Task {} · {}{} · {}/{}",
            &job.id.to_string()[..8],
            state(job.joined, job.progress.as_ref()),
            job.end.as_ref().map_or_else(
                || {
                    if job.error.is_some() {
                        " · failure observed".into()
                    } else if job.joined {
                        " · outcome unknown".into()
                    } else {
                        String::new()
                    }
                },
                |end| format!(" · {}", end_label(end))
            ),
            job.model.provider,
            job.model.model
        ),
        detail: Some({
            let mut text = String::new();
            if let Some(error) = &job.error {
                text.push_str(&format!("Error: {error}\n"));
            }
            text.push_str(
                &job.progress
                    .as_ref()
                    .map_or_else(|| "No live activity observed".into(), activity),
            );
            text
        }),
        value: PickerValue::Task(TaskChoice::Open(job.id)),
    }
}

fn open(ui: &mut Frontend, owner: &BackgroundOwner) {
    let items: Vec<_> = owner.jobs().iter().map(item).collect();
    if items.is_empty() {
        ui.pickers.close();
        ui.status = "No retained background tasks".into();
    } else {
        ui.pickers.open(Picker {
            title: "Background tasks",
            query: String::new(),
            selected: 0,
            items,
        });
    }
}

fn action_picker(owner: &BackgroundOwner, id: uuid::Uuid) -> Result<Picker> {
    let job = owner
        .jobs()
        .into_iter()
        .find(|job| job.id == id)
        .context("task is no longer retained by this application")?;
    Ok(actions(&job))
}

fn actions(job: &BackgroundJobStatus) -> Picker {
    let id = job.id;
    let label = format!(
        "Task {} · {} · {}/{}",
        &id.to_string()[..8],
        state(job.joined, job.progress.as_ref()),
        job.model.provider,
        job.model.model
    );
    let mut items = vec![PickerItem {
        label: "Show progress and details".into(),
        detail: Some(label.clone()),
        value: PickerValue::Task(TaskChoice::Progress(id)),
    }];
    if job.joined {
        items.push(PickerItem {
            label: "Show result".into(),
            detail: Some(label),
            value: PickerValue::Task(TaskChoice::Result(id)),
        });
        items.push(PickerItem { label: "Remove finished task from list".into(),
            detail: Some("Releases the retained result and any returned guidance; shows them once more. Saved Session facts remain, not uncommitted inputs. This does not undo effects.".into()),
            value: PickerValue::Task(TaskChoice::Remove(id)) });
    } else if !job.progress.as_ref().is_some_and(|p| p.stopping) {
        items.push(PickerItem { label: "Stop task".into(),
            detail: Some("Requests cancellation only. Cleanup must still finish; effects are not rolled back.".into()),
            value: PickerValue::Task(TaskChoice::Stop(id)) });
    }
    items.push(PickerItem {
        label: "Back to task list".into(),
        detail: None,
        value: PickerValue::Task(TaskChoice::List),
    });
    Picker {
        title: "Task actions",
        query: String::new(),
        selected: 0,
        items,
    }
}

/// Refresh presentation from Host observations, preserving the selected identity,
/// never an index that could silently retarget a stop/remove action.
pub(super) fn refresh(ui: &mut Frontend, owner: &BackgroundOwner) {
    let Some(picker) = ui.pickers.active() else {
        return;
    };
    let Some(PickerValue::Task(first)) = picker.items.first().map(|item| &item.value) else {
        return;
    };

    let next = match first {
        TaskChoice::Open(_) => {
            let items: Vec<_> = owner.jobs().iter().map(item).collect();
            if items.is_empty() {
                open(ui, owner);
                return;
            }
            Picker {
                title: "Background tasks",
                query: String::new(),
                selected: 0,
                items,
            }
        }
        TaskChoice::Progress(id) => match action_picker(owner, *id) {
            Ok(picker) => picker,
            Err(_) => {
                open(ui, owner);
                ui.status = "Task is no longer retained".into();
                return;
            }
        },
        _ => return,
    };
    let mut next = next;
    refresh_selection(&mut next, picker);
    ui.pickers.open(next);
}

fn refresh_selection(next: &mut Picker, previous: &Picker) {
    let selected = previous
        .matches(false, None)
        .get(previous.selected)
        .and_then(|index| {
            if let PickerValue::Task(choice) = previous.items[*index].value {
                Some(choice)
            } else {
                None
            }
        });
    next.query.clone_from(&previous.query);
    let matching = next.matches(false, None);
    if let Some(index) = matching.iter().position(|index| {
        matches!(next.items[*index].value, PickerValue::Task(choice) if Some(choice) == selected)
    }) {
        next.selected = index;
    } else {
        // A completion can turn a filtered Stop into Remove. A fresh render
        // alone must not reinterpret that intent as releasing an unread result.
        if matching.first().is_some_and(|index| matches!(next.items[*index].value,
            PickerValue::Task(TaskChoice::Stop(_) | TaskChoice::Remove(_)))) {
            next.query.clear(); // Unfiltered first choices only open/read views.
        }
        next.selected = 0;
    }
}

const USAGE: &str = "use /tasks [status|result|cancel|acknowledge UUID]";

pub(super) fn command(ui: &mut Frontend, owner: &BackgroundOwner, args: &str) -> Result<()> {
    if args.is_empty() {
        open(ui, owner);
        return Ok(());
    }
    let (action, id) = args.split_once(char::is_whitespace).context(USAGE)?;
    let child = id.trim().parse::<uuid::Uuid>()?;
    let choice = match action {
        "status" => TaskChoice::Progress(child),
        "result" => TaskChoice::Result(child),
        "cancel" => TaskChoice::Stop(child),
        "acknowledge" => TaskChoice::Remove(child),
        _ => anyhow::bail!(USAGE),
    };
    apply(ui, owner, choice)
}
