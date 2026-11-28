//! One pure compiler for control preflight and request encoding.
use super::*;
use crate::catalog::ResponsesProfile;

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
    let mode = anthropic_thinking_mode(&model.model);
    let mut fields = json!({"max_tokens": controls.max_output_tokens});
    let forced_tool = matches!(
        controls.tool_choice,
        ToolChoice::Required | ToolChoice::Named(_)
    );
    let adaptive_thinking = |native: bool| {
        if native && mode == AnthropicThinkingMode::AdaptiveManaged {
            json!({"type":"adaptive","block_binding":{"prefix_mismatch_behavior":"error"}})
        } else {
            json!({"type":"adaptive"})
        }
    };
    match (mode, controls.reasoning) {
        (_, Reasoning::XHigh | Reasoning::Max) => {
            return Err(unsupported(
                "extended reasoning effort is not supported by this adapter",
            ));
        }
        (AnthropicThinkingMode::AdaptiveManaged, Reasoning::ProviderDefault) => {
            if native_anthropic {
                if forced_tool {
                    return Err(unsupported("this Claude model cannot force tool use"));
                }
                fields["thinking"] = adaptive_thinking(true);
            }
        }
        (
            AnthropicThinkingMode::AdaptiveToggleable | AnthropicThinkingMode::Budget,
            Reasoning::ProviderDefault,
        ) => {}
        (AnthropicThinkingMode::AdaptiveManaged, Reasoning::Off) => {
            return Err(unsupported("this Claude model cannot disable thinking"));
        }
        (
            AnthropicThinkingMode::AdaptiveToggleable | AnthropicThinkingMode::Budget,
            Reasoning::Off,
        ) => {
            fields["thinking"] = json!({"type":"disabled"});
        }
        (
            AnthropicThinkingMode::AdaptiveManaged | AnthropicThinkingMode::AdaptiveToggleable,
            Reasoning::Low | Reasoning::Medium | Reasoning::High,
        ) => {
            if forced_tool {
                return Err(unsupported("Anthropic thinking cannot force tool use"));
            }
            let effort = match controls.reasoning {
                Reasoning::Low => "low",
                Reasoning::Medium => "medium",
                Reasoning::High => "high",
                _ => unreachable!("matched explicit effort levels"),
            };
            fields["thinking"] = adaptive_thinking(native_anthropic);
            fields["output_config"] = json!({"effort": effort});
        }
        (
            AnthropicThinkingMode::AdaptiveManaged | AnthropicThinkingMode::AdaptiveToggleable,
            Reasoning::BudgetTokens(_),
        ) => {
            return Err(unsupported(
                "adaptive Claude models use effort levels rather than token budgets",
            ));
        }
        (AnthropicThinkingMode::Budget, Reasoning::Low | Reasoning::Medium | Reasoning::High) => {
            if forced_tool {
                return Err(unsupported("Anthropic thinking cannot force tool use"));
            }
            let target = match controls.reasoning {
                Reasoning::Low => 2_048,
                Reasoning::Medium => 8_192,
                Reasoning::High => 16_384,
                _ => unreachable!("matched explicit effort levels"),
            };
            let max_budget = controls
                .max_output_tokens
                .saturating_sub(ANTHROPIC_MIN_ANSWER_ROOM);
            let budget = target.min(max_budget);
            if budget < ANTHROPIC_MIN_THINKING_BUDGET {
                return Err(unsupported(
                    "max_output_tokens is too small for Anthropic thinking budget",
                ));
            }
            fields["thinking"] = json!({"type":"enabled","budget_tokens": budget});
        }
        (AnthropicThinkingMode::Budget, Reasoning::BudgetTokens(budget)) => {
            if forced_tool {
                return Err(unsupported("Anthropic thinking cannot force tool use"));
            }
            if budget < ANTHROPIC_MIN_THINKING_BUDGET {
                return Err(unsupported(
                    "Anthropic thinking budget must be at least 1024 tokens",
                ));
            }
            if budget >= controls.max_output_tokens {
                return Err(unsupported(
                    "Anthropic thinking budget must be smaller than max_output_tokens",
                ));
            }
            fields["thinking"] = json!({"type":"enabled","budget_tokens": budget});
        }
    }
    Ok(fields)
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
        HttpWire::AnthropicMessages | HttpWire::OpenAiResponses => unreachable!("handled above"),
    }
    Ok(fields)
}
