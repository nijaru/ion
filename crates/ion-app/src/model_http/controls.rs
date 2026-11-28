//! One pure compiler for control preflight and request encoding.
use super::*;
use crate::catalog::{AdaptiveThinking, AnthropicProfile, ResponsesProfile};

const ANTHROPIC_MIN_THINKING_BUDGET: u32 = 1_024;
const ANTHROPIC_MIN_ANSWER_ROOM: u32 = 1_024;

fn anthropic_control_fields(
    model: &ion_ai::ModelRef,
    controls: &ion_ai::GenerationControls,
    native_anthropic: bool,
) -> Result<Value, ProviderError> {
    if controls.temperature.is_some() || controls.top_p.is_some() {
        return Err(unsupported(
            "explicit sampling controls are unsupported by Messages",
        ));
    }
    let profile = AnthropicProfile::for_model(&model.model);
    let mut fields = json!({"max_tokens": controls.max_output_tokens});
    let forced_tool = matches!(
        controls.tool_choice,
        ToolChoice::Required | ToolChoice::Named(_)
    );
    let thinking_conflict = || unsupported("Anthropic thinking cannot force tool use");
    let effort = controls.reasoning;
    if let AnthropicProfile::Adaptive(profile) = profile {
        let binding = native_anthropic && profile.bind_prefix;
        let adaptive = || {
            if binding {
                json!({"type":"adaptive","block_binding":{"prefix_mismatch_behavior":"error"}})
            } else {
                json!({"type":"adaptive"})
            }
        };
        match effort {
            Reasoning::ProviderDefault => {
                if forced_tool && profile.thinking != AdaptiveThinking::OptIn {
                    return Err(thinking_conflict());
                }
                if binding {
                    fields["thinking"] = adaptive();
                }
            }
            Reasoning::Off => {
                if profile.thinking == AdaptiveThinking::AlwaysOn {
                    return Err(unsupported("this Claude model cannot disable thinking"));
                }
                // Default-on toggleable models accept disabled at their native
                // default effort. Do not pair it with an extended effort.
                fields["thinking"] = json!({"type":"disabled"});
            }
            Reasoning::BudgetTokens(_) => {
                return Err(unsupported(
                    "adaptive Claude models use effort levels rather than token budgets",
                ));
            }
            Reasoning::XHigh if !profile.xhigh => {
                return Err(unsupported(
                    "xhigh reasoning is not verified for this Claude model",
                ));
            }
            Reasoning::Low
            | Reasoning::Medium
            | Reasoning::High
            | Reasoning::XHigh
            | Reasoning::Max => {
                if forced_tool {
                    return Err(thinking_conflict());
                }
                fields["thinking"] = adaptive();
                fields["output_config"] = json!({"effort":effort.to_string()});
            }
        }
    } else {
        match effort {
            Reasoning::ProviderDefault => {}
            Reasoning::Off => fields["thinking"] = json!({"type":"disabled"}),
            Reasoning::XHigh | Reasoning::Max => {
                return Err(unsupported(
                    "extended reasoning effort is not verified for this Claude model",
                ));
            }
            Reasoning::Low | Reasoning::Medium | Reasoning::High | Reasoning::BudgetTokens(_) => {
                if forced_tool {
                    return Err(thinking_conflict());
                }
                let budget = anthropic_budget(effort, controls.max_output_tokens)?;
                fields["thinking"] = json!({"type":"enabled","budget_tokens":budget});
            }
        }
    }
    Ok(fields)
}

fn anthropic_budget(effort: Reasoning, max_output: u32) -> Result<u32, ProviderError> {
    if let Reasoning::BudgetTokens(budget) = effort {
        if budget < ANTHROPIC_MIN_THINKING_BUDGET {
            return Err(unsupported(
                "Anthropic thinking budget must be at least 1024 tokens",
            ));
        }
        if budget >= max_output {
            return Err(unsupported(
                "Anthropic thinking budget must be smaller than max_output_tokens",
            ));
        }
        return Ok(budget);
    }
    let target = match effort {
        Reasoning::Low => 2_048,
        Reasoning::Medium => 8_192,
        Reasoning::High => 16_384,
        _ => unreachable!("already selected a legacy thinking budget"),
    };
    let budget = target.min(max_output.saturating_sub(ANTHROPIC_MIN_ANSWER_ROOM));
    if budget < ANTHROPIC_MIN_THINKING_BUDGET {
        return Err(unsupported(
            "max_output_tokens is too small for Anthropic thinking budget",
        ));
    }
    Ok(budget)
}

