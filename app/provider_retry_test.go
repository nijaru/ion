package app

import (
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

func TestProviderRetryUpdatesStreamingStatus(t *testing.T) {
	model := readyModel(t)
	ts := time.Now()
	model, _ = model.handleSessionEvent(session.ProviderRetry{
		Attempt:   2,
		Delay:     1500 * time.Millisecond,
		Err:       errors.New("upstream\nconnection reset"),
		Timestamp: ts,
	})

	if model.Progress.Mode != StateStreaming {
		t.Fatalf("progress mode = %v, want StateStreaming", model.Progress.Mode)
	}
	if model.Progress.Status != "Provider error: upstream connection reset. Retrying in 1.5s... Ctrl+C stops." {
		t.Fatalf("progress status = %q", model.Progress.Status)
	}
	if !model.Progress.StatusUpdatedAt.Equal(ts) {
		t.Fatalf("status timestamp = %v, want %v", model.Progress.StatusUpdatedAt, ts)
	}
}

func TestProviderRetryStatusBoundsProviderErrorText(t *testing.T) {
	longError := strings.Repeat("x", 200)
	status := providerRetryStatus(session.ProviderRetry{Err: errors.New(longError)})
	if len(status) > 240 {
		t.Fatalf("status length = %d, want bounded", len(status))
	}
	if !strings.Contains(status, "Retrying now") {
		t.Fatalf("status = %q, want immediate retry text", status)
	}
}
