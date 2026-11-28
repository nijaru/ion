//! The saved model rotation editor. Row indices refer to one frozen local catalogue.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use ion_ai::ModelRef;
use ion_app::{Host, ModelScopeChoices, ModelScopeEntry, SessionControl};

use crate::keybindings::{KeyAction, KeyBindings};

use super::{
    EditorDraft, Frontend, Picker, PickerItem, PickerValue, Pickers, session_io::ReadOutput,
};

pub(super) struct ScopeEditor {
    entries: Vec<ModelScopeEntry>,
    known: usize,
    // None follows the whole catalogue. An explicit empty order enables nothing.
    order: Option<Vec<usize>>,
    saved: Option<Vec<usize>>,
    invocation_override: bool,
}

pub(super) struct ScopeSave {
    entries: Option<Vec<ModelScopeEntry>>,
    order: Option<Vec<usize>>,
}

impl Pickers {
    pub(super) fn open_model_scope(&mut self, choices: ModelScopeChoices) {
        let mut items = Vec::new();
        let mut entries = Vec::new();
        let mut indices = HashMap::new();
        for choice in choices.all {
            let entry = ModelScopeEntry::Identity(choice.selected.identity());
            if indices.contains_key(&entry) {
                continue;
            }
            let status = if choice.is_ready() {
                "ready"
            } else {
                "needs API key"
            };
            let option = choice.selected;
            let label = format!(
                "{}/{}  {}  {status}",
                option.provider, option.model, choice.label
            );
            indices.insert(entry.clone(), entries.len());
            entries.push(entry);
            items.push(row(label));
        }
        let known = items.len();
        let order = choices.saved.map(|saved| {
            saved
                .into_iter()
                .map(|entry| {
                    *indices.entry(entry.clone()).or_insert_with(|| {
                        let label = match &entry {
                            ModelScopeEntry::Identity(id) => {
                                format!("{}/{}  [unavailable]", id.provider, id.model)
                            }
                            ModelScopeEntry::Pattern(pattern) => {
                                format!("{pattern:?}  [unmatched pattern]")
                            }
                        };
                        items.push(row(label));
                        let index = entries.len();
                        entries.push(entry);
                        index
                    })
                })
                .collect::<Vec<_>>()
        });
        self.open(Picker {
            title: "Scoped models",
            query: String::new(),
            selected: 0,
            items,
        });
        self.scope_editor = Some(ScopeEditor {
            entries,
            known,
            saved: order.clone(),
            order,
            invocation_override: choices.invocation_override,
        });
    }

    pub(super) fn finish_model_scope_save(&mut self, submitted: ScopeSave, result: &Result<()>) {
        if let Some(editor) = &mut self.scope_editor {
            let notice = match result {
                Ok(()) => {
                    editor.saved = submitted.order;
                    if editor.invocation_override {
                        "Saved model scope · --models still overrides this invocation".into()
                    } else {
                        "Saved model scope".into()
                    }
                }
                Err(error) => format!("Model scope save failed: {error:#}"),
            };
            self.set_scope_notice(notice);
        }
    }
}

fn row(label: String) -> PickerItem {
    PickerItem {
        label,
        detail: None,
        value: PickerValue::ScopeRow,
    }
}

impl ScopeEditor {
    pub(super) fn matches(&self, picker: &Picker) -> Vec<usize> {
        let query = picker.query.trim().to_ascii_lowercase();
        let rank = |index: usize| match &self.entries[index] {
            ModelScopeEntry::Identity(model) => {
                model_match_rank(&query, model, &picker.items[index].label)
            }
            ModelScopeEntry::Pattern(pattern) => {
                pattern.to_ascii_lowercase().contains(&query).then_some(0)
            }
        };
        let mut visible: Vec<_> = self
            .enabled_order()
            .filter(|&index| rank(index).is_some())
            .collect();
        let enabled: HashSet<_> = self.enabled_order().collect();
        let mut disabled: Vec<_> = (0..self.entries.len())
            .filter(|index| !enabled.contains(index))
            .filter_map(|index| rank(index).map(|rank| (index, rank)))
            .collect();
        if !query.is_empty() {
            disabled.sort_by_key(|&(_, rank)| rank);
        }
        visible.extend(disabled.into_iter().map(|(index, _)| index));
        visible
    }

