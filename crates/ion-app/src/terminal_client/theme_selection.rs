//! Reversible theme choice and light/dark drafts; no Session or input admission.
#[cfg(test)]
mod tests;
use super::{Frontend, Picker, PickerValue, picker::PickerItem};
use anyhow::{Context, Result};

#[derive(Clone, Copy)]
pub(super) enum ThemeAction {
    Automatic,
    Light,
    Dark,
    Apply,
    Single,
}

#[derive(Clone, Copy)]
enum View {
    Single,
    Automatic,
    Light,
    Dark,
}

pub(super) struct ThemePicker {
    back: Option<Picker>,
    view: View,
    single: String,
    light: String,
    dark: String,
}

impl Frontend {
    pub(super) fn open_theme_picker(&mut self) -> Result<()> {
        // Prepare before replacing the existing view; a catalogue error leaves
        // its visible choices and the current palette intact.
        self.themes.names()?;
        let name = self.themes.name();
        let (single, light, dark, view) = match name.split_once('/') {
            Some((light, dark)) => (
                if self.themes.is_dark() {
                    dark.into()
                } else {
                    light.into()
                },
                light.into(),
                dark.into(),
                View::Automatic,
            ),
            None => (name.into(), name.into(), name.into(), View::Single),
        };
        let back = self.pickers.take_active();
        self.theme_picker = Some(ThemePicker {
            back,
            view,
            single,
            light,
            dark,
        });
        if let Err(error) = self.show_theme_picker() {
            self.finish_theme_picker();
            return Err(error);
        }
        Ok(())
    }

    fn show_theme_picker(&mut self) -> Result<()> {
        let state = self
            .theme_picker
            .as_ref()
            .context("theme picker is not open")?;
        let view = state.view;
        let (title, selected, items) = match view {
            View::Automatic => (
                "Automatic theme",
                0,
                vec![
                    PickerItem {
                        label: format!("Light theme: {}", state.light),
                        detail: None,
                        value: PickerValue::ThemeAction(ThemeAction::Light),
                    },
                    PickerItem {
                        label: format!("Dark theme: {}", state.dark),
                        detail: None,
                        value: PickerValue::ThemeAction(ThemeAction::Dark),
                    },
                    PickerItem {
                        label: "Apply · save and go back".into(),
                        detail: Some("Only Apply saves this light/dark pair".into()),
                        value: PickerValue::ThemeAction(ThemeAction::Apply),
                    },
                    PickerItem {
                        label: "Change mode: single theme".into(),
                        detail: None,
                        value: PickerValue::ThemeAction(ThemeAction::Single),
                    },
                ],
            ),
            View::Single | View::Light | View::Dark => {
                let current = match view {
                    View::Single => &state.single,
                    View::Light => &state.light,
                    View::Dark => &state.dark,
                    View::Automatic => unreachable!(),
                };
                let mut names = self.themes.names()?;
                names.retain(|name| !name.contains('/'));
                let selected = names.iter().position(|name| name == current).unwrap_or(0);
                let mut items: Vec<_> = names
                    .into_iter()
                    .map(|name| PickerItem {
                        label: name.clone(),
                        detail: Some(
                            "Preview · Enter selects · Escape goes back · native history unchanged"
                                .into(),
                        ),
                        value: PickerValue::Theme(name),
                    })
                    .collect();
                if matches!(view, View::Single) {
                    items.push(PickerItem {
                        label: "Automatic · choose light and dark themes".into(),
                        detail: Some(
                            "Follow terminal appearance with a pair of named themes".into(),
                        ),
                        value: PickerValue::ThemeAction(ThemeAction::Automatic),
                    });
                }
                (
                    match view {
                        View::Light => "Light theme",
                        View::Dark => "Dark theme",
                        _ => "Theme",
                    },
                    selected,
                    items,
                )
            }
        };
        self.pickers.open(Picker {
            title,
            query: String::new(),
            selected,
            items,
        });
        self.theme_preview_name = None;
        Ok(())
    }

