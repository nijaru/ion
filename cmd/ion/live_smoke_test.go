package main

import (
	"context"
	"os"
	"strings"
	"testing"
	"time"

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
	t.Logf("opened runtime: backend=%s provider=%s model=%s status=%q session=%s", b.Name(), b.Provider(), b.Model(), b.Bootstrap().Status, sess.ID())
	agent := b.Session()
	t.Cleanup(func() {
		_ = agent.Close()
		if sess != nil {
			_ = sess.Close()
		}
	})

	var (
		seenTurnStarted   bool
		seenToolCall      bool
		seenAssistantText bool
		seenTurnFinished  bool
	)

	if err := agent.SubmitTurn(ctx, prompt); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	t.Logf("submitted prompt: %q", prompt)

	deadline := time.NewTimer(90 * time.Second)
	defer deadline.Stop()

loop:
	for {
		select {
		case ev, ok := <-agent.Events():
			if !ok {
				t.Fatal("event stream closed before smoke turn completed")
			}
			t.Logf("event: %T %#v", ev, ev)
			switch msg := ev.(type) {
			case session.ApprovalRequest:
				t.Logf("auto-approving request %s: %s", msg.RequestID, msg.Description)
				if err := agent.Approve(ctx, msg.RequestID, true); err != nil {
					t.Fatalf("approve %s: %v", msg.RequestID, err)
				}
			case session.TurnStarted:
				seenTurnStarted = true
			case session.ToolCallStarted:
				t.Logf("tool call started: %s args=%s", msg.ToolName, msg.Args)
				seenToolCall = true
			case session.AssistantDelta:
				if strings.TrimSpace(msg.Delta) != "" {
					t.Logf("assistant delta: %q", msg.Delta)
					seenAssistantText = true
				}
			case session.AssistantMessage:
				t.Logf("assistant message committed")
				seenAssistantText = true
			case session.Error:
				t.Fatalf("live smoke session error: %v", msg.Err)
			case session.TurnFinished:
				t.Logf("turn finished")
				seenTurnFinished = true
			}
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

	t.Log("closing live session before resume check")
	_ = agent.Close()
	_ = sess.Close()

	t.Log("reopening persisted session for resume check")
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

	resumedCfg := &config.Config{Provider: provider, Model: modelName}
	t.Log("opening runtime against resumed session")
	resumedBackend, resumedSess, err := openRuntime(ctx, store, cwd, "smoke", resumedCfg, "", sess.ID())
	if err != nil {
		t.Fatalf("resume runtime: %v", err)
	}
	t.Logf("resumed runtime: backend=%s provider=%s model=%s status=%q session=%s", resumedBackend.Name(), resumedBackend.Provider(), resumedBackend.Model(), resumedBackend.Bootstrap().Status, resumedSess.ID())
	t.Cleanup(func() {
		if resumedBackend != nil && resumedBackend.Session() != nil {
			_ = resumedBackend.Session().Close()
		}
		if resumedSess != nil {
			_ = resumedSess.Close()
		}
	})
	if got := resumedBackend.Session().ID(); got != sess.ID() {
		t.Fatalf("resumed session ID = %q, want %q", got, sess.ID())
	}

	switchProvider := strings.TrimSpace(os.Getenv("ION_SMOKE_SWITCH_PROVIDER"))
	switchModel := strings.TrimSpace(os.Getenv("ION_SMOKE_SWITCH_MODEL"))
	if switchProvider == "" && switchModel == "" {
		return
	}
	if switchProvider == "" || switchModel == "" {
		t.Fatal("set both ION_SMOKE_SWITCH_PROVIDER and ION_SMOKE_SWITCH_MODEL for live swap smoke")
	}

	switchPrompt := smokeEnv(
		"ION_SMOKE_SWITCH_PROMPT",
		"Reply with the single word continued if the earlier session included the exact text ion-smoke, otherwise reply with the single word fresh.",
	)

	t.Log("closing resumed runtime before swap phase")
	_ = resumedBackend.Session().Close()
	_ = resumedSess.Close()

	switchCfg := &config.Config{Provider: switchProvider, Model: switchModel}
	t.Logf("opening switched runtime: provider=%s model=%s", switchProvider, switchModel)
	switchedBackend, switchedSess, err := openRuntime(ctx, store, cwd, "smoke", switchCfg, "", sess.ID())
	if err != nil {
		t.Fatalf("open switched runtime: %v", err)
	}
	t.Cleanup(func() {
		if switchedBackend != nil && switchedBackend.Session() != nil {
			_ = switchedBackend.Session().Close()
		}
		if switchedSess != nil {
			_ = switchedSess.Close()
		}
	})

	assistantText, sawTool := runSmokeTurn(ctx, t, switchedBackend.Session(), switchPrompt, false)
	if sawTool {
		t.Fatal("swap phase should not require a tool call")
	}
	if !strings.Contains(strings.ToLower(assistantText), "continued") {
		t.Fatalf("swap assistant text = %q, want continuation acknowledgment", assistantText)
	}

	switchedEntries, err := switchedSess.Entries(ctx)
	if err != nil {
		t.Fatalf("read switched entries: %v", err)
	}
	foundSwitchPrompt := false
	for _, entry := range switchedEntries {
		if entry.Role == session.User && entry.Content == switchPrompt {
			foundSwitchPrompt = true
			break
		}
	}
	if !foundSwitchPrompt {
		t.Fatalf("swap prompt %q not found in persisted session", switchPrompt)
	}
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

func runSmokeTurn(ctx context.Context, t *testing.T, agent session.AgentSession, prompt string, requireTool bool) (string, bool) {
	t.Helper()

	var (
		seenTurnStarted   bool
		seenToolCall      bool
		seenAssistantText bool
		seenTurnFinished  bool
		assistantText     strings.Builder
	)

	if err := agent.SubmitTurn(ctx, prompt); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	t.Logf("submitted prompt: %q", prompt)

	deadline := time.NewTimer(90 * time.Second)
	defer deadline.Stop()

	for {
		select {
		case ev, ok := <-agent.Events():
			if !ok {
				t.Fatal("event stream closed before smoke turn completed")
			}
			t.Logf("event: %T %#v", ev, ev)
			switch msg := ev.(type) {
			case session.ApprovalRequest:
				t.Logf("auto-approving request %s: %s", msg.RequestID, msg.Description)
				if err := agent.Approve(ctx, msg.RequestID, true); err != nil {
					t.Fatalf("approve %s: %v", msg.RequestID, err)
				}
			case session.TurnStarted:
				seenTurnStarted = true
			case session.ToolCallStarted:
				t.Logf("tool call started: %s args=%s", msg.ToolName, msg.Args)
				seenToolCall = true
			case session.AssistantDelta:
				if strings.TrimSpace(msg.Delta) != "" {
					t.Logf("assistant delta: %q", msg.Delta)
					seenAssistantText = true
					assistantText.WriteString(msg.Delta)
				}
			case session.AssistantMessage:
				t.Logf("assistant message committed")
				seenAssistantText = true
				if msg.Message != "" {
					assistantText.Reset()
					assistantText.WriteString(msg.Message)
				}
			case session.Error:
				t.Fatalf("live smoke session error: %v", msg.Err)
			case session.TurnFinished:
				t.Logf("turn finished")
				seenTurnFinished = true
			}
			if seenTurnFinished {
				if !seenTurnStarted {
					t.Fatal("expected TurnStarted event")
				}
				if requireTool && !seenToolCall {
					t.Fatal("expected at least one tool call during live smoke")
				}
				if !seenAssistantText {
					t.Fatal("expected streamed assistant text during live smoke")
				}
				return assistantText.String(), seenToolCall
			}
		case <-deadline.C:
			t.Fatal("timed out waiting for live smoke turn to finish")
		}
	}
}
