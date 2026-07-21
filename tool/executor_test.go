package tool

import (
	"context"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestLocalExecutorHoldsProcessBeforeDurableIdentityCallback(t *testing.T) {
	workdir := t.TempDir()
	marker := filepath.Join(workdir, "started")
	executor := newLocalExecutorWithEnvironment(
		SandboxOff,
		NewEnvironmentPolicy(executorEnvironmentAllowlist, nil),
	)
	callbackEntered := make(chan struct{})
	release := make(chan struct{})
	done := make(chan error, 1)
	go func() {
		_, err := executor.Run(context.Background(), localCommand{
			CWD: workdir, Command: "touch started", Started: func(int) error {
				close(callbackEntered)
				<-release
				return nil
			},
		})
		done <- err
	}()

	select {
	case <-callbackEntered:
	case <-time.After(time.Second):
		t.Fatal("process identity callback was not reached")
	}
	if _, err := os.Stat(marker); !os.IsNotExist(err) {
		t.Fatalf("command crossed handshake before durable identity callback: stat err=%v", err)
	}
	close(release)
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("executor.Run: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("executor did not finish after handshake release")
	}
	if _, err := os.Stat(marker); err != nil {
		t.Fatalf("command did not execute after handshake release: %v", err)
	}
}

func TestEnvironmentAllowlistExcludesCredentialsAndArbitraryVariables(t *testing.T) {
	t.Setenv("OPENAI_API_KEY", "secret")
	t.Setenv("ION_UNLISTED", "hidden")
	t.Setenv("PATH", "/bin")
	policy := NewEnvironmentPolicy(executorEnvironmentAllowlist, nil)
	env := policy.CommandEnvironment()
	seenPath := false
	for _, item := range env {
		if item == "PATH=/bin" {
			seenPath = true
		}
		if item == "OPENAI_API_KEY=secret" || item == "ION_UNLISTED=hidden" {
			t.Fatalf("allowlisted environment leaked %q", item)
		}
	}
	if !seenPath {
		t.Fatalf("allowlisted environment = %#v, want PATH", env)
	}
	if got := policy.AllowedVariables(); len(got) == 0 || got[0] == "*" {
		t.Fatalf("allowlist identity = %#v, want explicit variables", got)
	}
}
