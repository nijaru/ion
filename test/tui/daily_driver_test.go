package main

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
	iontool "github.com/nijaru/ion/tool"
	toolweb "github.com/nijaru/ion/tool/web"
)

// TestDeterministicTUIDailyDriverJourney composes the promoted built-ins with
// the existing coding tools through the real Controller and Bubble Tea app.
// It proves the representative inspect/research/delegate/edit/test/resume
// path without using credentials or treating a fixture as live-provider proof.
func TestDeterministicTUIDailyDriverJourney(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	workdir := t.TempDir()
	filePath := filepath.Join(workdir, "daily.txt")
	if err := os.WriteFile(filePath, []byte("original\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/search":
			w.Header().Set("Content-Type", "text/html")
			_, _ = fmt.Fprintf(
				w,
				`<html><body><li class="b_algo"><h2><a href="%s/source">Fixture source</a></h2><div class="b_caption"><p>fixture search evidence</p></div></li></body></html>`,
				"http://"+r.Host,
			)
		case "/source":
			w.Header().Set("Content-Type", "text/html; charset=utf-8")
			_, _ = fmt.Fprint(
				w,
				`<html><head><title>Fixture article</title></head><body><p>fixture article evidence</p></body></html>`,
			)
		default:
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	webClient := toolweb.NewClient(toolweb.Config{
		SearchURL:         server.URL + "/search",
		AllowPrivateHosts: true,
		Timeout:           time.Second,
	})
	search := toolweb.NewSearchTool(webClient)
	fetch := toolweb.NewFetchTool(webClient)
	subagent := iontool.NewSubagentTool()
	edit := &iontool.Edit{FileTool: *iontool.NewFileTool(workdir)}
	bash := iontool.NewBash(workdir)

	registered := []iontool.Tool{search, fetch, subagent, edit, bash}
	tools := make([]agent.Tool, 0, len(registered))
	for _, registeredTool := range registered {
		tools = append(tools, dailyDriverAgentTool(registeredTool))
	}

	path := filepath.Join(t.TempDir(), "daily-driver.db")
	store, err := session.NewSQLiteStore(path, "daily-driver")
	if err != nil {
		t.Fatal(err)
	}
	sess := session.NewSession(store, 128)
	provider := &dailyDriverProvider{
		parentSessionID: sess.Meta().ID,
		filePath:        filePath,
		fetchURL:        server.URL + "/source",
	}
	harness := agent.NewController(agent.ControllerConfig{
		Session:             sess,
		Store:               store,
		Durable:             store,
		RequireDurable:      true,
		ActionJournal:       store,
		ApprovalMode:        agent.ApprovalTrusted,
		ApprovalInteractive: false,
		Workdir:             workdir,
		Model:               acceptanceModel(),
		Tools:               tools,
		StreamFn:            provider.stream,
	})
	subagent.SetRunner(harness)

	program, output, result := startAcceptanceProgram(t, store, sess, harness)
	program.Send(keyText("inspect, research, delegate, edit, and test this file"))
	program.Send(keyEnter())
	waitForAcceptanceOutput(t, output, "child research evidence", "child progress")
	waitForAcceptanceOutput(t, output, "daily-driver-reviewed", "daily-driver completion")
	waitForAcceptanceIdle(t, harness)

	content, err := os.ReadFile(filePath)
	if err != nil {
		t.Fatal(err)
	}
	if string(content) != "edited\n" {
		t.Fatalf("daily-driver file = %q, want edited content", content)
	}
	provider.assertParentJourney(t)
	assertDailyDriverToolsPersisted(t, sess)

	program.Quit()
	_ = waitAcceptanceProgram(t, result)
	closeAcceptanceHarness(t, harness, store)

	resumeProvider := newAcceptanceProvider(acceptanceResume)
	store2, err := session.NewSQLiteStore(path, "daily-driver")
	if err != nil {
		t.Fatalf("reopen daily-driver store: %v", err)
	}
	sess2 := session.NewSession(store2, 128)
	harness2 := agent.NewController(agent.ControllerConfig{
		Session:  sess2,
		Store:    store2,
		Durable:  store2,
		Model:    acceptanceModel(),
		StreamFn: resumeProvider.stream,
	})
	program2, output2, result2 := startAcceptanceProgram(t, store2, sess2, harness2)
	program2.Send(keyText("resume the reviewed daily-driver session"))
	program2.Send(keyEnter())
	waitForAcceptanceOutput(t, output2, "resumed-output", "resumed daily-driver output")
	if !resumeProvider.requestContains("daily-driver-reviewed") ||
		!resumeProvider.requestContains("child research evidence") ||
		!resumeProvider.requestContains("edited") {
		t.Fatalf("resumed provider request lost the completed journey: %s", resumeProvider.requestSummary())
	}
	program2.Quit()
	_ = waitAcceptanceProgram(t, result2)
	closeAcceptanceHarness(t, harness2, store2)
}

func keyText(text string) tea.KeyPressMsg {
	return tea.KeyPressMsg{Text: text}
}

func keyEnter() tea.KeyPressMsg {
	return tea.KeyPressMsg{Code: tea.KeyEnter}
}

func dailyDriverAgentTool(registered iontool.Tool) agent.Tool {
	spec := registered.Spec()
	metadata := iontool.MetadataFor(registered)
	var approval func(json.RawMessage) (agent.ApprovalRequirement, bool, error)
	if provider, ok := registered.(iontool.RequirementProvider); ok {
		approval = func(args json.RawMessage) (agent.ApprovalRequirement, bool, error) {
			requirement, required, err := provider.ApprovalRequirement(string(args))
			if err != nil {
				return agent.ApprovalRequirement{}, false, err
			}
			return agent.ApprovalRequirement{
				Category:      requirement.Category,
				Operation:     requirement.Operation,
				Resource:      requirement.Resource,
				Paths:         append([]string(nil), requirement.Paths...),
				Environment:   append([]string(nil), requirement.Environment...),
				NetworkIntent: requirement.NetworkIntent,
				MCPIdentity:   requirement.MCPIdentity,
				Metadata:      requirement.Metadata,
				AlwaysConfirm: requirement.AlwaysConfirm,
			}, required, nil
		}
	}
	mode := agent.ExecSequential
	if metadata.Concurrency == iontool.Parallel {
		mode = agent.ExecParallel
	}
	return agent.Tool{
		Name:                spec.Name,
		Description:         spec.Description,
		Parameters:          spec.Parameters,
		ReadOnly:            metadata.ReadOnly,
		RequiresAction:      !metadata.ReadOnly,
		ApprovalRequirement: approval,
		ExecutionMode:       mode,
		Execute: func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
			toolCtx, cancel := dailyDriverToolContext(ctx, signal)
			defer cancel()
			var (
				content string
				details any
				err     error
			)
			if detailed, ok := registered.(iontool.ProgressAwareDetailedTool); ok {
				content, details, err = detailed.ExecuteDetailedWithProgress(
					toolCtx,
					string(args),
					func(update iontool.StreamUpdate) {
						if progress != nil && update.Text != "" {
							progress(update.Text)
						}
					},
				)
			} else if detailed, ok := registered.(iontool.DetailedTool); ok {
				content, details, err = detailed.ExecuteDetailed(toolCtx, string(args))
			} else {
				content, err = registered.Execute(toolCtx, string(args))
			}
			result := session.ToolResultMessage{ToolCallID: id, ToolName: spec.Name, Timestamp: time.Now()}
			if content != "" {
				result.Content = append(result.Content, session.TextContent{Text: content})
			}
			if details != nil {
				result.Details, _ = json.Marshal(details)
			}
			if err != nil {
				result.IsError = true
				result.Content = append(result.Content, session.TextContent{Text: err.Error()})
			}
			return result, nil
		},
	}
}

func dailyDriverToolContext(ctx context.Context, signal <-chan struct{}) (context.Context, context.CancelFunc) {
	toolCtx, cancel := context.WithCancel(ctx)
	if signal == nil {
		return toolCtx, cancel
	}
	go func() {
		select {
		case <-signal:
			cancel()
		case <-toolCtx.Done():
		}
	}()
	return toolCtx, cancel
}

func assertDailyDriverToolsPersisted(t *testing.T, sess session.Session) {
	t.Helper()
	entries, err := sess.Entries(t.Context())
	if err != nil {
		t.Fatalf("load daily-driver entries: %v", err)
	}
	want := map[string]bool{
		"web_search": false,
		"web_fetch":  false,
		"subagent":   false,
		"edit":       false,
		"bash":       false,
	}
	for _, entry := range entries {
		messageEntry, ok := entry.(*session.MessageEntry)
		if !ok {
			continue
		}
		if result, ok := messageEntry.Message.(*session.ToolResultMessage); ok {
			if _, exists := want[result.ToolName]; exists && !result.IsError {
				want[result.ToolName] = true
			}
		}
	}
	for name, found := range want {
		if !found {
			t.Fatalf("daily-driver tool %q has no successful durable result: %#v", name, want)
		}
	}
}

type dailyDriverProvider struct {
	mu              sync.Mutex
	parentSessionID string
	filePath        string
	fetchURL        string
	parentCalls     int
	childCalls      int
	requests        []llm.Request
	parentRequests  []llm.Request
}

func (p *dailyDriverProvider) stream(_ context.Context, req *llm.Request) (llm.Stream, error) {
	p.mu.Lock()
	snapshot := *req
	snapshot.Messages = append([]llm.Message(nil), req.Messages...)
	p.requests = append(p.requests, snapshot)
	if req.SessionID != p.parentSessionID {
		p.childCalls++
		p.mu.Unlock()
		return &acceptanceStream{chunks: []*llm.Chunk{{Content: "child research evidence", StopReason: "stop"}}}, nil
	}
	p.parentCalls++
	call := p.parentCalls
	p.parentRequests = append(p.parentRequests, snapshot)
	p.mu.Unlock()

	switch call {
	case 1:
		return dailyDriverToolCall("web-search", "web_search", `{"query":"fixture research"}`), nil
	case 2:
		p.requireRequestText(call, "fixture search evidence")
		return dailyDriverToolCall("web-fetch", "web_fetch", fmt.Sprintf(`{"url":%q}`, p.fetchURL)), nil
	case 3:
		p.requireRequestText(call, "fixture article evidence")
		return dailyDriverToolCall(
			"subagent-call",
			iontool.SubagentToolName,
			`{"task":"summarize the fixture research"}`,
		), nil
	case 4:
		p.requireRequestText(call, "child research evidence")
		return dailyDriverToolCall(
			"edit-call",
			"edit",
			fmt.Sprintf(
				`{"path":%q,"edits":[{"old_string":"original","new_string":"edited"}]}`,
				filepath.Base(p.filePath),
			),
		), nil
	case 5:
		p.requireRequestText(call, "Applied 1 edit")
		return dailyDriverToolCall("bash-call", "bash", `{"command":"grep -q edited daily.txt","timeout":5}`), nil
	case 6:
		return &acceptanceStream{chunks: []*llm.Chunk{{Content: "daily-driver-reviewed", StopReason: "stop"}}}, nil
	default:
		return nil, fmt.Errorf("unexpected daily-driver parent call %d", call)
	}
}

func (p *dailyDriverProvider) requireRequestText(call int, needle string) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if call > len(p.parentRequests) || !requestMessagesContain(p.parentRequests[call-1].Messages, needle) {
		var summary []string
		if call <= len(p.parentRequests) {
			for _, message := range p.parentRequests[call-1].Messages {
				summary = append(summary, fmt.Sprintf("%s:%q", message.Role, message.TextContent()))
			}
		}
		panic(
			fmt.Sprintf("daily-driver request %d omitted %q; messages: %s", call, needle, strings.Join(summary, " | ")),
		)
	}
}

func (p *dailyDriverProvider) assertParentJourney(t *testing.T) {
	t.Helper()
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.parentCalls != 6 || p.childCalls != 1 {
		t.Fatalf("daily-driver provider calls = parent %d child %d, want parent 6 child 1", p.parentCalls, p.childCalls)
	}
	if p.parentSessionID == "" {
		t.Fatal("daily-driver provider has no parent session identity")
	}
}

func requestMessagesContain(messages []llm.Message, needle string) bool {
	for _, message := range messages {
		if strings.Contains(message.TextContent(), needle) {
			return true
		}
	}
	return false
}

func dailyDriverToolCall(id, name, args string) *acceptanceStream {
	function := struct {
		Name      string `json:"name"`
		Arguments string `json:"arguments"`
	}{Name: name, Arguments: args}
	return &acceptanceStream{chunks: []*llm.Chunk{{
		Calls:      []llm.Call{{ID: id, Type: "function", Function: function}},
		StopReason: "toolUse",
	}}}
}
