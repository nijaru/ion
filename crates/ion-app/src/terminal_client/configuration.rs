//! Terminal configuration choices; Host remains the admission and binding owner.
use std::collections::BTreeMap;

use anyhow::Result;
use ion_ai::{ModelRef, ProviderErrorKind, Reasoning};
use ion_core::CodingAgentError;
use ion_host::{CredentialStatus, Host, SessionControl};
use ion_terminal::{Screen, TerminalSession};

use super::picker::PickerItem;
use super::{EditorDraft, Frontend, Picker, PickerValue, ProgramStatusState, login_in_terminal};
use crate::{tool_output::OutputDetail, transcript_render::ThinkingVisibility};

#[derive(Clone, Copy)]
pub(super) enum Setting {
    Output(OutputDetail),
    Thinking(ThinkingVisibility),
}

impl Picker {
    pub fn models(services: &Host, current: Option<&ModelRef>) -> Result<Self> {
        let items: Vec<_> = services
            .models()
            .choices(services.credentials())?
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
        let selected = items
            .iter()
            .position(
                |item| matches!(&item.value, PickerValue::Model(model) if Some(model) == current),
            )
            .unwrap_or(0);
        Ok(Self {
            title: "Choose model",
            query: String::new(),
            selected,
            items,
        })
    }

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
                Err(CodingAgentError::Provider(error))
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
    pub(super) fn open_settings(&mut self) {
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
        self.pickers.open(Picker {
            title: "Settings · Enter/Space changes · Esc closes",
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

    pub(super) fn output_settings(&mut self, args: &str) -> Result<()> {
        let setting = match args {
            "" => {
                self.open_settings();
                return Ok(());
            }
            "compact" => Setting::Output(OutputDetail::Compact),
            "expanded" => Setting::Output(OutputDetail::Expanded),
            "thinking show" => Setting::Thinking(ThinkingVisibility::Visible),
            "thinking hide" => Setting::Thinking(ThinkingVisibility::Hidden),
            _ => anyhow::bail!("use /settings compact|expanded or /settings thinking show|hide"),
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
    model: ModelRef,
) -> Result<Result<()>> {
    let access = (|| -> Result<_> {
        anyhow::ensure!(
            !runtime.is_busy(),
            "This action is available after the operation"
        );
        let selection = services.models().resolve_identity(&model)?;
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
        .and_then(|()| runtime.select_model(model)))
}
