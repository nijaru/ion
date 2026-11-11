//! Terminal configuration choices; Host remains the admission and binding owner.
use std::collections::BTreeMap;

use anyhow::Result;
use ion_ai::{ModelRef, ProviderErrorKind, Reasoning};
use ion_app::{CredentialStatus, Host, SessionControl};
use ion_core::AgentError;

use super::picker::{ModelScope, PickerItem, Pickers};
use super::{EditorDraft, Frontend, Picker, PickerValue};
use crate::{tool_output::OutputDetail, transcript_render::ThinkingVisibility};

#[derive(Clone, Copy)]
pub(super) enum Setting {
    Output(OutputDetail),
    Thinking(ThinkingVisibility),
}

impl Pickers {
    #[cfg(test)]
    pub fn open_models(
        &mut self,
        services: &Host,
        current: Option<&ModelRef>,
        title: &'static str,
    ) -> Result<()> {
        let choices = services
            .models()
            .selection_choices(services.credentials())?;
        self.apply_models(choices, current, title);
        Ok(())
    }

    pub(super) fn apply_models(
        &mut self,
        choices: ion_app::ModelChoices,
        current: Option<&ModelRef>,
        title: &'static str,
    ) {
        let items: Vec<_> = choices
            .all
            .into_iter()
            .map(|choice| {
                let option = choice.selected;
                let identity = option.identity();
                PickerItem {
                    label: format!(
                        "{}/{}  {}  {}{}",
                        option.provider,
                        option.model,
                        choice.label,
                        if option.requires_key && choice.credential == CredentialStatus::Missing {
                            "needs API key"
                        } else {
                            "ready"
                        },
                        if current == Some(&identity) {
                            " (current)"
                        } else {
                            ""
                        }
                    ),
                    detail: None,
                    value: PickerValue::Model(identity),
                }
            })
            .collect();
        let selected = items.iter().position(
            |item| matches!(&item.value, PickerValue::Model(model) if Some(model) == current),
        );
        let mut picker = Picker {
            title,
            query: String::new(),
            selected: 0,
            items,
        };
        let matching = picker.matches(false, choices.preferred.as_deref());
        picker.selected = selected
            .and_then(|selected| matching.iter().position(|index| *index == selected))
            .unwrap_or(0);
        self.open(picker);
        self.models = choices.preferred.map(|identities| ModelScope {
            identities,
            scoped: true,
        });
    }
}

impl Picker {
    pub fn reasoning_for(agent: &ion_core::Agent, current: Reasoning) -> Result<Self> {
        agent.validate_reasoning(current)?;
        let mut candidates = vec![
            Reasoning::ProviderDefault,
            Reasoning::Off,
            Reasoning::Low,
            Reasoning::Medium,
            Reasoning::High,
        ];
        if !candidates.contains(&current) {
            candidates.push(current);
        }
        let mut items = Vec::new();
        for reasoning in candidates {
            match agent.validate_reasoning(reasoning) {
                Ok(()) => items.push(PickerItem {
                    label: format!(
                        "{reasoning}{}",
                        if reasoning == current {
                            " (current)"
                        } else {
                            ""
                        }
                    ),
                    detail: Some(if reasoning == Reasoning::ProviderDefault {
                        "Use this route's default generation policy".into()
                    } else {
                        "Accepted by this route's adapter; provider mappings may differ".into()
                    }),
                    value: PickerValue::Reasoning(reasoning),
                }),
                Err(AgentError::Provider(error))
                    if error.kind == ProviderErrorKind::Unsupported => {}
                Err(error) => return Err(error.into()),
            }
        }
        let selected = items
            .iter()
            .position(
                |item| matches!(item.value, PickerValue::Reasoning(value) if value == current),
            )
            .expect("validated current reasoning is a candidate");
        Ok(Self {
            title: "Generation effort",
            query: String::new(),
            selected,
            items,
        })
    }

    pub fn providers(services: &Host) -> Result<Self> {
        // A provider can have routes with different environment overrides. Report
        // observed sources, not a false provider-wide authentication verdict.
        let mut providers = BTreeMap::<String, (bool, bool)>::new();
        for choice in services.models().choices(services.credentials())? {
            let sources = providers.entry(choice.selected.provider).or_default();
            sources.0 |= choice.credential == CredentialStatus::Environment;
            sources.1 |= choice.credential == CredentialStatus::Saved;
        }
        let items = providers.into_iter().map(|(provider, (environment, saved))| PickerItem {
            label: format!("{provider}  {}", match (environment, saved) {
                (true, true) => "environment / saved key in use",
                (true, false) => "environment key in use",
                (false, true) => "saved key in use",
                (false, false) => "no key observed",
            }),
            detail: Some("Save or replace a local API key. Environment keys take precedence; this does not authenticate an account.".into()),
            value: PickerValue::Login(provider),
        }).collect();
        Ok(Self {
            title: "API-key provider",
            query: String::new(),
            selected: 0,
            items,
        })
    }
}

