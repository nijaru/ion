//! Optional provider prefix refresh while owned tool execution continues.
//! Optimization failures never override committed effect outcomes.
use std::sync::Arc;

use ion_ai::{ModelRequest, ModelService};
use tokio_util::sync::CancellationToken;

use super::AgentError;
use crate::{generation::refresh_prompt_cache, session::Session, tool_set::ToolExecution};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PromptCacheWarmingPolicy {
    /// Cache lifetime. Warming is disabled when the ten-second safety margin
    /// leaves no positive refresh point within the one-hour active horizon.
    pub lifetime_seconds: u64,
    /// Micro-US-dollars per million tokens.
    pub cache_write_microusd_per_million: u64,
    pub cache_read_microusd_per_million: u64,
    pub output_microusd_per_million: u64,
    pub minimum_savings_microusd: u64,
}

impl PromptCacheWarmingPolicy {
    fn cached_prefix_tokens(self, usage: ion_ai::Usage) -> Option<u64> {
        if usage.cache_read_input_tokens.is_none() && usage.cache_write_input_tokens.is_none() {
            return None;
        }
        let tokens = usage
            .cache_read_input_tokens
            .unwrap_or(0)
            .checked_add(usage.cache_write_input_tokens.unwrap_or(0))?;
        (tokens > 0).then_some(tokens)
    }

    fn token_cost_microusd(tokens: u64, rate_microusd_per_million: u64) -> u128 {
        u128::from(tokens).saturating_mul(u128::from(rate_microusd_per_million)) / 1_000_000
    }

    fn net_refresh_savings_microusd(self, prefix_tokens: u64) -> Option<u64> {
        let miss = Self::token_cost_microusd(prefix_tokens, self.cache_write_microusd_per_million);
        let hit = Self::token_cost_microusd(prefix_tokens, self.cache_read_microusd_per_million);
        let refresh = hit.saturating_add(Self::token_cost_microusd(
            1,
            self.output_microusd_per_million,
        ));
        let net = miss.checked_sub(hit)?.checked_sub(refresh)?;
        u64::try_from(net).ok()
    }

    fn should_warm(self, usage: ion_ai::Usage) -> bool {
        self.cached_prefix_tokens(usage)
            .and_then(|tokens| self.net_refresh_savings_microusd(tokens))
            .is_some_and(|net| net >= self.minimum_savings_microusd)
    }

    /// No immediate refresh: a lifetime within the safety margin cannot be
    /// extended economically and must not create a zero-delay request loop.
    fn refresh_after(self) -> Option<std::time::Duration> {
        let ninety_percent = self.lifetime_seconds.saturating_mul(9) / 10;
        let ten_second_margin = self.lifetime_seconds.checked_sub(10)?;
        (ten_second_margin > 0)
            .then(|| std::time::Duration::from_secs(ninety_percent.min(ten_second_margin)))
    }
}

const ACTIVE_CACHE_WARMING_LIMIT: std::time::Duration = std::time::Duration::from_secs(60 * 60);

pub(super) struct PromptCacheWarmer {
    request: ModelRequest,
    policy: PromptCacheWarmingPolicy,
    refresh_after: std::time::Duration,
    next_refresh: tokio::time::Instant,
    expires_at: tokio::time::Instant,
    stop_at: tokio::time::Instant,
}

impl PromptCacheWarmer {
    pub(super) fn new(
        policy: PromptCacheWarmingPolicy,
        request: &ModelRequest,
        usage: ion_ai::Usage,
        request_started: tokio::time::Instant,
    ) -> Option<Self> {
        if !policy.should_warm(usage) {
            return None;
        }
        let refresh_after = policy.refresh_after()?;
        // Discard ineligible delays before constructing their deadlines.
        if refresh_after >= ACTIVE_CACHE_WARMING_LIMIT {
            return None;
        }
        let next_refresh = request_started + refresh_after;
        let expires_at = request_started + std::time::Duration::from_secs(policy.lifetime_seconds);
        let stop_at = request_started + ACTIVE_CACHE_WARMING_LIMIT;
        Some(Self {
            request: request.clone(),
            policy,
            refresh_after,
            next_refresh,
            expires_at,
            stop_at,
        })
    }

    fn reschedule(&mut self, usage: ion_ai::Usage, refresh_started: tokio::time::Instant) -> bool {
        if !self.policy.should_warm(usage) {
            return false;
        }
        self.next_refresh = refresh_started + self.refresh_after;
        self.expires_at =
            refresh_started + std::time::Duration::from_secs(self.policy.lifetime_seconds);
        self.next_refresh < self.stop_at
    }
}

pub(super) async fn execute_with_cache_warming(
    service: &Arc<dyn ModelService>,
    session: &Session,
    turn: u64,
    tool: ion_ai::BoxFuture<'_, Result<ToolExecution, AgentError>>,
    stop: &CancellationToken,
    warmer: &mut Option<PromptCacheWarmer>,
) -> Result<ToolExecution, AgentError> {
    tokio::pin!(tool);
    loop {
        let Some(state) = warmer.as_ref() else {
            return tool.await;
        };
        let deadline = state.next_refresh;
        let valid_until = state.expires_at.min(state.stop_at);
        let request = state.request.clone();
        let refresh = async {
            tokio::time::sleep_until(deadline).await;
            let started = tokio::time::Instant::now();
            if started >= valid_until {
                return (started, None);
            }
            (started, refresh_prompt_cache(service, request, stop).await)
        };
        tokio::pin!(refresh);
        tokio::select! {
            output = &mut tool => return output,
            refreshed = &mut refresh => {
                let (started, Some((execution, usage))) = refreshed else {
                    *warmer = None;
                    continue;
                };
                // Optimization accounting cannot interrupt owned effects. The
                // next correctness-critical write still surfaces storage faults.
                let _ = session.record_cache_warm(turn, execution, usage);
                if !warmer.as_mut().is_some_and(|state| state.reschedule(usage, started)) {
                    *warmer = None;
                }
            }
            () = stop.cancelled() => {
                *warmer = None;
                // Stop joins the executor and delegation cleanup; never drop it.
                return tool.await;
            }
        }
    }
}
