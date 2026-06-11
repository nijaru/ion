package session

import (
	"regexp"
	"strings"
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
	return redact(raw)
}

var redactors = []struct {
	pattern     *regexp.Regexp
	replacement string
}{
	{
		pattern:     regexp.MustCompile(`(?i)(authorization\s*:\s*bearer\s+)[A-Za-z0-9._~+/=-]+`),
		replacement: `${1}[redacted-secret]`,
	},
	{
		pattern: regexp.MustCompile(
			`(?i)\b((?:api[_-]?key|token|secret|password|passwd|pwd)\s*[:=]\s*["']?)[^"',}\s]+`,
		),
		replacement: `${1}[redacted-secret]`,
	},
	{
		pattern:     regexp.MustCompile(`\b(?:sk|pk)-[A-Za-z0-9_-]{12,}\b`),
		replacement: `[redacted-secret]`,
	},
	{
		pattern:     regexp.MustCompile(`(?i)\b[A-Z0-9._%+\-]+@[A-Z0-9.\-]+\.[A-Z]{2,}\b`),
		replacement: `[redacted-email]`,
	},
	{
		pattern: regexp.MustCompile(
			`(?:\+?1[\s.-]?)?(?:\([2-9][0-9]{2}\)|[2-9][0-9]{2})[\s.-]?[0-9]{3}[\s.-]?[0-9]{4}`,
		),
		replacement: `[redacted-phone]`,
	},
}

func redact(text string) string {
	for _, r := range redactors {
		text = r.pattern.ReplaceAllString(text, r.replacement)
	}
	return text
}
