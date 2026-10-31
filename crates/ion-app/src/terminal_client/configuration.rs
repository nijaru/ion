//! Terminal configuration choices; Host remains the admission and binding owner.
use std::collections::BTreeMap;

use anyhow::Result;
use ion_ai::{ModelRef, ProviderErrorKind, Reasoning};
use ion_core::AgentError;
use ion_host::{CredentialStatus, Host, Selection, SessionControl};
use ion_terminal::{Screen, TerminalSession};

use super::picker::{ModelScope, PickerItem, Pickers};
use super::{EditorDraft, Frontend, Picker, PickerValue, ProgramStatusState, login_in_terminal};
use crate::{tool_output::OutputDetail, transcript_render::ThinkingVisibility};

#[derive(Clone, Copy)]
pub(super) enum Setting {
    Output(OutputDetail),
    Thinking(ThinkingVisibility),
}

impl Pickers {
    pub fn open_models(
        &mut self,
        services: &Host,
        current: Option<&ModelRef>,
        title: &'static str,
    ) -> Result<()> {
        let choices = services
            .models()
            .selection_choices(services.credentials())?;
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
        Ok(())
    }
}

impl Picker {
    pub fn reasoning(runtime: &SessionControl<EditorDraft>) -> Result<Self> {
        let current = runtime.session().reasoning()?;
        runtime.agent().validate_reasoning(current)?;
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
            match runtime.agent().validate_reasoning(reasoning) {
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
                    detail: Some("Terminal-local: fullscreen and future publication; native history unchanged".into()),
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

    pub(super) fn apply_setting(&mut self, setting: Setting) {
        match setting {
            Setting::Output(value) => {
                self.output_detail = value;
                self.status = format!(
                    "Tool output: {} · fullscreen and future publication; native history unchanged",
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
        self.scroll = 0;
        self.fullscreen_rows = 0;
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

// The outer result owns terminal lifecycle. Cancellation, missing access and
// binding preflight errors are recoverable without changing the current model.
pub(super) async fn select_model(
    runtime: &mut SessionControl<EditorDraft>,
    services: &Host,
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    program: &mut ProgramStatusState,
    selection: Selection,
) -> Result<Result<()>> {
    let access = (|| -> Result<_> {
        anyhow::ensure!(
            !runtime.is_busy(),
            "This action is available after the operation"
        );
        let missing = selection.credential_missing(services.credentials())?;
        Ok((selection, missing))
    })();
    let (selection, missing) = match access {
        Ok(access) => access,
        Err(error) => return Ok(Err(error)),
    };
    if missing {
        let result = login_in_terminal(
            terminal,
            screen,
            services.credentials(),
            &selection.provider,
            program,
        )
        .await?;
        if let Err(error) = result {
            return Ok(Err(error));
        }
    }
    Ok(selection
        .require_access(services.credentials())
        .and_then(|()| runtime.select_model(selection.identity())))
}
