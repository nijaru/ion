//! Claude Messages thinking, effort and replay capabilities by exact model ID.
use super::{CatalogWire, find};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnthropicProfile {
    Budget,
    Adaptive(AdaptiveProfile),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdaptiveProfile {
    pub(crate) thinking: AdaptiveThinking,
    /// All verified adaptive profiles support max; only newer ones support xhigh.
    pub(crate) xhigh: bool,
    pub(crate) bind_prefix: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdaptiveThinking {
    OptIn,
    DefaultOn,
    AlwaysOn,
}

impl AnthropicProfile {
    pub(super) const OPT_IN_MAX: Self = Self::adaptive(AdaptiveThinking::OptIn, false, false);
    pub(super) const DEFAULT_ON_BOUND: Self =
        Self::adaptive(AdaptiveThinking::DefaultOn, true, true);
    pub(super) const ALWAYS_ON_BOUND: Self = Self::adaptive(AdaptiveThinking::AlwaysOn, true, true);

    const fn adaptive(thinking: AdaptiveThinking, xhigh: bool, bind_prefix: bool) -> Self {
        Self::Adaptive(AdaptiveProfile {
            thinking,
            xhigh,
            bind_prefix,
        })
    }

    pub(crate) fn for_model(id: &str) -> Self {
        if let Some(model) = find("anthropic", id)
            && let CatalogWire::AnthropicMessages(profile) = model.wire
        {
            return profile;
        }
        // Also recognize verified first-party IDs that aren't catalog picks.
        // Other custom Messages routes retain the legacy budget contract.
        match id {
            "claude-opus-4-7" | "claude-opus-4-8" => {
                Self::adaptive(AdaptiveThinking::OptIn, true, false)
            }
            "claude-opus-5" | "claude-sonnet-5" => {
                Self::adaptive(AdaptiveThinking::DefaultOn, true, false)
            }
            "claude-fable-5" => Self::adaptive(AdaptiveThinking::AlwaysOn, true, false),
            _ => Self::Budget,
        }
    }
}
