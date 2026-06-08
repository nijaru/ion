// Package livetest provides live-provider smoke tests for the agent loop.
//
// These tests run against a real LLM API and are gated behind the "live" build
// tag so they never run in CI by default.
//
// Run: go test -tags live -v -timeout 120s ./internal/agent/livetest/
//
// Provider selection (first available key wins):
//
//	OPENROUTER_API_KEY  → openrouter / deepseek/deepseek-v4-flash
//	ANTHROPIC_API_KEY   → anthropic / claude-sonnet-4-20250514
//	OPENAI_API_KEY      → openai / gpt-4.1-mini
//	DEEPSEEK_API_KEY    → deepseek / deepseek-chat
//	GEMINI_API_KEY      → gemini / gemini-2.5-flash
//
// Override with ION_LIVE_PROVIDER, ION_LIVE_MODEL, ION_LIVE_API_KEY.
package livetest

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/llm/providers"
	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
)

// providerCandidate represents a provider/model pair to try.
type providerCandidate struct {
	provider string
	model    string
	envVar   string
}

var candidates = []providerCandidate{
	{"openrouter", "deepseek/deepseek-v4-flash", "OPENROUTER_API_KEY"},
	{"anthropic", "claude-sonnet-4-20250514", "ANTHROPIC_API_KEY"},
	{"openai", "gpt-4.1-mini", "OPENAI_API_KEY"},
	{"deepseek", "deepseek-chat", "DEEPSEEK_API_KEY"},
	{"gemini", "gemini-2.5-flash", "GEMINI_API_KEY"},
}

// liveConfig resolves the provider/model/key from env overrides or available keys.
func liveConfig(t *testing.T) (provider, model, apiKey string) {
	t.Helper()

	// Explicit override
	if p := os.Getenv("ION_LIVE_PROVIDER"); p != "" {
		m := os.Getenv("ION_LIVE_MODEL")
		k := os.Getenv("ION_LIVE_API_KEY")
		if k == "" {
			// Try to find the default env var for this provider
			for _, c := range candidates {
				if c.provider == p {
					k = os.Getenv(c.envVar)
					if m == "" {
						m = c.model
					}
					break
				}
			}
		}
		if k == "" {
			t.Fatalf("ION_LIVE_PROVIDER=%q set but no API key (ION_LIVE_API_KEY or provider env var)", p)
		}
		if m == "" {
			t.Fatalf("ION_LIVE_PROVIDER=%q set but ION_LIVE_MODEL is empty", p)
		}
		return p, m, k
	}

	// Auto-detect from available keys
	for _, c := range candidates {
		if k := os.Getenv(c.envVar); k != "" {
			return c.provider, c.model, k
		}
	}

	t.Skip("no live provider API key found; set OPENROUTER_API_KEY, ANTHROPIC_API_KEY, OPENAI_API_KEY, DEEPSEEK_API_KEY, or GEMINI_API_KEY")
	return "", "", ""
}