fn responses_control_fields(
    model: &str,
    controls: &ion_ai::GenerationControls,
) -> Result<Value, ProviderError> {
    if controls.max_output_tokens < 16 {
        return Err(invalid("Responses requires at least 16 output tokens"));
    }
    let profile = ResponsesProfile::for_model(model);
    let mut fields = json!({"max_output_tokens":controls.max_output_tokens});
    match controls.reasoning {
        Reasoning::ProviderDefault => {}
        Reasoning::Off => {
            if profile == Some(ResponsesProfile::Gpt62) {
                return Err(unsupported("this Responses model cannot disable reasoning"));
            }
            fields["reasoning"] = json!({"effort":"none"});
        }
        Reasoning::XHigh if profile.is_none() => {
            return Err(unsupported(
                "xhigh reasoning is not verified for this Responses model",
            ));
        }
        Reasoning::Max
            if !matches!(
                profile,
                Some(ResponsesProfile::Max | ResponsesProfile::Gpt62)
            ) =>
        {
            return Err(unsupported(
                "max reasoning is not verified for this Responses model",
            ));
        }
        Reasoning::Low
        | Reasoning::Medium
        | Reasoning::High
        | Reasoning::XHigh
        | Reasoning::Max => {
            fields["reasoning"] = json!({"effort":controls.reasoning.to_string(),"summary":"auto"});
        }
        Reasoning::BudgetTokens(_) => {
            return Err(unsupported(
                "exact reasoning-token budgets are unsupported by Responses",
            ));
        }
    }
    let sampling_permitted = profile.is_none()
        || controls.reasoning == Reasoning::Off
        || (profile == Some(ResponsesProfile::XhighDefaultOff)
            && controls.reasoning == Reasoning::ProviderDefault);
    if !sampling_permitted && (controls.temperature.is_some() || controls.top_p.is_some()) {
        return Err(unsupported(
            "explicit sampling requires reasoning to be off on this Responses model",
        ));
    }
    if let Some(value) = controls.temperature {
        fields["temperature"] = json!(value);
    }
    if let Some(value) = controls.top_p {
        fields["top_p"] = json!(value);
    }
    Ok(fields)
}

pub(super) fn control_fields(
    model: &ion_ai::ModelRef,
    controls: &ion_ai::GenerationControls,
    wire: HttpWire,
    native_anthropic: bool,
) -> Result<Value, ProviderError> {
    controls.validate()?;
    if wire == HttpWire::AnthropicMessages {
        return anthropic_control_fields(model, controls, native_anthropic);
    }
    if wire == HttpWire::OpenAiResponses {
        return responses_control_fields(&model.model, controls);
    }
    if wire == HttpWire::GeminiGenerateContent {
        return super::gemini_request::gemini_controls(model, controls);
    }
    match controls.reasoning {
        Reasoning::BudgetTokens(_) => {
            return Err(unsupported(
                "exact reasoning-token budgets are unsupported by Chat Completions",
            ));
        }
        Reasoning::XHigh | Reasoning::Max => {
            return Err(unsupported(
                "extended reasoning effort is not supported by this adapter",
            ));
        }
        _ => {}
    }
    let mut fields = if wire == HttpWire::DeepSeekChat {
        json!({"max_tokens":controls.max_output_tokens})
    } else {
        json!({"max_completion_tokens":controls.max_output_tokens})
    };
    if let Some(temperature) = controls.temperature {
        fields["temperature"] = json!(temperature);
    }
    if let Some(top_p) = controls.top_p {
        fields["top_p"] = json!(top_p);
    }
    match wire {
        HttpWire::ChatCompletions => match controls.reasoning {
            Reasoning::ProviderDefault => {}
            Reasoning::Off => fields["reasoning_effort"] = json!("none"),
            Reasoning::Low => fields["reasoning_effort"] = json!("low"),
            Reasoning::Medium => fields["reasoning_effort"] = json!("medium"),
            Reasoning::High => fields["reasoning_effort"] = json!("high"),
            Reasoning::BudgetTokens(_) | Reasoning::XHigh | Reasoning::Max => {
                unreachable!("rejected above")
            }
        },
        HttpWire::LlamaCppNoThinking => {
            if !matches!(
                controls.reasoning,
                Reasoning::ProviderDefault | Reasoning::Off
            ) {
                return Err(unsupported("llama.cpp thinking needs reasoning replay"));
            }
            fields["chat_template_kwargs"] = json!({"enable_thinking":false});
        }
        HttpWire::DeepSeekChat => match controls.reasoning {
            Reasoning::ProviderDefault => {}
            Reasoning::Off => fields["thinking"] = json!({"type":"disabled"}),
            Reasoning::Low => fields["reasoning_effort"] = json!("low"),
            Reasoning::Medium | Reasoning::High => fields["reasoning_effort"] = json!("high"),
            Reasoning::BudgetTokens(_) | Reasoning::XHigh | Reasoning::Max => {
                unreachable!("rejected above")
            }
        },
        HttpWire::MiMoChat => match controls.reasoning {
            Reasoning::ProviderDefault => {}
            Reasoning::Off => fields["thinking"] = json!({"type":"disabled"}),
            Reasoning::Low | Reasoning::Medium | Reasoning::High => {
                fields["thinking"] = json!({"type":"enabled"})
            }
            Reasoning::BudgetTokens(_) | Reasoning::XHigh | Reasoning::Max => {
                unreachable!("rejected above")
            }
        },
        HttpWire::OpenRouterChat => match controls.reasoning {
            Reasoning::ProviderDefault => {}
            Reasoning::Off => fields["reasoning"] = json!({"enabled":false}),
            Reasoning::Low => fields["reasoning"] = json!({"effort":"low"}),
            Reasoning::Medium => fields["reasoning"] = json!({"effort":"medium"}),
            Reasoning::High => fields["reasoning"] = json!({"effort":"high"}),
            Reasoning::BudgetTokens(_) | Reasoning::XHigh | Reasoning::Max => {
                unreachable!("rejected above")
            }
        },
        HttpWire::AnthropicMessages
        | HttpWire::OpenAiResponses
        | HttpWire::GeminiGenerateContent => unreachable!("handled above"),
    }
    Ok(fields)
}