impl Frontend {
    pub(super) fn open_settings(&mut self, completion: ion_core::CompletionDelivery) {
        let (query, selected) = self.pickers.active().map_or_else(
            || (String::new(), 0),
            |picker| (picker.query.clone(), picker.selected),
        );
        let next_output = match self.output_detail {
            OutputDetail::Compact => OutputDetail::Expanded,
            OutputDetail::Expanded => OutputDetail::Compact,
        };
        let next_thinking = match self.thinking_visibility {
            ThinkingVisibility::Visible => ThinkingVisibility::Hidden,
            ThinkingVisibility::Hidden => ThinkingVisibility::Visible,
        };
        let next_completion = match completion {
            ion_core::CompletionDelivery::Inbox => ion_core::CompletionDelivery::Continue,
            ion_core::CompletionDelivery::Continue => ion_core::CompletionDelivery::Wake,
            ion_core::CompletionDelivery::Wake => ion_core::CompletionDelivery::Inbox,
        };
        self.pickers.open(Picker {
            title: "Settings",
            query,
            selected,
            items: vec![
                PickerItem {
                    label: format!("Tool output: {}", self.output_detail.label()),
                    detail: Some("Terminal-local: current view and future publication; native history unchanged".into()),
                    value: PickerValue::Setting(Setting::Output(next_output)),
                },
                PickerItem {
                    label: format!("Thinking: {}", self.thinking_visibility.label()),
                    detail: Some("Presentation only, independent of generation effort. Recorded source remains available.".into()),
                    value: PickerValue::Setting(Setting::Thinking(next_thinking)),
                },
                PickerItem {
                    label: format!("Managed-work completion: {completion}"),
                    detail: Some("Receiving Host policy: inbox waits for natural requests; continue extends active Turns; wake also starts idle Turns. Changes revoke prior grants.".into()),
                    value: PickerValue::CompletionDelivery(next_completion),
                },
                PickerItem {
                    label: format!("Theme: {}", self.themes.name()),
                    detail: Some("Preview a terminal theme; Enter saves the user preference".into()),
                    value: PickerValue::Themes,
                },
            ],
        });
    }

    pub(super) fn set_tui_mode(&mut self, mode: super::TuiMode) {
        self.tool_expansion_origin = None;
        self.mode = mode;
        self.view.follow_end();
    }

    pub(super) fn restore_tool_expansion_viewport(&mut self) {
        if let Some(mode) = self.tool_expansion_origin.take() {
            self.mode = mode;
        }
    }

    pub(super) fn toggle_tool_output(&mut self) {
        let next = match self.output_detail {
            OutputDetail::Compact => {
                if self.mode == super::TuiMode::Inline {
                    // Published native history is immutable. Inspect the same
                    // conversation in an editable viewport rather than repaint it.
                    self.tool_expansion_origin = Some(self.mode);
                    self.mode = super::TuiMode::Fullscreen;
                }
                OutputDetail::Expanded
            }
            OutputDetail::Expanded => OutputDetail::Compact,
        };
        self.apply_setting(Setting::Output(next));
    }

    pub(super) fn apply_setting(&mut self, setting: Setting) {
        match setting {
            Setting::Output(value) => {
                if value == OutputDetail::Compact {
                    self.restore_tool_expansion_viewport();
                }
                self.output_detail = value;
                self.status = format!(
                    "Tool output: {} · current view and future publication; native history unchanged",
                    value.label()
                );
            }
            Setting::Thinking(value) => {
                self.thinking_visibility = value;
                self.status = format!(
                    "Thinking: {} · presentation only; native history unchanged",
                    value.label()
                );
            }
        }
    }

    pub(super) fn output_settings(
        &mut self,
        args: &str,
        completion: ion_core::CompletionDelivery,
    ) -> Result<()> {
        let setting = match args {
            "" => {
                self.open_settings(completion);
                return Ok(());
            }
            "theme" => {
                self.open_theme_picker()?;
                return Ok(());
            }
            "compact" => Setting::Output(OutputDetail::Compact),
            "expanded" => Setting::Output(OutputDetail::Expanded),
            "thinking show" => Setting::Thinking(ThinkingVisibility::Visible),
            "thinking hide" => Setting::Thinking(ThinkingVisibility::Hidden),
            _ => anyhow::bail!(
                "use /settings theme, /settings compact|expanded, /settings thinking show|hide or /settings completion inbox|continue|wake"
            ),
        };
        self.apply_setting(setting);
        Ok(())
    }
}

pub(super) enum ModelRequest {
    Pattern(String),
    Identity(ModelRef),
    Cycle(ion_app::ModelCycleDirection),
}

pub(super) fn select_model(
    runtime: &SessionControl<EditorDraft>,
    ui: &mut Frontend,
    services: std::sync::Arc<Host>,
    request: ModelRequest,
) -> Result<()> {
    anyhow::ensure!(
        !runtime.is_busy(),
        "This action is available after the operation"
    );
    let current = runtime.agent().model().clone();
    let io = runtime.session().io()?;
    ui.session_io
        .start_binding_read(runtime.session(), async move {
            io.run(move |_| {
                Ok((|| -> Result<_> {
                    let selection = match request {
                        ModelRequest::Pattern(pattern) => Some(services.models().resolve_pattern(
                            &pattern,
                            None,
                            services.credentials(),
                        )?),
                        ModelRequest::Identity(identity) => {
                            Some(services.models().resolve_identity(&identity)?)
                        }
                        ModelRequest::Cycle(direction) => direction.candidate(
                            services.models().cycle_choices(services.credentials())?,
                            &current,
                        ),
                    };
                    let Some(selection) = selection else {
                        return Ok(super::session_io::ReadOutput::Notice(
                            "No other locally ready model".into(),
                        ));
                    };
                    let missing = selection.credential_missing(services.credentials())?;
                    Ok(super::session_io::ReadOutput::ModelAccess { selection, missing })
                })())
            })
            .await?
        })
}

pub(super) fn model_access_ready(
    runtime: &SessionControl<EditorDraft>,
    ui: &mut Frontend,
    services: std::sync::Arc<Host>,
    selection: ion_app::Selection,
) -> Result<()> {
    let io = runtime.session().io()?;
    ui.session_io
        .start_binding_read(runtime.session(), async move {
            io.run(move |_| {
                Ok((|| -> Result<_> {
                    selection.require_access(services.credentials())?;
                    Ok(super::session_io::ReadOutput::SelectModel(
                        selection.identity(),
                    ))
                })())
            })
            .await?
        })
}
