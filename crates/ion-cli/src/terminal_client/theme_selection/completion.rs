use super::*;

impl Frontend {
    pub(crate) fn apply_theme_completion(&mut self, completion: Completion) {
        match completion {
            Completion::Catalogue { epoch, result } if self.theme_io.current_epoch(epoch) => {
                match result {
                    Ok(names) => {
                        if let Some(state) = &mut self.theme_picker {
                            state.names = Some(names);
                            let query = self
                                .pickers
                                .active()
                                .map(|picker| picker.query.clone())
                                .unwrap_or_default();
                            self.status = "Choose a theme · Escape goes back".into();
                            self.refresh_theme_picker();
                            if !query.is_empty() {
                                self.pickers.insert(&query);
                            }
                        }
                    }
                    Err(error) => {
                        self.dismiss_theme_picker();
                        self.status = format!("Theme catalogue failed: {error:#}");
                    }
                }
            }
            Completion::Chosen {
                epoch,
                name,
                result,
            } if self.theme_io.current_epoch(epoch) => {
                match result {
                    Ok(selection) => {
                        if let Some(state) = &mut self.theme_picker {
                            self.themes.preview(selection);
                            match state.view {
                                View::Light => state.light = name,
                                View::Dark => state.dark = name,
                                _ => unreachable!("slot choice retains its menu until completion"),
                            }
                            state.view = View::Automatic;
                            self.status = "Theme pair is not saved until Apply".into();
                        }
                    }
                    Err(error) => self.status = format!("Theme choice unchanged: {error:#}"),
                }
                self.refresh_theme_picker();
            }
            Completion::Saved {
                epoch,
                name,
                result,
            } => {
                match result {
                    Ok(saved) => {
                        // A menu dismissal cannot revoke a requested preference
                        // effect. Adopt the native publication even in a new view.
                        let warning = self.themes.confirm(saved);
                        if self.theme_io.current_epoch(epoch) {
                            self.finish_theme_picker();
                        }
                        self.refresh_theme_label();
                        self.status = warning.map_or_else(
                            || format!("Theme saved: {name} · native history unchanged"),
                            |warning| {
                                format!(
                                    "Theme selected: {name} · {warning}; crash durability uncertain"
                                )
                            },
                        );
                    }
                    Err(error) => {
                        if self.theme_io.current_epoch(epoch) {
                            self.refresh_theme_picker();
                        }
                        self.status = format!("Theme was not saved; retry or cancel: {error:#}");
                    }
                }
            }
            Completion::Preview { revision, result }
                if self.theme_io.current_revision(revision) =>
            {
                match result {
                    Ok(selection) => self.themes.preview(selection),
                    Err(error) => self.status = format!("Theme preview unchanged: {error:#}"),
                }
            }
            Completion::Poll {
                revision,
                name,
                result,
            } if self.theme_io.current_revision(revision) => {
                if let Some(notice) = self.themes.checked(&name, result) {
                    self.status = notice;
                }
            }
            // A completed native read may belong to a dismissed/replaced view.
            _ => {}
        }
    }

    pub(in crate::terminal_client) fn install_reloaded_theme(
        &mut self,
        themes: crate::themes::Themes,
    ) {
        self.dismiss_theme_picker();
        self.themes = themes;
        self.refresh_theme_label();
    }

    fn refresh_theme_label(&mut self) {
        // Escape may already have restored the settings menu.
        if self.pickers.active().is_some_and(|picker| {
            picker
                .items
                .iter()
                .any(|item| matches!(item.value, PickerValue::Themes))
        }) {
            let mut picker = self.pickers.take_active().expect("visible settings menu");
            for item in &mut picker.items {
                if matches!(item.value, PickerValue::Themes) {
                    item.label = format!("Theme: {}", self.themes.name());
                }
            }
            self.pickers.open(picker);
        }
    }

    fn refresh_theme_picker(&mut self) {
        if let Err(error) = self.show_theme_picker() {
            self.status = format!("{error:#}");
        }
    }
}
