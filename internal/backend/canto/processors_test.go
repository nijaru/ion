package canto

import (
	"context"
	"strings"
	"testing"

	"github.com/nijaru/canto/llm"
	csession "github.com/nijaru/canto/session"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func TestReasoningEffortProcessorSetsRequestField(t *testing.T) {
	req := &llm.Request{}
	processor := reasoningEffortProcessor(&config.Config{ReasoningEffort: "med"})
	provider := &reasoningCapProvider{reasoningEffort: true}
	if err := processor.ApplyRequest(context.Background(), provider, "o3-mini", nil, req); err != nil {
		t.Fatalf("process: %v", err)
	}
	if req.ReasoningEffort != "medium" {
		t.Fatalf("reasoning effort = %q, want %q", req.ReasoningEffort, "medium")
	}
}

func TestReasoningEffortProcessorRespectsCapabilities(t *testing.T) {
	req := &llm.Request{}
	processor := reasoningEffortProcessor(&config.Config{ReasoningEffort: "high"})
	provider := &reasoningCapProvider{}
	if err := processor.ApplyRequest(context.Background(), provider, "local-model", nil, req); err != nil {
		t.Fatalf("process: %v", err)
	}
	if req.ReasoningEffort != "" {
		t.Fatalf("reasoning effort = %q, want empty for unsupported provider", req.ReasoningEffort)
	}
}

func TestReasoningEffortProcessorMapsOffToNone(t *testing.T) {
	req := &llm.Request{}
	processor := reasoningEffortProcessor(&config.Config{ReasoningEffort: "off"})
	provider := &reasoningCapProvider{reasoningEffort: true}
	if err := processor.ApplyRequest(context.Background(), provider, "gpt-5.2", nil, req); err != nil {
		t.Fatalf("process: %v", err)
	}
	if req.ReasoningEffort != "none" {
		t.Fatalf("reasoning effort = %q, want none", req.ReasoningEffort)
	}
}

func TestReasoningEffortProcessorDropsUnsupportedEffortValue(t *testing.T) {
	req := &llm.Request{}
	processor := reasoningEffortProcessor(&config.Config{ReasoningEffort: "xhigh"})
	provider := &reasoningCapProvider{reasoningEffort: true}
	if err := processor.ApplyRequest(context.Background(), provider, "model", nil, req); err != nil {
		t.Fatalf("process: %v", err)
	}
	if req.ReasoningEffort != "" {
		t.Fatalf("reasoning effort = %q, want empty for unsupported effort", req.ReasoningEffort)
	}
}

func TestReasoningEffortProcessorDoesNotSendMaxYet(t *testing.T) {
	req := &llm.Request{}
	processor := reasoningEffortProcessor(&config.Config{ReasoningEffort: "max"})
	provider := &reasoningCapProvider{reasoningEffort: true}
	if err := processor.ApplyRequest(context.Background(), provider, "model", nil, req); err != nil {
		t.Fatalf("process: %v", err)
	}
	if req.ReasoningEffort != "" {
		t.Fatalf(
			"reasoning effort = %q, want empty until provider-specific max mapping exists",
			req.ReasoningEffort,
		)
	}
}

func TestToolVisibilityProcessorFiltersReadModeTools(t *testing.T) {
	policy := backend.NewPolicyEngine()
	policy.SetMode(ionsession.ModeRead)
	req := &llm.Request{
		Tools: []*llm.Spec{
			{Name: "bash"},
			{Name: "edit"},
			{Name: "glob"},
			{Name: "grep"},
			{Name: "list"},
			{Name: "read"},
			{Name: "write"},
		},
	}

	processor := toolVisibilityProcessor(policy)
	if err := processor.ApplyRequest(context.Background(), nil, "model", nil, req); err != nil {
		t.Fatalf("process: %v", err)
	}

	got := specNames(req.Tools)
	want := []string{"glob", "grep", "list", "read"}
	if strings.Join(got, ",") != strings.Join(want, ",") {
		t.Fatalf("READ request tools = %#v, want %#v", got, want)
	}
}

func TestReflexionProcessorAddsNoteAfterToolError(t *testing.T) {
	sess := csession.New("reflexion")
	if err := sess.Append(context.Background(), csession.NewEvent("reflexion", csession.ToolCompleted, map[string]string{
		"tool":  "bash",
		"id":    "toolu_123",
		"error": "exit status 1",
	})); err != nil {
		t.Fatalf("append tool error: %v", err)
	}

	req := &llm.Request{
		Messages: []llm.Message{{
			Role:    llm.RoleUser,
			ToolID:  "toolu_123",
			Content: "failed output",
		}},
	}
	processor := reflexionProcessor()
	if err := processor.ApplyRequest(context.Background(), nil, "model-a", sess, req); err != nil {
		t.Fatalf("process: %v", err)
	}
	if !strings.Contains(req.Messages[0].Content, "tool execution failed") {
		t.Fatalf("reflexion note not appended: %q", req.Messages[0].Content)
	}
}

func TestLocalAPIRequestsKeepSystemMessagesLeading(t *testing.T) {
	ctx := context.Background()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(ctx, "/tmp/ion-local-api", "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := llm.NewFauxProvider("local-api", llm.FauxStep{Content: "ok"})
	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		return provider, nil
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(
		&config.Config{
			Provider: "local-api",
			Model:    "model-a",
			Endpoint: "http://localhost:8080/v1",
		},
	)
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	if err := b.SubmitTurn(ctx, "hi"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	calls := provider.Calls()
	if len(calls) != 1 {
		t.Fatalf("provider calls = %d, want 1", len(calls))
	}
	gotTools := specNames(calls[0].Tools)
	wantTools := []string{
		"bash",
		"edit",
		"glob",
		"grep",
		"list",
		"multi_edit",
		"read",
		"write",
	}
	if strings.Join(gotTools, ",") != strings.Join(wantTools, ",") {
		t.Fatalf("default provider tools = %#v, want %#v", gotTools, wantTools)
	}
	roles := make([]llm.Role, 0, len(calls[0].Messages))
	for _, msg := range calls[0].Messages {
		roles = append(roles, msg.Role)
	}
	firstNonSystem := len(roles)
	for i, role := range roles {
		if role != llm.RoleSystem {
			firstNonSystem = i
			break
		}
	}
	for _, role := range roles[firstNonSystem:] {
		if role == llm.RoleSystem {
			t.Fatalf("local-api request has non-leading system messages: %#v", roles)
		}
	}
}

func TestReadModeProviderRequestHidesUnavailableTools(t *testing.T) {
	ctx := context.Background()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(ctx, t.TempDir(), "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := llm.NewFauxProvider("local-api", llm.FauxStep{Content: "ok"})
	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		return provider, nil
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(
		&config.Config{
			Provider: "local-api",
			Model:    "model-a",
			Endpoint: "http://localhost:8080/v1",
		},
	)
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()
	b.SetMode(ionsession.ModeRead)

	if err := b.SubmitTurn(ctx, "read only please"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	calls := provider.Calls()
	if len(calls) != 1 {
		t.Fatalf("provider calls = %d, want 1", len(calls))
	}
	got := specNames(calls[0].Tools)
	want := []string{"glob", "grep", "list", "read"}
	if strings.Join(got, ",") != strings.Join(want, ",") {
		t.Fatalf("READ provider tools = %#v, want %#v", got, want)
	}
}
