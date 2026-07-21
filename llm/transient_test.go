package llm_test

import (
	"errors"
	"testing"

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