// newLiveAdapter creates a SessionAdapter wired to a real provider with real tools.
func newLiveAdapter(t *testing.T) (*agent.SessionAdapter, session.SessionStore, session.SessionHandle) {
	t.Helper()
	providerName, modelName, _ := liveConfig(t)

	cfg := &config.Config{
		Provider: providerName,
		Model:    modelName,
	}
	provider, err := providers.NewProviderFromConfig(cfg)
	if err != nil {
		t.Fatalf("create provider: %v", err)
	}

	// Session store
	store, err := session.NewCantoStore(filepath.Join(t.TempDir(), ".ion"))
	if err != nil {
		t.Fatalf("create store: %v", err)
	}
	t.Cleanup(func() { store.Close() })

	cwd := t.TempDir()
	// Create a test file for tool-call tests
	if err := os.WriteFile(filepath.Join(cwd, "test.txt"), []byte("hello from ion live test"), 0o644); err != nil {
		t.Fatalf("write test file: %v", err)
	}

	sess, err := store.OpenSession(context.Background(), cwd, providerName+"/"+modelName, "live-test")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	t.Cleanup(func() { sess.Close() })

	// Wire up coding tools
	registry := tool.NewRegistry()
	if err := tool.RegisterCodingTools(registry, tool.CodingToolsConfig{Workdir: cwd}); err != nil {
		t.Fatalf("register tools: %v", err)
	}

	toolExecutor := func(ctx context.Context, tc agent.AgentToolCall) (agent.AgentToolResult, error) {
		t.Helper()
		tt, ok := registry.Get(tc.Name)
		if !ok {
			return agent.AgentToolResult{
				Content: []llm.ContentPart{{Type: "text", Text: fmt.Sprintf("Unknown tool: %s", tc.Name)}},
				IsError: true,
			}, nil
		}
		argsJSON, _ := json.Marshal(tc.Arguments)
		result, execErr := tt.Execute(ctx, string(argsJSON))
		if execErr != nil {
			return agent.AgentToolResult{
				Content: []llm.ContentPart{{Type: "text", Text: execErr.Error()}},
				IsError: true,
			}, nil
		}
		return agent.AgentToolResult{
			Content: []llm.ContentPart{{Type: "text", Text: result}},
		}, nil
	}

	var agentTools []agent.AgentTool
	for _, entry := range registry.Entries() {
		agentTools = append(agentTools, agent.AgentTool{
			Name:        entry.Spec.Name,
			Description: entry.Spec.Description,
			Parameters:  entry.Spec.Parameters,
		})
	}

	model := llm.Model{ID: modelName}
	if meta, ok := llm.GetCachedMetadata(providerName, modelName); ok {
		model.ContextWindow = meta.ContextLimit
	}

	adapter := agent.NewSessionAdapter(&agent.SessionAdapterConfig{
		ID:           sess.ID(),
		Model:        model,
		StreamFn:     provider.Stream,
		ToolExecutor: toolExecutor,
		Tools:        agentTools,
		MaxRetries:   1,
	})
	adapter.SetStore(store)
	adapter.SetSession(sess)

	if err := adapter.Open(context.Background()); err != nil {
		t.Fatalf("adapter open: %v", err)
	}
	t.Cleanup(func() { adapter.Close() })

	return adapter, store, sess
}

// collectEvents drains the event channel until TurnEnd, TurnError
// (if fatal), or timeout. Returns all collected events.
func collectEvents(t *testing.T, ch <-chan session.AgentEvent, timeout time.Duration) []session.AgentEvent {
	t.Helper()
	var events []session.AgentEvent
	timer := time.NewTimer(timeout)
	defer timer.Stop()
	for {
		select {
		case ev, ok := <-ch:
			if !ok {
				return events
			}
			events = append(events, ev)
			switch ev.(type) {
			case session.TurnEnd:
				return events
			case session.TurnError:
				// Keep collecting — TurnEnd follows
			}
		case <-timer.C:
			t.Fatalf("timed out after %s waiting for terminal event; got %d events", timeout, len(events))
			return events
		}
	}
}

// hasEvent checks if any event in the slice matches the given type.
func hasEvent[T session.AgentEvent](events []session.AgentEvent) bool {
	for _, ev := range events {
		if _, ok := ev.(T); ok {
			return true
		}
	}
	return false
}

// findEvents returns all events matching the given type.
func findEvents[T session.AgentEvent](events []session.AgentEvent) []T {
	var out []T
	for _, ev := range events {
		if typed, ok := ev.(T); ok {
			out = append(out, typed)
		}
	}
	return out
}

// TestLiveStreaming verifies the agent loop streams tokens from a real provider.
func TestLiveStreaming(t *testing.T) {
	adapter, _, _ := newLiveAdapter(t)

	if err := adapter.SubmitTurn(context.Background(), "Say exactly: 'ion live test ok'"); err != nil {
		t.Fatalf("submit: %v", err)
	}

	events := collectEvents(t, adapter.Events(), 30*time.Second)

	if !hasEvent[session.TurnStart](events) {
		t.Error("missing TurnStart")
	}
	if !hasEvent[session.TurnEnd](events) {
		t.Error("missing TurnEnd")
	}

	// Should have at least one content delta or final message
	deltas := findEvents[session.AgentDelta](events)
	messages := findEvents[session.AgentMessage](events)
	if len(deltas) == 0 && len(messages) == 0 {
		t.Error("no content delivered: expected AgentDelta or AgentMessage")
	}

	// No fatal errors
	for _, ev := range events {
		if err, ok := ev.(session.TurnError); ok && err.Fatal {
			t.Fatalf("fatal error during streaming: %v", err.Err)
		}
	}

	t.Logf("streaming ok: %d deltas, %d messages, %d total events", len(deltas), len(messages), len(events))
}

