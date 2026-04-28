package tools

import (
	"context"
	"testing"
	"time"
)

func TestVerifyCancellationKillsProcessGroup(t *testing.T) {
	v := &Verify{CWD: t.TempDir()}

	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()
	start := time.Now()
	out, err := v.Execute(ctx, `{"command":"sleep 10 & wait"}`)
	if err != nil {
		t.Fatalf("verify returns structured failure output instead of error: %v", err)
	}
	if elapsed := time.Since(start); elapsed > 2*time.Second {
		t.Fatalf("canceled verification took %s, want prompt process-group cleanup; output=%q", elapsed, out)
	}
}
