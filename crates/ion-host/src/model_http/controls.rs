//! One pure compiler for control preflight and request encoding.
use super::*;

pub(super) fn control_fields(
    model: &ion_ai::ModelRef,
    controls: &ion_ai::GenerationControls,
    wire: HttpWire,
    native_anthropic: bool,
) -> Result<Value, ProviderError> {
    controls.validate()?;
    if wire == HttpWire::AnthropicMessages {
        if controls.reasoning != Reasoning::ProviderDefault
            || controls.temperature.is_some()
            || controls.top_p.is_some()
        {
            return Err(unsupported(
                "explicit reasoning and sampling controls are unsupported by Messages",
            ));
        }
        let managed = native_anthropic && managed_anthropic_thinking(&model.model);
        if managed
            && matches!(
                controls.tool_choice,
                ToolChoice::Required | ToolChoice::Named(_)
            )
        {
            return Err(unsupported("this Claude model cannot force tool use"));
        }
        let mut fields = json!({"max_tokens":controls.max_output_tokens});
        if managed {
            fields["thinking"] =
                json!({"type":"adaptive","block_binding":{"prefix_mismatch_behavior":"error"}});
        }
        return Ok(fields);
    }
    if matches!(controls.reasoning, Reasoning::BudgetTokens(_)) {
        return Err(unsupported(
            "exact reasoning-token budgets are unsupported by Chat Completions",
        ));
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
            Reasoning::BudgetTokens(_) => unreachable!("rejected above"),
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
            Reasoning::BudgetTokens(_) => unreachable!("rejected above"),
        },
        HttpWire::MiMoChat => match controls.reasoning {
            Reasoning::ProviderDefault => {}
            Reasoning::Off => fields["thinking"] = json!({"type":"disabled"}),
            Reasoning::Low | Reasoning::Medium | Reasoning::High => {
                fields["thinking"] = json!({"type":"enabled"})
            }
            Reasoning::BudgetTokens(_) => unreachable!("rejected above"),
        },
        HttpWire::OpenRouterChat => match controls.reasoning {
            Reasoning::ProviderDefault => {}
            Reasoning::Off => fields["reasoning"] = json!({"enabled":false}),
            Reasoning::Low => fields["reasoning"] = json!({"effort":"low"}),
            Reasoning::Medium => fields["reasoning"] = json!({"effort":"medium"}),
            Reasoning::High => fields["reasoning"] = json!({"effort":"high"}),
            Reasoning::BudgetTokens(_) => unreachable!("rejected above"),
        },
        HttpWire::AnthropicMessages => unreachable!("handled above"),
    }
    Ok(fields)
}
