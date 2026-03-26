package main

import (
	"context"
	"os"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/app"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func TestLiveSmokeTurnAndToolCall(t *testing.T) {
	if !liveSmokeEnabled() {
		t.Skip("set ION_LIVE_SMOKE=1 to run live smoke coverage")
	}

	home := t.TempDir()
	t.Setenv("HOME", home)

	provider := smokeEnv("ION_SMOKE_PROVIDER", "openrouter")
	modelName := smokeEnv("ION_SMOKE_MODEL", "deepseek/deepseek-v3.2")
	prompt := smokeEnv(
		"ION_SMOKE_PROMPT",
		"Use the bash tool exactly once to run `echo ion-smoke`, then reply with the single word `done`.",
	)

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	t.Cleanup(cancel)

	dataDir, err := config.DefaultDataDir()
	if err != nil {
		t.Fatalf("default data dir: %v", err)
	}

	store, err := storage.NewCantoStore(dataDir)
	if err != nil {
		t.Fatalf("new store: %v", err)
	}

	cwd, err := os.Getwd()
	if err != nil {
		t.Fatalf("cwd: %v", err)
	}

	cfg := &config.Config{Provider: provider, Model: modelName}
	b, sess, err := openRuntime(ctx, store, cwd, "smoke", cfg, "", "")
	if err != nil {
		if isLiveSmokeUnavailable(err) {
			t.Skipf("live smoke unavailable: %v", err)
		}
		t.Fatalf("open runtime: %v", err)
	}
	agent := b.Session()
	t.Cleanup(func() {
		_ = agent.Close()
		if sess != nil {
			_ = sess.Close()
		}
	})

	model := app.New(b, sess, cwd, "smoke", "dev", nil).
		WithStartupLines(startupBannerLines(b.Provider(), b.Model(), false))
	model = applyUpdate(t, model, tea.WindowSizeMsg{Width: 120, Height: 40})

	for _, r := range prompt {
		model = applyUpdate(t, model, tea.KeyPressMsg{Text: string(r), Code: r})
	}
	model = applyUpdate(t, model, tea.KeyPressMsg{Code: tea.KeyEnter})

	var (
		seenTurnStarted   bool
		seenToolCall      bool
		seenAssistantText bool
		seenTurnFinished  bool
	)

	deadline := time.NewTimer(90 * time.Second)
	defer deadline.Stop()

loop:
	for {
		select {
		case ev, ok := <-agent.Events():
			if !ok {
				t.Fatal("event stream closed before smoke turn completed")
			}
			switch msg := ev.(type) {
			case session.ApprovalRequest:
				if err := agent.Approve(ctx, msg.RequestID, true); err != nil {
					t.Fatalf("approve %s: %v", msg.RequestID, err)
				}
			case session.TurnStarted:
				seenTurnStarted = true
			case session.ToolCallStarted:
				seenToolCall = true
			case session.AssistantDelta:
				if strings.TrimSpace(msg.Delta) != "" {
					seenAssistantText = true
				}
			case session.AssistantMessage:
				seenAssistantText = true
			case session.Error:
				t.Fatalf("live smoke session error: %v", msg.Err)
			case session.TurnFinished:
				seenTurnFinished = true
			}
			model = applyUpdate(t, model, ev)
			if seenTurnFinished {
				break loop
			}
		case <-deadline.C:
			t.Fatal("timed out waiting for live smoke turn to finish")
		}
	}

	if !seenTurnStarted {
		t.Fatal("expected TurnStarted event")
	}
	if !seenToolCall {
		t.Fatal("expected at least one tool call during live smoke")
	}
	if !seenAssistantText {
		t.Fatal("expected streamed assistant text during live smoke")
	}
	if !seenTurnFinished {
		t.Fatal("expected TurnFinished event")
	}

	resumed, err := store.ResumeSession(ctx, sess.ID())
	if err != nil {
		t.Fatalf("resume session: %v", err)
	}

	entries, err := resumed.Entries(ctx)
	if err != nil {
		t.Fatalf("read entries: %v", err)
	}

	foundUser := false
	foundAssistant := false
	for _, entry := range entries {
		if entry.Role == session.User && entry.Content == prompt {
			foundUser = true
		}
		if entry.Role == session.Assistant && strings.TrimSpace(entry.Content) != "" {
			foundAssistant = true
		}
	}

	if !foundUser {
		t.Fatalf("user prompt %q not found in persisted session", prompt)
	}
	if !foundAssistant {
		t.Fatal("assistant response not found in persisted session")
	}

	// Exercise the render path once on the live session so markdown/status
	// regressions fail the smoke test instead of only the unit suite.
	if view := model.View(); strings.TrimSpace(view.Content) == "" {
		t.Fatal("expected non-empty rendered view")
	}
}

func applyUpdate(t *testing.T, model app.Model, msg tea.Msg) app.Model {
	t.Helper()

	updated, _ := model.Update(msg)
	next, ok := updated.(app.Model)
	if !ok {
		t.Fatalf("expected app.Model after %T, got %T", msg, updated)
	}
	return next
}

func liveSmokeEnabled() bool {
	value := strings.TrimSpace(os.Getenv("ION_LIVE_SMOKE"))
	return value == "1" || strings.EqualFold(value, "true") || strings.EqualFold(value, "yes")
}

func smokeEnv(name, fallback string) string {
	if value := strings.TrimSpace(os.Getenv(name)); value != "" {
		return value
	}
	return fallback
}

func isLiveSmokeUnavailable(err error) bool {
	if err == nil {
		return false
	}
	msg := err.Error()
	for _, needle := range []string{
		"not set",
		"environment variable not set",
		"unsupported provider",
	} {
		if strings.Contains(msg, needle) {
			return true
		}
	}
	return false
}