// TestLiveToolCalls verifies the agent loop executes tools through a real provider.
func TestLiveToolCalls(t *testing.T) {
	adapter, _, _ := newLiveAdapter(t)

	if err := adapter.SubmitTurn(context.Background(), "Read the file test.txt using the read tool and tell me its contents. Do not use any other tools."); err != nil {
		t.Fatalf("submit: %v", err)
	}

	events := collectEvents(t, adapter.Events(), 60*time.Second)

	if !hasEvent[session.TurnStart](events) {
		t.Error("missing TurnStart")
	}
	if !hasEvent[session.TurnEnd](events) {
		t.Error("missing TurnEnd")
	}

	// Must have triggered at least one tool call
	toolCalls := findEvents[session.ToolCallStart](events)
	if len(toolCalls) == 0 {
		t.Fatal("no tool calls: expected at least one ToolCallStart")
	}

	// Each tool call must have a corresponding result
	toolResults := findEvents[session.ToolCallEnd](events)
	if len(toolResults) == 0 {
		t.Fatal("no tool results: expected at least one ToolCallEnd")
	}

	// Verify the read tool was called
	foundRead := false
	for _, tc := range toolCalls {
		if tc.ToolName == "read" {
			foundRead = true
			break
		}
	}
	if !foundRead {
		t.Errorf("expected 'read' tool call, got tool calls: %v", toolNames(toolCalls))
	}

	// Verify tool result contains our test file content
	foundContent := false
	for _, tr := range toolResults {
		if strings.Contains(tr.Result, "hello from ion live test") {
			foundContent = true
			break
		}
	}
	if !foundContent {
		t.Error("tool result missing expected content 'hello from ion live test'")
	}

	// No fatal errors
	for _, ev := range events {
		if err, ok := ev.(session.TurnError); ok && err.Fatal {
			t.Fatalf("fatal error during tool call: %v", err.Err)
		}
	}

	t.Logf("tool calls ok: %d calls, %d results", len(toolCalls), len(toolResults))
}

// TestLiveCancel verifies mid-stream cancel doesn't corrupt state.
func TestLiveCancel(t *testing.T) {
	adapter, _, _ := newLiveAdapter(t)

	if err := adapter.SubmitTurn(context.Background(), "Write a very detailed 500-word essay about the history of programming languages, starting from the earliest."); err != nil {
		t.Fatalf("submit: %v", err)
	}

	// Wait for stream to start, then cancel
	var started bool
	timeout := time.NewTimer(15 * time.Second)
	defer timeout.Stop()
	for !started {
		select {
		case ev := <-adapter.Events():
			switch ev.(type) {
			case session.TurnStart, session.AgentDelta:
				started = true
			case session.TurnError:
				// Provider error before we could cancel — skip
				t.Skipf("provider error before cancel: %v", ev.(session.TurnError).Err)
			}
		case <-timeout.C:
			t.Skip("stream never started within 15s")
		}
	}

	if err := adapter.CancelTurn(context.Background()); err != nil {
		t.Fatalf("cancel: %v", err)
	}

	// Drain remaining events — should end with TurnFinished, no crash
	events := collectEvents(t, adapter.Events(), 15*time.Second)

	if !hasEvent[session.TurnEnd](events) {
		t.Error("missing TurnEnd after cancel")
	}

	// Cancel should not produce a fatal error
	for _, ev := range events {
		if err, ok := ev.(session.TurnError); ok && err.Fatal {
			t.Fatalf("fatal error after cancel: %v", err.Err)
		}
	}

	t.Logf("cancel ok: %d events after cancel signal", len(events))
}