    fn pair(&self) -> String {
        let state = self.theme_picker.as_ref().expect("open theme picker");
        format!("{}/{}", state.light, state.dark)
    }

    pub(super) fn theme_action(&mut self, action: ThemeAction) -> Result<()> {
        let dark = self.themes.is_dark();
        let state = self
            .theme_picker
            .as_mut()
            .context("theme picker is not open")?;
        match action {
            ThemeAction::Automatic => state.view = View::Automatic,
            ThemeAction::Light => state.view = View::Light,
            ThemeAction::Dark => state.view = View::Dark,
            ThemeAction::Single => {
                state.single = if dark {
                    state.dark.clone()
                } else {
                    state.light.clone()
                };
                state.view = View::Single;
            }
            ThemeAction::Apply => {
                let pair = self.pair();
                return self.save_theme(&pair);
            }
        }
        self.show_theme_picker()
    }

    pub(super) fn choose_theme(&mut self, name: &str) -> Result<()> {
        // Validate before changing either the draft pair or saved selection.
        self.themes.preview(name)?;
        let state = self
            .theme_picker
            .as_mut()
            .context("theme picker is not open")?;
        match state.view {
            View::Light => {
                state.light = name.into();
                state.view = View::Automatic;
            }
            View::Dark => {
                state.dark = name.into();
                state.view = View::Automatic;
            }
            View::Single => {
                state.single = name.into();
                return self.save_theme(name);
            }
            View::Automatic => unreachable!("automatic menu selects actions"),
        }
        self.show_theme_picker()
    }

    fn save_theme(&mut self, name: &str) -> Result<()> {
        let warning = match self.themes.confirm(name) {
            Ok(warning) => warning,
            Err(error) => {
                // Confirmation consumed the row, not this unsaved draft.
                self.show_theme_picker()?;
                return Err(error.context("theme was not saved; retry or cancel"));
            }
        };
        self.finish_theme_picker();
        self.status = warning.map_or_else(
            || format!("Theme saved: {name} · native history unchanged"),
            |warning| format!("Theme selected: {name} · {warning}; crash durability uncertain"),
        );
        Ok(())
    }

    pub(super) fn cancel_theme_picker(&mut self) {
        let Some(state) = self.theme_picker.as_mut() else {
            return;
        };
        if matches!(state.view, View::Light | View::Dark) {
            state.view = View::Automatic;
            if let Err(error) = self.show_theme_picker() {
                self.status = format!("{error:#}");
            }
        } else {
            self.themes.cancel_preview();
            self.finish_theme_picker();
        }
    }

    fn finish_theme_picker(&mut self) {
        let state = self.theme_picker.take();
        self.theme_preview_name = None;
        self.pickers.close();
        if let Some(mut picker) = state.and_then(|state| state.back) {
            for item in &mut picker.items {
                if matches!(item.value, PickerValue::Themes) {
                    item.label = format!("Theme: {}", self.themes.name());
                }
            }
            self.pickers.open(picker);
        }
    }

    pub(super) fn sync_theme_preview(&mut self) {
        if !self.pickers.active().is_some_and(Picker::is_theme) {
            self.themes.cancel_preview();
            self.theme_picker = None;
            self.theme_preview_name = None;
            return;
        }
        let candidate = self.pickers.selected_value().and_then(|value| match value {
            PickerValue::Theme(name) => Some(name.clone()),
            PickerValue::ThemeAction(_) => Some(self.pair()),
            _ => None,
        });
        if candidate == self.theme_preview_name {
            return;
        }
        match &candidate {
            Some(name) => {
                if let Err(error) = self.themes.preview(name) {
                    self.status = format!("Theme preview unchanged: {error:#}");
                }
            }
            // No match is not confirmation or dismissal; retain the draft pair.
            None => self.themes.cancel_preview(),
        }
        self.theme_preview_name = candidate;
    }
}
