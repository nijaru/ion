package main

import (
	"context"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/canto/llm"
	cantobackend "github.com/nijaru/ion/internal/backend/canto"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func TestLiveSmokeTurnAndToolCall(t *testing.T) {
	if !liveSmokeEnabled() {
		t.Skip("set ION_LIVE_SMOKE=1 to run live smoke coverage")
	}

	baseCfg, err := config.Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
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

	cfg := *baseCfg
	cfg.Provider = provider
	cfg.Model = modelName
	if endpoint := strings.TrimSpace(os.Getenv("ION_SMOKE_ENDPOINT")); endpoint != "" {
		cfg.Endpoint = endpoint
	}

	requests := &liveRequestRecorder{}
	restoreRequestObserver := cantobackend.SetProviderRequestObserverForTest(requests.record)
	t.Cleanup(restoreRequestObserver)

	b, sess, err := openRuntime(ctx, store, cwd, "smoke", &cfg, "", "")
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
		seenTurnStarted  bool
		seenToolCall     bool
		seenAgentText    bool
		seenTurnFinished bool
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
			case session.AgentDelta:
				if strings.TrimSpace(msg.Delta) != "" {
					t.Logf("agent delta: %q", msg.Delta)
					seenAgentText = true
				}
			case session.AgentMessage:
				t.Logf("agent message committed")
				seenAgentText = true
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
	if !seenAgentText {
		t.Fatal("expected streamed agent text during live smoke")
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
	foundAgent := false
	for _, entry := range entries {
		if entry.Role == session.User && entry.Content == prompt {
			foundUser = true
		}
		if entry.Role == session.Agent && strings.TrimSpace(entry.Content) != "" {
			foundAgent = true
		}
	}

	if !foundUser {
		t.Fatalf("user prompt %q not found in persisted session", prompt)
	}
	if !foundAgent {
		t.Fatal("agent response not found in persisted session")
	}

	resumedCfg := cfg
	t.Log("opening runtime against resumed session")
	resumedBackend, resumedSess, err := openRuntime(ctx, store, cwd, "smoke", &resumedCfg, "", sess.ID())
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

	resumePrompt := smokeEnv(
		"ION_SMOKE_RESUME_PROMPT",
		"Reply with the single word continued if the earlier session included the exact text ion-smoke, otherwise reply with the single word fresh.",
	)
	agentText, sawTool := runSmokeTurn(ctx, t, resumedBackend.Session(), resumePrompt, false)
	if sawTool {
		t.Fatal("resume follow-up should not require a tool call")
	}
	assertResumeProviderHistory(t, requests, prompt, resumePrompt)
	if !strings.Contains(strings.ToLower(agentText), "continued") {
		t.Logf(
			"resume follow-up agent text = %q, but provider request contained prior tool history; treating as model/provider semantic miss",
			agentText,
		)
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

	switchCfg := cfg
	switchCfg.Provider = switchProvider
	switchCfg.Model = switchModel
	t.Logf("opening switched runtime: provider=%s model=%s", switchProvider, switchModel)
	switchedBackend, switchedSess, err := openRuntime(ctx, store, cwd, "smoke", &switchCfg, "", sess.ID())
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

	agentText, sawTool = runSmokeTurn(ctx, t, switchedBackend.Session(), switchPrompt, false)
	if sawTool {
		t.Fatal("swap phase should not require a tool call")
	}
	assertResumeProviderHistory(t, requests, prompt, switchPrompt)
	if !strings.Contains(strings.ToLower(agentText), "continued") {
		t.Logf(
			"swap agent text = %q, but provider request contained prior tool history; treating as model/provider semantic miss",
			agentText,
		)
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

func smokeEnv(name, defaultValue string) string {
	if value := strings.TrimSpace(os.Getenv(name)); value != "" {
		return value
	}
	return defaultValue
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
		seenTurnStarted  bool
		seenToolCall     bool
		seenAgentText    bool
		seenTurnFinished bool
		agentText        strings.Builder
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
			case session.AgentDelta:
				if strings.TrimSpace(msg.Delta) != "" {
					t.Logf("agent delta: %q", msg.Delta)
					seenAgentText = true
					agentText.WriteString(msg.Delta)
				}
			case session.AgentMessage:
				t.Logf("agent message committed")
				seenAgentText = true
				if msg.Message != "" {
					agentText.Reset()
					agentText.WriteString(msg.Message)
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
				if !seenAgentText {
					t.Fatal("expected streamed agent text during live smoke")
				}
				return agentText.String(), seenToolCall
			}
		case <-deadline.C:
			t.Fatal("timed out waiting for live smoke turn to finish")
		}
	}
}

type liveRequestRecorder struct {
	mu       sync.Mutex
	requests []liveRecordedRequest
}

type liveRecordedRequest struct {
	Provider string
	Model    string
	Messages []llm.Message
}

func (r *liveRequestRecorder) record(provider string, req *llm.Request) {
	if req == nil {
		return
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	r.requests = append(r.requests, liveRecordedRequest{
		Provider: provider,
		Model:    req.Model,
		Messages: req.Messages,
	})
}

func (r *liveRequestRecorder) findRequestWithUser(content string) (liveRecordedRequest, bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	for i := len(r.requests) - 1; i >= 0; i-- {
		req := r.requests[i]
		if messageIndex(req.Messages, llm.RoleUser, content, 0) >= 0 {
			return req, true
		}
	}
	return liveRecordedRequest{}, false
}

func (r *liveRequestRecorder) summary() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	var b strings.Builder
	for i, req := range r.requests {
		if i > 0 {
			b.WriteString("; ")
		}
		b.WriteString(req.Provider)
		b.WriteString("/")
		b.WriteString(req.Model)
		b.WriteString(" roles=")
		for j, msg := range req.Messages {
			if j > 0 {
				b.WriteString(",")
			}
			b.WriteString(string(msg.Role))
			if len(msg.Calls) > 0 {
				b.WriteString("(tool-call)")
			}
			if msg.Role == llm.RoleTool {
				b.WriteString("(tool-result)")
			}
		}
	}
	return b.String()
}

func assertResumeProviderHistory(t *testing.T, requests *liveRequestRecorder, firstPrompt, resumePrompt string) {
	t.Helper()

	req, ok := requests.findRequestWithUser(resumePrompt)
	if !ok {
		t.Fatalf(
			"resume provider request containing %q was not captured; captured requests: %s",
			resumePrompt,
			requests.summary(),
		)
	}

	firstUser := messageIndex(req.Messages, llm.RoleUser, firstPrompt, 0)
	if firstUser < 0 {
		t.Fatalf("resume provider request missing first user prompt %q; request roles: %s", firstPrompt, requestRoles(req.Messages))
	}

	toolCall, toolCallID := toolCallIndex(req.Messages, "bash", "echo ion-smoke", firstUser+1)
	if toolCall < 0 {
		t.Fatalf("resume provider request missing bash tool call for ion-smoke; request roles: %s", requestRoles(req.Messages))
	}

	toolResult := toolResultIndex(req.Messages, toolCallID, "ion-smoke", toolCall+1)
	if toolResult < 0 {
		t.Fatalf("resume provider request missing matching ion-smoke tool result for %q; request roles: %s", toolCallID, requestRoles(req.Messages))
	}

	assistant := assistantContentIndex(req.Messages, toolResult+1)
	if assistant < 0 {
		t.Fatalf("resume provider request missing final assistant message after tool result; request roles: %s", requestRoles(req.Messages))
	}

	resumeUser := messageIndex(req.Messages, llm.RoleUser, resumePrompt, assistant+1)
	if resumeUser < 0 {
		t.Fatalf("resume provider request missing follow-up prompt after prior history; request roles: %s", requestRoles(req.Messages))
	}
	t.Logf(
		"resume provider request verified: first_user=%d tool_call=%d tool_result=%d assistant=%d resume_user=%d",
		firstUser,
		toolCall,
		toolResult,
		assistant,
		resumeUser,
	)
}

func messageIndex(messages []llm.Message, role llm.Role, content string, start int) int {
	for i := max(start, 0); i < len(messages); i++ {
		if messages[i].Role == role && strings.Contains(messages[i].Content, content) {
			return i
		}
	}
	return -1
}

func assistantContentIndex(messages []llm.Message, start int) int {
	for i := max(start, 0); i < len(messages); i++ {
		if messages[i].Role == llm.RoleAssistant && strings.TrimSpace(messages[i].Content) != "" {
			return i
		}
	}
	return -1
}

func toolCallIndex(messages []llm.Message, name, argNeedle string, start int) (int, string) {
	for i := max(start, 0); i < len(messages); i++ {
		if messages[i].Role != llm.RoleAssistant {
			continue
		}
		for _, call := range messages[i].Calls {
			if call.Function.Name == name && strings.Contains(call.Function.Arguments, argNeedle) {
				return i, call.ID
			}
		}
	}
	return -1, ""
}

func toolResultIndex(messages []llm.Message, toolCallID, contentNeedle string, start int) int {
	for i := max(start, 0); i < len(messages); i++ {
		if messages[i].Role != llm.RoleTool {
			continue
		}
		if messages[i].ToolID == toolCallID && strings.Contains(messages[i].Content, contentNeedle) {
			return i
		}
	}
	return -1
}

func requestRoles(messages []llm.Message) string {
	parts := make([]string, 0, len(messages))
	for _, msg := range messages {
		part := string(msg.Role)
		if len(msg.Calls) > 0 {
			part += "(tool-call)"
		}
		if msg.Role == llm.RoleTool {
			part += "(tool-result)"
		}
		parts = append(parts, part)
	}
	return strings.Join(parts, ",")
}
