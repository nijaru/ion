package session

import (
	"errors"
	"strings"
	"testing"
)

func TestDisplayError(t *testing.T) {
	tests := []struct {
		name string
		err  error
		want string
	}{
		{
			name: "nil error",
			want: "session error",
		},
		{
			name: "blank error",
			err:  errors.New("   "),
			want: "session error",
		},
		{
			name: "empty assistant response",
			err:  errors.New("assistant response has no content"),
			want: "Provider returned an empty response. Try again or switch models.",
		},
		{
			name: "reasoning model 422",
			err:  errors.New("status code: 422 unprocessable entity"),
			want: "Model rejected the request (422). This often happens with reasoning models that don't accept temperature. Try /model to switch models.",
		},
		{
			name: "invalid role",
			err:  errors.New("message has invalid role"),
			want: "Session history contains an invalid message role. Try starting a new session with /session.",
		},
		{
			name: "too many empty responses",
			err:  errors.New("too many empty messages"),
			want: "Provider sent too many empty responses. Try again or switch models with /model.",
		},
		{
			name: "redacts raw provider text",
			err:  errors.New("request failed with sk-test1234567890"),
			want: "request failed with [redacted-secret]",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := DisplayError(tt.err); got != tt.want {
				t.Fatalf("DisplayError() = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestClassifyProviderLimitError(t *testing.T) {
	tests := []struct {
		name       string
		err        error
		wantOK     bool
		wantReason string
		wantLabel  string
	}{
		{
			name:       "context limit",
			err:        errors.New("context_length_exceeded: too many tokens"),
			wantOK:     true,
			wantReason: "context_limit",
			wantLabel:  "API context limit",
		},
		{
			name:       "quota limit",
			err:        errors.New("insufficient_quota: billing hard limit has been reached"),
			wantOK:     true,
			wantReason: "quota_limit",
			wantLabel:  "API quota or usage limit",
		},
		{
			name:       "rate limit",
			err:        errors.New("status code: 429 Too Many Requests: rate limit exceeded"),
			wantOK:     true,
			wantReason: "rate_limit",
			wantLabel:  "API rate limit",
		},
		{
			name:       "provider capacity",
			err:        errors.New("resource_exhausted: model overloaded"),
			wantOK:     true,
			wantReason: "provider_capacity",
			wantLabel:  "Provider capacity limit",
		},
		{
			name:   "reasoning model 422 stays display-only",
			err:    errors.New("status code: 422 unprocessable entity"),
			wantOK: false,
		},
		{
			name:   "ordinary error",
			err:    errors.New("backend failed"),
			wantOK: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, ok := ClassifyProviderLimitError(tt.err)
			if ok != tt.wantOK {
				t.Fatalf("ClassifyProviderLimitError() ok = %v, want %v", ok, tt.wantOK)
			}
			if !tt.wantOK {
				return
			}
			if got.Reason != tt.wantReason || got.Label != tt.wantLabel ||
				got.Raw != strings.TrimSpace(tt.err.Error()) {
				t.Fatalf("ClassifyProviderLimitError() = %#v", got)
			}
			if got.Display() != got.Label+": "+got.Raw {
				t.Fatalf("Display() = %q", got.Display())
			}
		})
	}
}