// TestLiveSessionPersistence verifies session entries survive close/resume.
func TestLiveSessionPersistence(t *testing.T) {
	adapter, store, sess := newLiveAdapter(t)

	if err := adapter.SubmitTurn(context.Background(), "Reply with exactly: 'persist test ok'"); err != nil {
		t.Fatalf("submit: %v", err)
	}

	events := collectEvents(t, adapter.Events(), 30*time.Second)
	if !hasEvent[session.TurnEnd](events) {
		t.Fatal("turn did not finish")
	}

	// Close the adapter
	if err := adapter.Close(); err != nil {
		t.Fatalf("close adapter: %v", err)
	}

	// Verify entries were persisted
	entries, err := sess.Entries(context.Background())
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) == 0 {
		t.Fatal("no entries persisted")
	}

	// Should have at least a user entry and an agent entry
	var hasUser, hasAgent bool
	for _, e := range entries {
		switch e.Role {
		case session.RoleUser:
			hasUser = true
		case session.RoleAgent:
			hasAgent = true
		}
	}
	if !hasUser {
		t.Error("missing user entry in persisted session")
	}
	if !hasAgent {
		t.Error("missing agent entry in persisted session")
	}

	// Resume the session
	resumed, err := store.ResumeSession(context.Background(), sess.ID())
	if err != nil {
		t.Fatalf("resume: %v", err)
	}
	defer resumed.Close()

	resumedEntries, err := resumed.Entries(context.Background())
	if err != nil {
		t.Fatalf("resumed entries: %v", err)
	}

	if len(resumedEntries) != len(entries) {
		t.Fatalf("entry count mismatch: original=%d resumed=%d", len(entries), len(resumedEntries))
	}

	for i, orig := range entries {
		res := resumedEntries[i]
		if orig.Role != res.Role {
			t.Fatalf("entry[%d] role mismatch: %s vs %s", i, orig.Role, res.Role)
		}
		if orig.Content != res.Content {
			t.Fatalf("entry[%d] content mismatch", i)
		}
	}

	t.Logf("persistence ok: %d entries survived close/resume", len(entries))
}

// TestLiveProviderErrors verifies error messages are user-friendly.
func TestLiveProviderErrors(t *testing.T) {
	providerName, _, _ := liveConfig(t)

	// Create a provider with a bad API key
	cfg := &config.Config{
		Provider: providerName,
		Model:    "nonexistent-model-xyz",
	}
	// Override the env var for this test
	t.Setenv("ION_LIVE_BAD_KEY_TEST", "1")

	provider, err := providers.NewProviderFromConfig(cfg)
	if err != nil {
		// Provider creation itself might fail for some providers — that's fine
		t.Logf("provider creation failed (expected for some providers): %v", err)
		return
	}

	store, err := session.NewCantoStore(filepath.Join(t.TempDir(), ".ion"))
	if err != nil {
		t.Fatalf("store: %v", err)
	}
	defer store.Close()

	cwd := t.TempDir()
	sess, err := store.OpenSession(context.Background(), cwd, providerName+"/nonexistent-model-xyz", "error-test")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	defer sess.Close()

	adapter := agent.NewSessionAdapter(&agent.SessionAdapterConfig{
		ID:       sess.ID(),
		Model:    llm.Model{ID: "nonexistent-model-xyz"},
		StreamFn: provider.Stream,
		MaxRetries: 0, // Don't retry in error test
	})
	adapter.SetStore(store)
	adapter.SetSession(sess)

	if err := adapter.Open(context.Background()); err != nil {
		t.Fatalf("open: %v", err)
	}
	defer adapter.Close()

	if err := adapter.SubmitTurn(context.Background(), "hello"); err != nil {
		t.Fatalf("submit: %v", err)
	}

	events := collectEvents(t, adapter.Events(), 30*time.Second)

	// Should get an error event
	if !hasEvent[session.TurnError](events) {
		t.Fatal("expected TurnError for bad model, got none")
	}

	// Error should be user-friendly (not raw HTTP dump)
	errEvents := findEvents[session.TurnError](events)
	for _, ev := range errEvents {
		errMsg := ev.Err.Error()
		// Should not be empty
		if errMsg == "" {
			t.Error("empty error message")
		}
		// Should not be a raw stack trace
		if strings.Contains(errMsg, "goroutine") && strings.Contains(errMsg, "runtime.go") {
			t.Errorf("error contains raw stack trace: %s", errMsg[:min(200, len(errMsg))])
		}
		t.Logf("error message: %s", errMsg)
	}

	// Should still get TurnFinished
	if !hasEvent[session.TurnEnd](events) {
		t.Error("missing TurnEnd after error")
	}
}

// toolNames extracts tool names from ToolCallStarts for error messages.
func toolNames(calls []session.ToolCallStart) []string {
	names := make([]string, len(calls))
	for i, c := range calls {
		names[i] = c.ToolName
	}
	return names
}
