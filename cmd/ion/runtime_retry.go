package main

import (
	"time"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/llm"
)

// providerWithRetryPolicy installs the runtime-owned transient failure policy.
// Transport failures may wait for the caller to cancel the active turn; API
// failures remain bounded so a provider-side outage cannot spin forever.
func retryPolicyForConfig(cfg *config.Config) llm.RetryConfig {
	policy := llm.DefaultRetryConfig()
	policy.MaxAttempts = cfg.GetMaxRetries()
	policy.MinInterval = time.Duration(cfg.GetRetryBaseDelayMs()) * time.Millisecond
	policy.RetryForeverTransportOnly = true
	policy.RetryForever = cfg.RetryUntilCancelledEnabled()
	if !policy.RetryForever {
		policy.MaxAttempts = 1
	}
	return policy
}

func providerWithRetryPolicy(provider llm.Provider, cfg *config.Config) llm.Provider {
	if provider == nil {
		return nil
	}

	retry := llm.NewRetryProvider(provider)
	retry.Config = retryPolicyForConfig(cfg)
	return retry
}
