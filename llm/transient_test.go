package llm_test

import (
	"errors"
	"net/http"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
)

func TestHTTPErrorParticipatesInProviderPolicies(t *testing.T) {
	err := &llm.HTTPError{Provider: "openrouter", Code: 429, Body: "rate limited"}
	wrapped := errors.Join(errors.New("request failed"), err)

	if !llm.IsRateLimit(wrapped) {
		t.Fatal("IsRateLimit() = false, want true")
	}
	if got, ok := llm.HTTPStatusCode(wrapped); !ok || got != 429 {
		t.Fatalf("HTTPStatusCode() = (%d, %v), want (429, true)", got, ok)
	}
}

func TestNewHTTPErrorBoundsResponseBody(t *testing.T) {
	err := llm.NewHTTPError("provider", 500, make([]byte, 32<<10))
	if len(err.Body) > 8<<10+3 {
		t.Fatalf("HTTP error body length = %d, want bounded", len(err.Body))
	}
}

func TestParseRetryAfterSupportsSecondsAndHTTPDate(t *testing.T) {
	now := time.Date(2026, 7, 22, 12, 0, 0, 0, time.UTC)

	if got, ok := llm.ParseRetryAfter("2", now); !ok || got != 2*time.Second {
		t.Fatalf("seconds Retry-After = (%s, %v), want (2s, true)", got, ok)
	}
	if got, ok := llm.ParseRetryAfter(
		now.Add(3*time.Second).Format(http.TimeFormat),
		now,
	); !ok ||
		got != 3*time.Second {
		t.Fatalf("date Retry-After = (%s, %v), want (3s, true)", got, ok)
	}
	if got, ok := llm.ParseRetryAfter("not-a-delay", now); ok || got != 0 {
		t.Fatalf("invalid Retry-After = (%s, %v), want (0, false)", got, ok)
	}
	if got, ok := llm.ParseRetryAfter("999999999", now); ok || got != 0 {
		t.Fatalf("unbounded Retry-After = (%s, %v), want (0, false)", got, ok)
	}
}
