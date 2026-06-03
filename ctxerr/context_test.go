package ctxerr

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"
)

func TestTimeoutErrorNamesOperationWithoutRawContextText(t *testing.T) {
	err := Timeout("print mode", time.Millisecond, context.DeadlineExceeded)
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("errors.Is(%v, DeadlineExceeded) = false", err)
	}
	if got := err.Error(); got != "print mode timed out after 1ms" {
		t.Fatalf("Timeout error = %q", got)
	}
	if strings.Contains(err.Error(), "context deadline exceeded") {
		t.Fatalf("Timeout error leaked raw context text: %q", err.Error())
	}
}

func TestWrapContextPreservesCancellationCause(t *testing.T) {
	err := WrapContext("bash tool", context.Canceled)
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("errors.Is(%v, Canceled) = false", err)
	}
	if got := err.Error(); got != "bash tool canceled: context canceled" {
		t.Fatalf("canceled error = %q", got)
	}
}
