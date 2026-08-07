package main

import (
	"time"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/llm"
)

// providerWithRetryPolicy installs the runtime-owned transient failure policy.
// Transport failures may wait for the caller to cancel the active turn; API
// failures remain bounded so a provider-side outage cannot spin forever.
func providerWithRetryPolicy(provider llm.Provider, cfg *config.Config) llm.Provider {
	if provider == nil {
		return nil
	}

	retry := llm.NewRetryProvider(provider)
	retry.Config.MaxAttempts = cfg.GetMaxRetries()
	retry.Config.MinInterval = time.Duration(cfg.GetRetryBaseDelayMs()) * time.Millisecond
	retry.Config.RetryForeverTransportOnly = true
	retry.Config.RetryForever = cfg.RetryUntilCancelledEnabled()
	if !retry.Config.RetryForever {
		retry.Config.MaxAttempts = 1
	}
	return retry
}
