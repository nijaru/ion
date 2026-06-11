package agent

import "testing"

func TestIsRetryableError(t *testing.T) {
	tests := []struct {
		name     string
		input    string
		expected bool
	}{
		// Empty/no error
		{"empty", "", false},

		// Retryable errors (transient)
		{"overloaded", "overloaded_error", true},
		{"rate_limit", "rate limit exceeded", true},
		{"too_many_requests", "too many requests", true},
		{"429", "429 Too Many Requests", true},
		{"500", "500 Internal Server Error", true},
		{"502", "502 Bad Gateway", true},
		{"503", "503 Service Unavailable", true},
		{"504", "504 Gateway Timeout", true},
		{"service_unavailable", "service unavailable", true},
		{"server_error", "server error", true},
		{"network_error", "network error", true},
		{"connection_error", "connection error", true},
		{"connection_refused", "connection refused", true},
		{"connection_lost", "connection lost", true},
		{"websocket_closed", "websocket closed", true},
		{"fetch_failed", "fetch failed", true},
		{"socket_hang_up", "socket hang up", true},
		{"timeout", "request timed out", true},
		{"timeout_variant", "request timeout", true},
		{"terminated", "terminated", true},

		// Non-retryable errors (permanent)
		{"context_overflow", "prompt is too long: 213462 tokens > 200000 maximum", false},
		{"billing", "Monthly usage limit reached", false},
		{"quota_exceeded", "quota exceeded", false},
		{"insufficient_funds", "insufficient_quota", false},
		{"out_of_budget", "out of budget", false},

		// Unrelated errors
		{"auth_error", "invalid API key", false},
		{"generic_error", "something went wrong", false},
		{"not_retryable", "bad request", false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := IsRetryableError(tt.input)
			if got != tt.expected {
				t.Fatalf("IsRetryableError(%q) = %v, want %v", tt.input, got, tt.expected)
			}
		})
	}
}
