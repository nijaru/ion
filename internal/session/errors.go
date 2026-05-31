package session

import (
	"strings"

	"github.com/nijaru/ion/internal/privacy"
)

type ProviderLimitError struct {
	Reason string
	Label  string
	Raw    string
}

func ClassifyProviderLimitError(err error) (ProviderLimitError, bool) {
	if err == nil {
		return ProviderLimitError{}, false
	}
	raw := strings.TrimSpace(err.Error())
	if raw == "" {
		return ProviderLimitError{}, false
	}
	lower := strings.ToLower(raw)
	for _, marker := range []string{
		"context_length_exceeded",
		"context length",
		"maximum context",
		"max context",
		"token limit",
		"too many tokens",
	} {
		if strings.Contains(lower, marker) {
			return ProviderLimitError{
				Reason: "context_limit",
				Label:  "API context limit",
				Raw:    raw,
			}, true
		}
	}
	for _, marker := range []string{
		"insufficient_quota",
		"usage limit",
		"quota",
		"billing",
		"credit",
		"credits",
		"balance",
		"spend limit",
	} {
		if strings.Contains(lower, marker) {
			return ProviderLimitError{
				Reason: "quota_limit",
				Label:  "API quota or usage limit",
				Raw:    raw,
			}, true
		}
	}
	for _, marker := range []string{
		"status code: 429",
		" 429 ",
		"too many requests",
		"rate limit",
		"rate_limit",
		"requests per",
		"tokens per",
	} {
		if strings.Contains(lower, marker) {
			return ProviderLimitError{
				Reason: "rate_limit",
				Label:  "API rate limit",
				Raw:    raw,
			}, true
		}
	}
	for _, marker := range []string{
		"resource_exhausted",
		"overloaded",
		"capacity",
		"temporarily unavailable",
	} {
		if strings.Contains(lower, marker) {
			return ProviderLimitError{
				Reason: "provider_capacity",
				Label:  "Provider capacity limit",
				Raw:    raw,
			}, true
		}
	}
	return ProviderLimitError{}, false
}

func (e ProviderLimitError) Display() string {
	return e.Label + ": " + e.Raw
}

func DisplayError(err error) string {
	if err == nil {
		return "session error"
	}
	raw := strings.Join(strings.Fields(err.Error()), " ")
	if raw == "" {
		return "session error"
	}
	lower := strings.ToLower(raw)
	if strings.Contains(lower, "assistant response has no content") {
		return "Provider returned an empty response. Try again or switch models."
	}
	if strings.Contains(lower, "status code: 422") ||
		strings.Contains(lower, "unprocessable entity") {
		return "Model rejected the request (422). This often happens with reasoning models that don't accept temperature. Try /model to switch models."
	}
	if strings.Contains(lower, "message has invalid role") {
		return "Session history contains an invalid message role. Try starting a new session with /session."
	}
	if strings.Contains(lower, "too many empty") || strings.Contains(lower, "empty messages") {
		return "Provider sent too many empty responses. Try again or switch models with /model."
	}
	return privacy.Redact(raw)
}
