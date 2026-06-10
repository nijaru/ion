package agent

import (
	"fmt"
	"testing"
)

func TestNewAgentError(t *testing.T) {
	tests := []struct {
		name      string
		msg       string
		cause     error
		wantCode  ErrorCode
		wantRetry bool
	}{
		{"overflow", "prompt is too long: 213462 tokens > 200000 maximum", nil, ErrCodeOverflow, true},
		{"rate limit", "Rate limit exceeded for requests", nil, ErrCodeRateLimit, true},
		{"auth", "Invalid API key provided", nil, ErrCodeAuth, false},
		{"timeout", "Request timed out after 30s", nil, ErrCodeTimeout, true},
		{"provider", "Internal server error", nil, ErrCodeProvider, false},
		{"with cause", "prompt is too long", fmt.Errorf("underlying"), ErrCodeOverflow, true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := NewAgentError(tt.msg, tt.cause)
			if err.Code != tt.wantCode {
				t.Errorf("Code = %q, want %q", err.Code, tt.wantCode)
			}
			if err.IsRetryable != tt.wantRetry {
				t.Errorf("IsRetryable = %v, want %v", err.IsRetryable, tt.wantRetry)
			}
			if err.Message != tt.msg {
				t.Errorf("Message = %q, want %q", err.Message, tt.msg)
			}
			if err.Cause != tt.cause {
				t.Errorf("Cause = %v, want %v", err.Cause, tt.cause)
			}
		})
	}
}

func TestAgentErrorUnwrap(t *testing.T) {
	cause := fmt.Errorf("underlying")
	err := NewAgentError("test", cause)
	if err.Unwrap() != cause {
		t.Errorf("Unwrap() = %v, want %v", err.Unwrap(), cause)
	}
}

func TestAgentErrorString(t *testing.T) {
	err := NewAgentError("test error", nil)
	want := "provider_error: test error"
	if got := err.Error(); got != want {
		t.Errorf("Error() = %q, want %q", got, want)
	}

	err = NewAgentError("test error", fmt.Errorf("cause"))
	want = "provider_error: test error: cause"
	if got := err.Error(); got != want {
		t.Errorf("Error() = %q, want %q", got, want)
	}
}