    fn enabled_order(&self) -> impl Iterator<Item = usize> + '_ {
        let implicit = if self.order.is_none() { self.known } else { 0 };
        (0..implicit).chain(self.order.iter().flatten().copied())
    }

    pub(super) fn enabled(&self, index: usize) -> bool {
        self.order
            .as_ref()
            .map_or(index < self.known, |order| order.contains(&index))
    }

    pub(super) fn title(&self) -> String {
        let enabled = self.enabled_order().count();
        let description = if self.order.is_none() {
            "all models".into()
        } else {
            format!("{enabled} enabled")
        };
        let unsaved = if self.order == self.saved {
            ""
        } else {
            " · unsaved"
        };
        let source = if self.invocation_override {
            " · --models overrides"
        } else {
            ""
        };
        format!("Scoped models · {description}{unsaved}{source}")
    }

    pub(super) fn controls(&self, keys: &KeyBindings, width: usize) -> Vec<String> {
        use KeyAction::*;
        crate::display_text::pack_controls(
            &format!(
                "{} toggle · {} save · {} close · {}/{} reorder · {} provider · {} enable matches · {} clear matches · {} reset to all",
                keys.hint(Confirm),
                keys.hint(ScopeSave),
                keys.hint(Cancel),
                keys.hint(ScopeMoveUp),
                keys.hint(ScopeMoveDown),
                keys.hint(ScopeProvider),
                keys.hint(ScopeEnable),
                keys.hint(ScopeClear),
                keys.hint(ScopeReset),
            ),
            width,
        )
    }

    pub(super) fn save(&self) -> Option<ScopeSave> {
        (self.saved != self.order).then(|| ScopeSave {
            entries: self.order.as_ref().map(|order| {
                order
                    .iter()
                    .map(|&index| self.entries[index].clone())
                    .collect()
            }),
            order: self.order.clone(),
        })
    }

    pub(super) fn edit(&mut self, action: KeyAction, selected: Option<usize>, matching: &[usize]) {
        use KeyAction::*;
        match action {
            ScopeReset => self.order = None,
            ScopeEnable => self.enable(
                matching
                    .iter()
                    .copied()
                    .filter(|&index| index < self.known)
                    .collect(),
            ),
            ScopeClear => self.disable(matching),
            Confirm | Toggle => {
                if let Some(index) = selected {
                    if self.enabled(index) {
                        self.disable(&[index]);
                    } else {
                        self.enable(vec![index]);
                    }
                }
            }
            ScopeProvider => {
                if let Some(ModelScopeEntry::Identity(model)) =
                    selected.map(|index| &self.entries[index])
                {
                    let provider: Vec<_> = self.entries[..self.known]
                        .iter()
                        .enumerate()
                        .filter_map(|(index, entry)| match entry {
                            ModelScopeEntry::Identity(other)
                                if model.provider == other.provider =>
                            {
                                Some(index)
                            }
                            _ => None,
                        })
                        .collect();
                    if provider.iter().all(|&index| self.enabled(index)) {
                        self.disable(&provider);
                    } else {
                        self.enable(provider);
                    }
                }
            }
            ScopeMoveUp | ScopeMoveDown => {
                let Some(index) = selected else { return };
                let mut order: Vec<_> = self.enabled_order().collect();
                let Some(from) = order.iter().position(|&member| member == index) else {
                    return;
                };
                let to = if action == ScopeMoveUp {
                    from.checked_sub(1)
                } else {
                    Some(from + 1)
                };
                if let Some(to) = to.filter(|&to| to < order.len()) {
                    order.swap(from, to);
                    self.order = Some(order);
                }
            }
            _ => unreachable!("scope editor owns only checklist actions"),
        }
    }

    fn enable(&mut self, targets: Vec<usize>) {
        let mut order: Vec<_> = self.enabled_order().collect();
        let enabled: HashSet<_> = order.iter().copied().collect();
        order.extend(targets.into_iter().filter(|index| !enabled.contains(index)));
        if order.len() > enabled.len() {
            self.order = Some(order);
        }
    }

    fn disable(&mut self, targets: &[usize]) {
        let targets: HashSet<_> = targets.iter().copied().collect();
        let mut changed = false;
        let remaining = self
            .enabled_order()
            .filter(|index| {
                let drop = targets.contains(index);
                changed |= drop;
                !drop
            })
            .collect();
        if changed {
            self.order = Some(remaining);
        }
    }
}

pub(super) fn save(
    runtime: &SessionControl<EditorDraft>,
    ui: &mut Frontend,
    services: Arc<Host>,
    submitted: ScopeSave,
) -> Result<()> {
    let io = runtime.session().io()?;
    let entries = submitted.entries.clone();
    ui.session_io
        .start_file_write(runtime.session(), async move {
            let result = match io
                .run(move |_| Ok(services.models().save_scope(entries)))
                .await
            {
                Ok(result) => result,
                Err(error) => Err(error.into()),
            };
            Ok(ReadOutput::ModelScopeSave { submitted, result })
        })?;
    ui.pickers.set_scope_notice("Saving model scope…".into());
    Ok(())
}

// Verbatim hits outrank abbreviations; ties keep catalogue or shortlist order.
pub(super) fn model_match_rank(query: &str, model: &ModelRef, label: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    let id = model.model.to_ascii_lowercase();
    let qualified = format!("{}/{}", model.provider.to_ascii_lowercase(), id);
    let name = id.rsplit('/').next().expect("split always yields a field");
    let fields = [qualified.as_str(), id.as_str(), name];
    if fields.contains(&query) {
        Some(0)
    } else if fields.iter().any(|field| field.starts_with(query)) {
        Some(1)
    } else if label.to_ascii_lowercase().contains(query)
        || fields.iter().any(|field| field.contains(query))
    {
        Some(2)
    } else if query.chars().take(65).count() <= 64 {
        let mut words = query
            .split(|ch: char| ch.is_whitespace() || ch == '/')
            .filter(|word| !word.is_empty())
            .peekable();
        words.peek()?;
        let mut rank = 3;
        for word in words {
            if !qualified.contains(word) {
                let mut letters = word.chars().peekable();
                for ch in qualified.chars() {
                    if letters.peek() == Some(&ch) {
                        letters.next();
                        if letters.peek().is_none() {
                            break;
                        }
                    }
                }
                letters.peek().is_none().then_some(())?;
                rank = 4;
            }
        }
        Some(rank)
    } else {
        None
    }
}
