package canto

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/nijaru/canto/llm"
	csession "github.com/nijaru/canto/session"
	"github.com/nijaru/canto/tool"
	ctesting "github.com/nijaru/canto/x/testing"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
	"github.com/nijaru/ion/internal/subagents"
)

type compactProvider struct {
	id          string
	lastRequest *llm.Request
}

func (p *compactProvider) ID() string { return p.id }

func (p *compactProvider) Generate(ctx context.Context, req *llm.Request) (*llm.Response, error) {
	p.lastRequest = req
	return &llm.Response{Content: "condensed summary"}, nil
}

func (p *compactProvider) Stream(ctx context.Context, req *llm.Request) (llm.Stream, error) {
	return nil, nil
}

func (p *compactProvider) Models(ctx context.Context) ([]llm.Model, error) {
	return nil, nil
}

func (p *compactProvider) CountTokens(
	ctx context.Context,
	model string,
	messages []llm.Message,
) (int, error) {
	return 10_000, nil
}

func (p *compactProvider) Cost(
	ctx context.Context,
	model string,
	usage llm.Usage,
) float64 {
	return 0
}

func (p *compactProvider) Capabilities(model string) llm.Capabilities {
	return llm.DefaultCapabilities()
}

func (p *compactProvider) IsTransient(err error) bool { return false }

func (p *compactProvider) IsContextOverflow(err error) bool { return false }

type reasoningCapProvider struct {
	compactProvider
	reasoningEffort bool
}

func (p *reasoningCapProvider) Capabilities(model string) llm.Capabilities {
	caps := llm.DefaultCapabilities()
	caps.ReasoningEffort = p.reasoningEffort
	if p.reasoningEffort {
		caps.Reasoning = llm.ReasoningCapabilities{
			Kind:       llm.ReasoningKindEffort,
			Efforts:    []string{"minimal", "low", "medium", "high"},
			CanDisable: true,
		}
	}
	return caps
}

var (
	transientStreamErr = errors.New("transient provider failure")
	overflowErr        = errors.New("context_length_exceeded")
)

type retryProvider struct {
	*ctesting.FauxProvider
}

type proactiveUsageSession struct {
	id       string
	meta     storage.Metadata
	usageIn  int
	usageOut int
}

func (p *retryProvider) IsTransient(err error) bool {
	return errors.Is(err, transientStreamErr)
}

func (p *retryProvider) IsContextOverflow(err error) bool { return false }

type overflowRecoveryProvider struct {
	*ctesting.FauxProvider
}

type blockingStreamProvider struct {
	compactProvider
	streamCtx chan context.Context
}

type contextBlockingStream struct {
	ctx context.Context
}

func (p *blockingStreamProvider) Stream(ctx context.Context, req *llm.Request) (llm.Stream, error) {
	p.streamCtx <- ctx
	return &contextBlockingStream{ctx: ctx}, nil
}

func (s *contextBlockingStream) Next() (*llm.Chunk, bool) {
	<-s.ctx.Done()
	return nil, false
}

func (s *contextBlockingStream) Err() error {
	return s.ctx.Err()
}

func (s *contextBlockingStream) Close() error { return nil }

type lateSuccessStreamProvider struct {
	compactProvider
	streamCtx chan context.Context
}

type lateSuccessStream struct {
	ctx  context.Context
	sent bool
}

func (p *lateSuccessStreamProvider) Stream(
	ctx context.Context,
	req *llm.Request,
) (llm.Stream, error) {
	p.streamCtx <- ctx
	return &lateSuccessStream{ctx: ctx}, nil
}

func (s *lateSuccessStream) Next() (*llm.Chunk, bool) {
	if s.sent {
		return nil, false
	}
	<-s.ctx.Done()
	s.sent = true
	return &llm.Chunk{Content: "late answer"}, true
}

func (s *lateSuccessStream) Err() error   { return nil }
func (s *lateSuccessStream) Close() error { return nil }

type testTool struct {
	name string
}

func (t *testTool) Spec() llm.Spec {
	return llm.Spec{Name: t.name}
}

func (t *testTool) Execute(ctx context.Context, args string) (string, error) {
	return "", nil
}

func (p *overflowRecoveryProvider) CountTokens(
	ctx context.Context,
	model string,
	messages []llm.Message,
) (int, error) {
	return 10_000, nil
}

func (p *overflowRecoveryProvider) IsContextOverflow(err error) bool {
	return errors.Is(err, overflowErr)
}

func (s *proactiveUsageSession) ID() string                                  { return s.id }
func (s *proactiveUsageSession) Meta() storage.Metadata                      { return s.meta }
func (s *proactiveUsageSession) Append(ctx context.Context, event any) error { return nil }
func (s *proactiveUsageSession) Entries(ctx context.Context) ([]ionsession.Entry, error) {
	return nil, nil
}
func (s *proactiveUsageSession) LastStatus(ctx context.Context) (string, error) { return "", nil }
func (s *proactiveUsageSession) Usage(ctx context.Context) (int, int, float64, error) {
	return s.usageIn, s.usageOut, 0, nil
}
func (s *proactiveUsageSession) Close() error { return nil }

func TestProviderAndModelLoadFromEnv(t *testing.T) {
	t.Setenv("ION_PROVIDER", "anthropic")
	t.Setenv("ION_MODEL", "claude-sonnet-4-5")

	b := New()

	if got := b.Provider(); got != "anthropic" {
		t.Fatalf("Provider() = %q, want %q", got, "anthropic")
	}
	if got := b.Model(); got != "claude-sonnet-4-5" {
		t.Fatalf("Model() = %q, want %q", got, "claude-sonnet-4-5")
	}
}

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

func TestSubmitTurnPreservesProviderInSessionMetadata(t *testing.T) {
	ctx := context.Background()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	cwd := "/tmp/ion-local-api"
	storageSession, err := store.OpenSession(ctx, cwd, "local-api/model-a", "main")
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

	sessions, err := store.ListSessions(ctx, cwd)
	if err != nil {
		t.Fatalf("list sessions: %v", err)
	}
	if len(sessions) != 1 {
		t.Fatalf("sessions = %d, want 1", len(sessions))
	}
	if sessions[0].Model != "local-api/model-a" {
		t.Fatalf("session model = %q, want provider-qualified model", sessions[0].Model)
	}
}

func TestToolSurfaceFiltersReadModeTools(t *testing.T) {
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
	surface := b.ToolSurface()
	want := []string{"glob", "grep", "list", "read"}
	if surface.Count != len(want) {
		t.Fatalf("READ tool count = %d, want %d", surface.Count, len(want))
	}
	if strings.Join(surface.Names, ",") != strings.Join(want, ",") {
		t.Fatalf("READ tool surface = %#v, want %#v", surface.Names, want)
	}

	b.SetMode(ionsession.ModeEdit)
	surface = b.ToolSurface()
	want = []string{"bash", "edit", "glob", "grep", "list", "multi_edit", "read", "write"}
	if surface.Count != len(want) {
		t.Fatalf("EDIT tool count = %d, want %d", surface.Count, len(want))
	}
	if strings.Join(surface.Names, ",") != strings.Join(want, ",") {
		t.Fatalf("EDIT tool surface = %#v, want %#v", surface.Names, want)
	}
}

func TestSkillToolSurfaceIsOptIn(t *testing.T) {
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
	b.SetConfig(&config.Config{
		Provider:   "local-api",
		Model:      "model-a",
		Endpoint:   "http://localhost:8080/v1",
		SkillTools: "read",
	})
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	surface := b.ToolSurface()
	if !slices.Contains(surface.Names, "read_skill") {
		t.Fatalf("tool surface = %#v, want read_skill", surface.Names)
	}
	if surface.Count != 9 {
		t.Fatalf("EDIT tool count = %d, want 9", surface.Count)
	}

	b.SetMode(ionsession.ModeRead)
	surface = b.ToolSurface()
	if !slices.Contains(surface.Names, "read_skill") {
		t.Fatalf("READ tool surface = %#v, want read_skill", surface.Names)
	}
}

func TestSubagentToolSurfaceIsOptIn(t *testing.T) {
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
	b.SetConfig(&config.Config{
		Provider: "local-api",
		Model:    "model-a",
		Endpoint: "http://localhost:8080/v1",
	})
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	if surface := b.ToolSurface(); slices.Contains(surface.Names, "subagent") {
		t.Fatalf("default tool surface = %#v, want no subagent", surface.Names)
	}
	if err := b.Close(); err != nil {
		t.Fatalf("close default backend: %v", err)
	}

	withSubagentSession, err := store.OpenSession(
		ctx,
		t.TempDir(),
		"local-api/model-a",
		"main",
	)
	if err != nil {
		t.Fatalf("open second session: %v", err)
	}
	withSubagent := New()
	withSubagent.SetStore(store)
	withSubagent.SetSession(withSubagentSession)
	withSubagent.SetConfig(&config.Config{
		Provider:      "local-api",
		Model:         "model-a",
		Endpoint:      "http://localhost:8080/v1",
		SubagentTools: "on",
	})
	if err := withSubagent.Open(ctx); err != nil {
		t.Fatalf("open subagent backend: %v", err)
	}
	defer func() { _ = withSubagent.Close() }()

	surface := withSubagent.ToolSurface()
	if !slices.Contains(surface.Names, "subagent") {
		t.Fatalf("tool surface = %#v, want subagent", surface.Names)
	}
	withSubagent.SetMode(ionsession.ModeRead)
	readSurface := withSubagent.ToolSurface()
	if slices.Contains(readSurface.Names, "subagent") {
		t.Fatalf("READ tool surface = %#v, want subagent hidden", readSurface.Names)
	}
}

func TestSubagentToolExecutesWhenOptedIn(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	call := llm.Call{ID: "subagent-call-1", Type: "function"}
	call.Function.Name = "subagent"
	call.Function.Arguments = `{"agent":"explorer","task":"inspect README","context_mode":"none"}`
	provider := ctesting.NewMockProvider("local-api",
		ctesting.Step{Calls: []llm.Call{call}},
		ctesting.Step{Content: "child summary"},
		ctesting.Step{Content: "done"},
	)

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		return provider, nil
	}
	defer func() { providerFactory = oldFactory }()

	cwd := t.TempDir()
	storageSession, err := store.OpenSession(ctx, cwd, "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(&config.Config{
		Provider:      "local-api",
		Model:         "model-a",
		Endpoint:      "http://localhost:8080/v1",
		SubagentTools: "on",
	})
	b.SetMode(ionsession.ModeYolo)
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	if err := b.SubmitTurn(ctx, "delegate a read-only inspection"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	calls := provider.Calls()
	if len(calls) != 3 {
		t.Fatalf("provider calls = %d, want 3", len(calls))
	}
	if !requestHasMessage(calls[1].Messages, llm.RoleUser, "Task: inspect README") {
		t.Fatalf("child request missing task: %#v", calls[1].Messages)
	}
	if requestHasMessage(calls[1].Messages, llm.RoleUser, "delegate a read-only inspection") {
		t.Fatalf("none-mode child request inherited parent prompt: %#v", calls[1].Messages)
	}
	if !requestHasMessage(calls[2].Messages, llm.RoleTool, "child summary") {
		t.Fatalf("parent continuation missing child tool result: %#v", calls[2].Messages)
	}

	cantoStore, ok := store.(interface{ Canto() *csession.SQLiteStore })
	if !ok {
		t.Fatal("store does not expose canto store")
	}
	parent, err := cantoStore.Canto().Load(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("load parent session: %v", err)
	}
	messages, err := parent.EffectiveMessages()
	if err != nil {
		t.Fatalf("parent effective messages: %v", err)
	}
	if !requestHasMessage(messages, llm.RoleTool, "child summary") {
		t.Fatalf("parent history missing child tool result: %#v", messages)
	}
}

func TestSubmitTurnMaterializesLazySession(t *testing.T) {
	ctx := context.Background()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	cwd := "/tmp/ion-lazy-turn"
	storageSession := storage.NewLazySession(store, cwd, "local-api/model-a", "main")

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

	if storage.IsMaterialized(storageSession) {
		t.Fatal("lazy session materialized during backend open")
	}
	before, err := store.ListSessions(ctx, cwd)
	if err != nil {
		t.Fatalf("list before submit: %v", err)
	}
	if len(before) != 0 {
		t.Fatalf("sessions before submit = %#v, want none", before)
	}

	if err := b.SubmitTurn(ctx, "hi"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	if !storage.IsMaterialized(storageSession) {
		t.Fatal("lazy session not materialized by submit")
	}
	after, err := store.ListSessions(ctx, cwd)
	if err != nil {
		t.Fatalf("list after submit: %v", err)
	}
	if len(after) != 1 {
		t.Fatalf("sessions after submit = %d, want 1", len(after))
	}
	if after[0].LastPreview != "hi" {
		t.Fatalf("last preview = %q, want hi", after[0].LastPreview)
	}
}

func TestSubmitTurnUsesCallerContext(t *testing.T) {
	ctx := t.Context()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(ctx, "/tmp/ion-context", "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := &blockingStreamProvider{
		compactProvider: compactProvider{id: "local-api"},
		streamCtx:       make(chan context.Context, 1),
	}
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

	turnCtx, cancel := context.WithCancel(ctx)
	if err := b.SubmitTurn(turnCtx, "hi"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}

	var streamCtx context.Context
	select {
	case streamCtx = <-provider.streamCtx:
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for provider stream")
	}

	cancel()
	select {
	case <-streamCtx.Done():
	case <-time.After(2 * time.Second):
		t.Fatal("provider stream context was not canceled")
	}
	waitForTurnFinished(t, b.Events())
}

func TestSubmitTurnCancelSuppressesLateAssistant(t *testing.T) {
	ctx := t.Context()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(
		ctx,
		"/tmp/ion-late-cancel",
		"local-api/model-a",
		"main",
	)
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := &lateSuccessStreamProvider{
		compactProvider: compactProvider{id: "local-api"},
		streamCtx:       make(chan context.Context, 1),
	}
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
	select {
	case <-provider.streamCtx:
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for provider stream")
	}
	if err := b.CancelTurn(ctx); err != nil {
		t.Fatalf("cancel turn: %v", err)
	}

	for {
		select {
		case ev := <-b.Events():
			switch msg := ev.(type) {
			case ionsession.AgentMessage:
				t.Fatalf("late assistant reached Ion after cancel: %#v", msg)
			case ionsession.TurnFinished:
				entries, err := storageSession.Entries(ctx)
				if err != nil {
					t.Fatalf("load entries: %v", err)
				}
				for _, entry := range entries {
					if entry.Role == ionsession.Agent {
						t.Fatalf("late assistant persisted after cancel: %#v", entry)
					}
				}
				return
			}
		case <-time.After(2 * time.Second):
			t.Fatal("timed out waiting for canceled turn")
		}
	}
}

func TestSubmitTurnRejectsConcurrentTurn(t *testing.T) {
	ctx := t.Context()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(
		ctx,
		"/tmp/ion-concurrent",
		"local-api/model-a",
		"main",
	)
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := &blockingStreamProvider{
		compactProvider: compactProvider{id: "local-api"},
		streamCtx:       make(chan context.Context, 1),
	}
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

	turnCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	if err := b.SubmitTurn(turnCtx, "first"); err != nil {
		t.Fatalf("submit first turn: %v", err)
	}

	select {
	case <-provider.streamCtx:
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for provider stream")
	}

	err = b.SubmitTurn(ctx, "second")
	if err == nil || !strings.Contains(err.Error(), "turn already in progress") {
		t.Fatalf("second SubmitTurn error = %v, want turn already in progress", err)
	}

	cancel()
	waitForTurnFinished(t, b.Events())
}

func TestResumeDoesNotDeadlockWhenBackendNeedsOpen(t *testing.T) {
	b := New()

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	done := make(chan error, 1)
	go func() {
		done <- b.Resume(ctx, "session-id")
	}()

	select {
	case err := <-done:
		if err == nil {
			t.Fatal("expected resume to fail without provider/model")
		}
	case <-time.After(500 * time.Millisecond):
		t.Fatal("resume appears to deadlock")
	}
}

func TestCloseIsIdempotent(t *testing.T) {
	b := New()

	if err := b.Close(); err != nil {
		t.Fatalf("first close: %v", err)
	}
	if err := b.Close(); err != nil {
		t.Fatalf("second close: %v", err)
	}
}

func TestTranslateEventsCommitsAssistantFromMessageAdded(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 2)
	events <- csession.NewEvent("session-id", csession.MessageAdded, llm.Message{
		Role:      llm.RoleAssistant,
		Content:   "done",
		Reasoning: "brief reasoning",
	})
	events <- csession.NewTurnCompletedEvent("session-id", csession.TurnCompletedData{})
	close(events)

	b.translateEvents(t.Context(), events, 0)

	ev1 := receiveEvent(t, b.Events())
	committed, ok := ev1.(ionsession.AgentMessage)
	if !ok {
		t.Fatalf("first event = %T, want AgentMessage", ev1)
	}
	if committed.Message != "done" || committed.Reasoning != "brief reasoning" {
		t.Fatalf("committed message = %#v", committed)
	}

	ev2 := receiveEvent(t, b.Events())
	if _, ok := ev2.(ionsession.TurnFinished); !ok {
		t.Fatalf("second event = %T, want TurnFinished", ev2)
	}

	ev3 := receiveEvent(t, b.Events())
	status, ok := ev3.(ionsession.StatusChanged)
	if !ok {
		t.Fatalf("third event = %T, want StatusChanged", ev3)
	}
	if status.Status != "Ready" {
		t.Fatalf("status = %q, want Ready", status.Status)
	}
}

func TestTranslateEventsTurnCompletedDoesNotEmitEmptyAssistant(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 1)
	events <- csession.NewTurnCompletedEvent("session-id", csession.TurnCompletedData{})
	close(events)

	b.translateEvents(t.Context(), events, 0)

	ev1 := receiveEvent(t, b.Events())
	if _, ok := ev1.(ionsession.TurnFinished); !ok {
		t.Fatalf("first event = %T, want TurnFinished", ev1)
	}

	ev2 := receiveEvent(t, b.Events())
	status, ok := ev2.(ionsession.StatusChanged)
	if !ok {
		t.Fatalf("second event = %T, want StatusChanged", ev2)
	}
	if status.Status != "Ready" {
		t.Fatalf("status = %q, want Ready", status.Status)
	}
}

func TestTranslateEventsClearsActiveTurnBeforeFinishedEvent(t *testing.T) {
	b := New()
	b.turnSeq = 7
	b.turnActive = true
	b.cancel = func() {}

	events := make(chan csession.Event, 1)
	events <- csession.NewTurnCompletedEvent("session-id", csession.TurnCompletedData{})
	close(events)

	b.translateEvents(t.Context(), events, 7)

	if b.turnActive {
		t.Fatal("turnActive remained true after terminal event translation")
	}
	if b.cancel != nil {
		t.Fatal("cancel func remained set after terminal event translation")
	}
	ev := receiveEvent(t, b.Events())
	if _, ok := ev.(ionsession.TurnFinished); !ok {
		t.Fatalf("event = %T, want TurnFinished", ev)
	}
}

func TestTranslateEventsSuppressesCanceledTerminalError(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 1)
	events <- csession.NewTurnCompletedEvent("session-id", csession.TurnCompletedData{
		Error: context.Canceled.Error(),
	})
	close(events)

	b.translateEvents(t.Context(), events, 0)

	ev1 := receiveEvent(t, b.Events())
	if _, ok := ev1.(ionsession.TurnFinished); !ok {
		t.Fatalf("first event = %T, want TurnFinished", ev1)
	}

	ev2 := receiveEvent(t, b.Events())
	status, ok := ev2.(ionsession.StatusChanged)
	if !ok {
		t.Fatalf("second event = %T, want StatusChanged", ev2)
	}
	if status.Status != "Ready" {
		t.Fatalf("status = %q, want Ready", status.Status)
	}
}

func TestTranslateEventsPreservesToolUseID(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 2)
	events <- csession.NewToolStartedEvent("session-id", csession.ToolStartedData{
		ID:        "tool-call-1",
		Tool:      "bash",
		Arguments: "git status",
	})
	events <- csession.NewToolCompletedEvent("session-id", csession.ToolCompletedData{
		ID:     "tool-call-1",
		Tool:   "bash",
		Output: "ok",
	})
	close(events)

	b.translateEvents(t.Context(), events, 0)

	ev1 := receiveEvent(t, b.Events())
	started, ok := ev1.(ionsession.ToolCallStarted)
	if !ok {
		t.Fatalf("first event = %T, want ToolCallStarted", ev1)
	}
	if started.ToolUseID != "tool-call-1" {
		t.Fatalf("started id = %q, want tool-call-1", started.ToolUseID)
	}
	_ = receiveEvent(t, b.Events()) // status

	ev3 := receiveEvent(t, b.Events())
	result, ok := ev3.(ionsession.ToolResult)
	if !ok {
		t.Fatalf("third event = %T, want ToolResult", ev3)
	}
	if result.ToolUseID != "tool-call-1" {
		t.Fatalf("result id = %q, want tool-call-1", result.ToolUseID)
	}
}

func TestTranslateEventsPreservesToolOutputDeltaID(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 1)
	events <- csession.NewEvent("session-id", csession.ToolOutputDelta, map[string]string{
		"id":    "tool-call-1",
		"tool":  "bash",
		"delta": "partial output",
	})
	close(events)

	b.translateEvents(t.Context(), events, 0)

	ev := receiveEvent(t, b.Events())
	delta, ok := ev.(ionsession.ToolOutputDelta)
	if !ok {
		t.Fatalf("event = %T, want ToolOutputDelta", ev)
	}
	if delta.ToolUseID != "tool-call-1" {
		t.Fatalf("delta id = %q, want tool-call-1", delta.ToolUseID)
	}
	if delta.Delta != "partial output" {
		t.Fatalf("delta = %q, want partial output", delta.Delta)
	}
}

func TestTranslateEventsPreservesToolCompletedError(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 1)
	events <- csession.NewToolCompletedEvent("session-id", csession.ToolCompletedData{
		ID:     "tool-call-1",
		Tool:   "bash",
		Output: "partial output\nError: exit status 1",
		Error:  "exit status 1",
	})
	close(events)

	b.translateEvents(t.Context(), events, 0)

	ev := receiveEvent(t, b.Events())
	result, ok := ev.(ionsession.ToolResult)
	if !ok {
		t.Fatalf("event = %T, want ToolResult", ev)
	}
	if result.Error == nil || result.Error.Error() != "exit status 1" {
		t.Fatalf("tool result error = %v, want exit status 1", result.Error)
	}
	if result.Result != "partial output\nError: exit status 1" {
		t.Fatalf("tool result output = %q", result.Result)
	}
}

func TestTranslateEventsUsesChildIDForSubagentRows(t *testing.T) {
	b := New()
	events := make(chan csession.Event, 2)
	events <- csession.NewChildRequestedEvent("session-id", csession.ChildRequestedData{
		ChildID:        "explorer-123",
		ChildSessionID: "child-session",
		Task:           "inspect policy flow",
		AgentID:        "explorer",
		Mode:           csession.ChildModeHandoff,
	})
	events <- csession.NewChildStartedEvent("session-id", csession.ChildStartedData{
		ChildID:        "explorer-123",
		ChildSessionID: "child-session",
		AgentID:        "explorer",
	})
	close(events)

	b.translateEvents(t.Context(), events, 0)

	requested, ok := receiveEvent(t, b.Events()).(ionsession.ChildRequested)
	if !ok {
		t.Fatal("first event is not ChildRequested")
	}
	if requested.AgentName != "explorer-123" {
		t.Fatalf("requested agent name = %q, want child id", requested.AgentName)
	}
	_ = receiveEvent(t, b.Events()) // request status

	started, ok := receiveEvent(t, b.Events()).(ionsession.ChildStarted)
	if !ok {
		t.Fatal("third event is not ChildStarted")
	}
	if started.AgentName != "explorer-123" {
		t.Fatalf("started agent name = %q, want child id", started.AgentName)
	}
}

func TestLoadSubagentPersonasMergesCustomAgents(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "explorer.md"), []byte(`---
name: explorer
description: Custom explorer.
model: primary
tools: [read]
---
Custom prompt.
`), 0o600); err != nil {
		t.Fatalf("write persona: %v", err)
	}

	personas, err := loadSubagentPersonas(&config.Config{SubagentsPath: dir})
	if err != nil {
		t.Fatalf("loadSubagentPersonas returned error: %v", err)
	}
	if len(personas) != 3 {
		t.Fatalf("persona count = %d, want 3", len(personas))
	}
	found := false
	for _, persona := range personas {
		if persona.Name == "explorer" {
			found = true
			if persona.Description != "Custom explorer." {
				t.Fatalf("explorer description = %q, want custom", persona.Description)
			}
		}
	}
	if !found {
		t.Fatal("explorer persona not found")
	}
}

func TestValidateSubagentPersonaToolsFailsClosed(t *testing.T) {
	registry := tool.NewRegistry()
	registry.Register(&testTool{name: "read"})

	err := validateSubagentPersonaTools([]subagents.Persona{{
		Name:        "bad",
		Description: "bad",
		ModelSlot:   subagents.ModelSlotFast,
		Tools:       []string{"read", "missing"},
		Prompt:      "bad prompt",
	}}, registry)
	if err == nil {
		t.Fatal("validateSubagentPersonaTools returned nil error")
	}
}

func TestRegisterMCPServerIsDeferred(t *testing.T) {
	b := New()

	err := b.RegisterMCPServer(t.Context(), "server")
	if err == nil || !strings.Contains(err.Error(), "MCP registration is deferred") {
		t.Fatalf("RegisterMCPServer error = %v, want deferred error", err)
	}
}

func TestCrossProviderHandoffPreservesPromptTruth(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	firstProvider := ctesting.NewMockProvider("openai", ctesting.Step{
		Chunks: []llm.Chunk{{Content: "first reply"}},
	})
	secondProvider := ctesting.NewMockProvider("openrouter", ctesting.Step{
		Chunks: []llm.Chunk{{Content: "second reply"}},
	})

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		switch cfg.Provider {
		case "openai":
			return firstProvider, nil
		case "openrouter":
			return secondProvider, nil
		default:
			return oldFactory(ctx, cfg)
		}
	}
	defer func() {
		providerFactory = oldFactory
	}()

	storageSession, err := store.OpenSession(ctx, "/tmp/ion-handoff", "openai/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	first := New()
	first.SetStore(store)
	first.SetSession(storageSession)
	first.SetConfig(&config.Config{Provider: "openai", Model: "model-a"})
	if err := first.Open(ctx); err != nil {
		t.Fatalf("open first backend: %v", err)
	}
	defer func() { _ = first.Close() }()

	if err := first.SubmitTurn(ctx, "first question"); err != nil {
		t.Fatalf("submit first turn: %v", err)
	}
	waitForTurnFinished(t, first.Events())
	if err := first.Close(); err != nil {
		t.Fatalf("close first backend: %v", err)
	}

	resumedSession, err := store.ResumeSession(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("resume session: %v", err)
	}

	second := New()
	second.SetStore(store)
	second.SetSession(resumedSession)
	second.SetConfig(&config.Config{Provider: "openrouter", Model: "model-b"})
	if err := second.Resume(ctx, storageSession.ID()); err != nil {
		t.Fatalf("resume second backend: %v", err)
	}
	defer func() { _ = second.Close() }()

	if got := second.ID(); got != storageSession.ID() {
		t.Fatalf("second backend session ID = %q, want %q", got, storageSession.ID())
	}

	if err := second.SubmitTurn(ctx, "second question"); err != nil {
		t.Fatalf("submit second turn: %v", err)
	}
	waitForTurnFinished(t, second.Events())

	calls := secondProvider.Calls()
	if len(calls) != 1 {
		t.Fatalf("second provider calls = %d, want 1", len(calls))
	}

	req := calls[0]
	if !requestHasMessage(req.Messages, llm.RoleUser, "first question") {
		t.Fatal("second provider request missing first user turn")
	}
	if !requestHasMessage(req.Messages, llm.RoleAssistant, "first reply") {
		t.Fatal("second provider request missing first agent reply")
	}
	if !requestHasMessage(req.Messages, llm.RoleUser, "second question") {
		t.Fatal("second provider request missing second user turn")
	}

	resumed, err := store.ResumeSession(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("resume persisted session: %v", err)
	}
	entries, err := resumed.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if !entryExists(entries, ionsession.User, "first question") {
		t.Fatal("persisted entries missing first user turn")
	}
	if !entryExists(entries, ionsession.Agent, "first reply") {
		t.Fatal("persisted entries missing first agent turn")
	}
	if !entryExists(entries, ionsession.User, "second question") {
		t.Fatal("persisted entries missing second user turn")
	}
	if !entryExists(entries, ionsession.Agent, "second reply") {
		t.Fatal("persisted entries missing second agent turn")
	}
}

func TestResumedToolSessionSendsValidFollowUpHistory(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	call := llm.Call{ID: "tool-call-1", Type: "function"}
	call.Function.Name = "bash"
	call.Function.Arguments = `{"command":"echo ion-smoke"}`
	provider := ctesting.NewMockProvider("local-api",
		ctesting.Step{Calls: []llm.Call{call}},
		ctesting.Step{Content: "done"},
		ctesting.Step{Content: "continued"},
	)

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "local-api" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	cwd := t.TempDir()
	storageSession, err := store.OpenSession(ctx, cwd, "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	first := New()
	first.SetStore(store)
	first.SetSession(storageSession)
	first.SetConfig(
		&config.Config{
			Provider: "local-api",
			Model:    "model-a",
			Endpoint: "http://localhost:8080/v1",
		},
	)
	first.SetMode(ionsession.ModeYolo)
	if err := first.Open(ctx); err != nil {
		t.Fatalf("open first backend: %v", err)
	}
	defer func() { _ = first.Close() }()

	if err := first.SubmitTurn(ctx, "run the smoke command"); err != nil {
		t.Fatalf("submit first turn: %v", err)
	}
	waitForTurnFinished(t, first.Events())
	if err := first.Close(); err != nil {
		t.Fatalf("close first backend: %v", err)
	}

	resumedSession, err := store.ResumeSession(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("resume session: %v", err)
	}

	second := New()
	second.SetStore(store)
	second.SetSession(resumedSession)
	second.SetConfig(
		&config.Config{
			Provider: "local-api",
			Model:    "model-a",
			Endpoint: "http://localhost:8080/v1",
		},
	)
	second.SetMode(ionsession.ModeYolo)
	if err := second.Resume(ctx, storageSession.ID()); err != nil {
		t.Fatalf("resume backend: %v", err)
	}
	defer func() { _ = second.Close() }()

	if err := second.SubmitTurn(ctx, "reply continued if the earlier tool result said ion-smoke"); err != nil {
		t.Fatalf("submit follow-up turn: %v", err)
	}
	waitForTurnFinished(t, second.Events())

	calls := provider.Calls()
	if len(calls) != 3 {
		t.Fatalf("provider calls = %d, want 3", len(calls))
	}
	req := calls[2]
	if !requestHasMessage(req.Messages, llm.RoleUser, "run the smoke command") {
		t.Fatal("follow-up request missing first user turn")
	}
	if !requestHasMessage(req.Messages, llm.RoleAssistant, "done") {
		t.Fatal("follow-up request missing post-tool assistant reply")
	}
	if !requestHasMessage(req.Messages, llm.RoleUser, "reply continued") {
		t.Fatal("follow-up request missing new user turn")
	}

	var (
		toolCallIndex   = -1
		toolResultIndex = -1
	)
	for i, msg := range req.Messages {
		if msg.Role == llm.RoleAssistant && len(msg.Calls) == 1 &&
			msg.Calls[0].ID == "tool-call-1" &&
			msg.Calls[0].Function.Name == "bash" {
			toolCallIndex = i
		}
		if msg.Role == llm.RoleTool &&
			msg.ToolID == "tool-call-1" &&
			msg.Name == "bash" &&
			strings.Contains(msg.Content, "ion-smoke") {
			toolResultIndex = i
		}
		if msg.Role == llm.RoleAssistant &&
			strings.TrimSpace(msg.Content) == "" &&
			msg.Reasoning == "" &&
			len(msg.ThinkingBlocks) == 0 &&
			len(msg.Calls) == 0 {
			t.Fatalf("follow-up request contains empty assistant message: %#v", req.Messages)
		}
	}
	if toolCallIndex < 0 {
		t.Fatalf("follow-up request missing assistant tool call: %#v", req.Messages)
	}
	if toolResultIndex < 0 {
		t.Fatalf("follow-up request missing matching tool result: %#v", req.Messages)
	}
	if toolResultIndex < toolCallIndex {
		t.Fatalf(
			"tool result appears before tool call: call=%d result=%d messages=%#v",
			toolCallIndex,
			toolResultIndex,
			req.Messages,
		)
	}
}

func TestSubmitTurnToolFailurePersistsForFollowUp(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	call := llm.Call{ID: "tool-call-fail", Type: "function"}
	call.Function.Name = "bash"
	call.Function.Arguments = `{"command":"exit 7"}`
	provider := ctesting.NewMockProvider("local-api",
		ctesting.Step{Calls: []llm.Call{call}},
		ctesting.Step{Content: "handled tool failure"},
		ctesting.Step{Content: "continued"},
	)

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "local-api" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	cwd := t.TempDir()
	storageSession, err := store.OpenSession(ctx, cwd, "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

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
	b.SetMode(ionsession.ModeYolo)
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	if err := b.SubmitTurn(ctx, "run a failing command"); err != nil {
		t.Fatalf("submit first turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	if err := b.SubmitTurn(ctx, "can you continue after that failure?"); err != nil {
		t.Fatalf("submit follow-up turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	calls := provider.Calls()
	if len(calls) != 3 {
		t.Fatalf("provider calls = %d, want 3", len(calls))
	}
	postToolRequest := calls[1]
	if !requestHasMessage(postToolRequest.Messages, llm.RoleTool, "exit status 7") {
		t.Fatalf("post-tool request missing failed tool result: %#v", postToolRequest.Messages)
	}
	followUpRequest := calls[2]
	if !requestHasMessage(followUpRequest.Messages, llm.RoleAssistant, "handled tool failure") {
		t.Fatalf(
			"follow-up request missing post-tool assistant reply: %#v",
			followUpRequest.Messages,
		)
	}
	if !requestHasMessage(followUpRequest.Messages, llm.RoleTool, "exit status 7") {
		t.Fatalf("follow-up request missing failed tool result: %#v", followUpRequest.Messages)
	}
}

func TestProviderHistoryExcludesIonDisplayOnlyEvents(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	cwd := t.TempDir()
	storageSession, err := store.OpenSession(ctx, cwd, "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	if err := storageSession.Append(ctx, storage.System{
		Type:    "system",
		Content: "UI-only resumed marker must not reach provider",
		TS:      time.Now().Unix(),
	}); err != nil {
		t.Fatalf("append display system: %v", err)
	}
	if err := storageSession.Append(ctx, storage.Status{
		Type:   "status",
		Status: "UI-only retry status must not reach provider",
		TS:     time.Now().Unix(),
	}); err != nil {
		t.Fatalf("append display status: %v", err)
	}
	appendCantoHistory(t, ctx, store, storageSession.ID(),
		llm.Message{Role: llm.RoleUser, Content: "prior user"},
		llm.Message{Role: llm.RoleAssistant, Content: "prior assistant"},
	)

	provider := ctesting.NewMockProvider("local-api", ctesting.Step{Content: "next"})
	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "local-api" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
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

	if err := b.SubmitTurn(ctx, "new user"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	calls := provider.Calls()
	if len(calls) != 1 {
		t.Fatalf("provider calls = %d, want 1", len(calls))
	}
	req := calls[0]
	for _, msg := range req.Messages {
		if strings.Contains(msg.Content, "UI-only") {
			t.Fatalf("provider request contains display-only event: %#v", req.Messages)
		}
	}
	if !requestHasMessage(req.Messages, llm.RoleUser, "prior user") {
		t.Fatalf("provider request missing prior user: %#v", req.Messages)
	}
	if !requestHasMessage(req.Messages, llm.RoleAssistant, "prior assistant") {
		t.Fatalf("provider request missing prior assistant: %#v", req.Messages)
	}
	if !requestHasMessage(req.Messages, llm.RoleUser, "new user") {
		t.Fatalf("provider request missing new user: %#v", req.Messages)
	}
}

func TestCompactUsesManualCompactionHelper(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	storageSession, err := store.OpenSession(ctx, "/tmp/ion-compact", "openai/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	appendCantoHistory(t, ctx, store, storageSession.ID(),
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("alpha ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("beta ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("gamma ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: "recent answer"},
		llm.Message{Role: llm.RoleUser, Content: "recent question"},
	)

	oldFactory := providerFactory
	provider := &compactProvider{id: "openai"}
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "openai" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(&config.Config{Provider: "openai", Model: "model-a", ContextLimit: 100})
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	compacted, err := b.Compact(ctx)
	if err != nil {
		t.Fatalf("compact: %v", err)
	}
	if !compacted {
		t.Fatal("expected compacted=true")
	}

	resumed, err := store.ResumeSession(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("resume compacted session: %v", err)
	}
	entries, err := resumed.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if !entryExists(entries, ionsession.System, "<conversation_summary>") {
		t.Fatalf(
			"expected compacted effective history to include conversation summary, got %#v",
			entries,
		)
	}
	if provider.lastRequest == nil || len(provider.lastRequest.Messages) < 2 ||
		!strings.Contains(
			provider.lastRequest.Messages[1].Content,
			"current user goal and immediate next step",
		) {
		t.Fatalf(
			"summarizer prompt did not include Ion compaction guidance: %#v",
			provider.lastRequest,
		)
	}

	cantoStore, ok := store.(interface{ Canto() *csession.SQLiteStore })
	if !ok {
		t.Fatal("expected canto-backed store")
	}
	sess, err := cantoStore.Canto().Load(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}
	var compactionEvents int
	for _, e := range sess.Events() {
		if e.Type == csession.CompactionTriggered {
			compactionEvents++
		}
	}
	if compactionEvents == 0 {
		t.Fatal("expected at least one durable compaction event")
	}
}

func TestOpenRetriesTransientProviderErrors(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	storageSession, err := store.OpenSession(ctx, "/tmp/ion-retry", "openai/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	provider := &retryProvider{
		FauxProvider: ctesting.NewMockProvider(
			"openai",
			ctesting.Step{Err: transientStreamErr},
			ctesting.Step{Content: "recovered reply"},
		),
	}

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "openai" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(&config.Config{Provider: "openai", Model: "model-a", ContextLimit: 100})
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	if err := b.SubmitTurn(ctx, "retry this request"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	calls := provider.Calls()
	if len(calls) != 2 {
		t.Fatalf("provider calls = %d, want 2 retries", len(calls))
	}
}

func TestSubmitTurnProviderErrorLeavesBackendReusable(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(
		ctx,
		"/tmp/ion-provider-error",
		"openai/model-a",
		"main",
	)
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	providerErr := errors.New("provider unavailable")
	provider := ctesting.NewMockProvider(
		"openai",
		ctesting.Step{Err: providerErr},
		ctesting.Step{Content: "recovered reply"},
	)

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "openai" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(&config.Config{Provider: "openai", Model: "model-a"})
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	if err := b.SubmitTurn(ctx, "first turn fails"); err != nil {
		t.Fatalf("submit failing turn: %v", err)
	}
	errEvent := waitForSessionError(t, b.Events())
	if !strings.Contains(errEvent.Err.Error(), providerErr.Error()) {
		t.Fatalf("error = %v, want provider error", errEvent.Err)
	}
	waitForTurnFinished(t, b.Events())

	if err := b.SubmitTurn(ctx, "second turn recovers"); err != nil {
		t.Fatalf("submit recovery turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	calls := provider.Calls()
	if len(calls) != 2 {
		t.Fatalf("provider calls = %d, want 2", len(calls))
	}

	cantoStore, ok := store.(interface{ Canto() *csession.SQLiteStore })
	if !ok {
		t.Fatal("expected canto-backed store")
	}
	cantoSess, err := cantoStore.Canto().Load(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}
	var terminalErrorFound bool
	for _, ev := range cantoSess.Events() {
		if ev.Type != csession.TurnCompleted {
			continue
		}
		data, ok, err := ev.TurnCompletedData()
		if err != nil {
			t.Fatalf("decode turn completed: %v", err)
		}
		if ok && strings.Contains(data.Error, providerErr.Error()) {
			terminalErrorFound = true
		}
	}
	if !terminalErrorFound {
		t.Fatalf("missing durable provider error terminal event")
	}
}

func TestConfigureRetryProviderUsesUntilCancelledSetting(t *testing.T) {
	events := make(chan ionsession.Event, 1)
	retryUntilCancelled := true
	provider := &retryProvider{
		FauxProvider: ctesting.NewMockProvider("openai"),
	}

	wrapped := configureRetryProvider(
		provider,
		&config.Config{RetryUntilCancelled: &retryUntilCancelled},
		events,
	)
	retry, ok := wrapped.(*llm.RetryProvider)
	if !ok {
		t.Fatalf("wrapped provider = %T, want *llm.RetryProvider", wrapped)
	}
	if !retry.Config.RetryForever {
		t.Fatal("RetryForever = false, want true")
	}
	if !retry.Config.RetryForeverTransportOnly {
		t.Fatal("RetryForeverTransportOnly = false, want true")
	}

	retry.Config.OnRetry(llm.RetryEvent{
		Attempt: 1,
		Delay:   2 * time.Second,
		Err:     transientStreamErr,
	})

	select {
	case ev := <-events:
		status, ok := ev.(ionsession.StatusChanged)
		if !ok {
			t.Fatalf("event = %T, want StatusChanged", ev)
		}
		if !strings.Contains(status.Status, "Retrying in 2s") {
			t.Fatalf("status = %q, want retry delay", status.Status)
		}
		if !strings.Contains(status.Status, "Provider error") {
			t.Fatalf("status = %q, want provider error label", status.Status)
		}
		if !strings.Contains(status.Status, "Ctrl+C stops") {
			t.Fatalf("status = %q, want cancel hint", status.Status)
		}
	default:
		t.Fatal("expected retry status event")
	}
}

func TestRetryStatusLabelsTransportErrors(t *testing.T) {
	status := retryStatus(llm.RetryEvent{
		Attempt: 1,
		Delay:   time.Second,
		Err:     syscall.ECONNRESET,
	})
	if !strings.Contains(status, "Network error") {
		t.Fatalf("status = %q, want network error label", status)
	}
}

func TestOpenRecoversFromContextOverflowByCompacting(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	storageSession, err := store.OpenSession(ctx, "/tmp/ion-overflow", "openai/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	appendCantoHistory(t, ctx, store, storageSession.ID(),
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("alpha ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("beta ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("gamma ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: "recent answer"},
		llm.Message{Role: llm.RoleUser, Content: "recent question"},
	)

	provider := &overflowRecoveryProvider{
		FauxProvider: ctesting.NewMockProvider(
			"openai",
			ctesting.Step{Err: overflowErr},
			ctesting.Step{Content: "compacted summary"},
			ctesting.Step{Content: "recovered reply"},
		),
	}

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "openai" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(&config.Config{Provider: "openai", Model: "model-a", ContextLimit: 100})
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	if err := b.SubmitTurn(ctx, "overflow recovery please"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	calls := provider.Calls()
	if len(calls) != 3 {
		t.Fatalf("provider calls = %d, want 3 (overflow, compact, retry)", len(calls))
	}
	if !requestContains(calls[2], "<conversation_summary>") {
		t.Fatalf("retry request was not rebuilt from compacted history: %#v", calls[2].Messages)
	}
	if requestContains(calls[2], strings.Repeat("alpha ", 20)) {
		t.Fatalf("retry request still contains pre-compaction history: %#v", calls[2].Messages)
	}

	resumed, err := store.ResumeSession(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("resume compacted session: %v", err)
	}
	entries, err := resumed.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if !entryExists(entries, ionsession.System, "<conversation_summary>") {
		t.Fatalf("expected automatic compaction to add a conversation summary, got %#v", entries)
	}

	cantoStore, ok := store.(interface{ Canto() *csession.SQLiteStore })
	if !ok {
		t.Fatal("expected canto-backed store")
	}
	sess, err := cantoStore.Canto().Load(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}
	var compactionEvents int
	for _, e := range sess.Events() {
		if e.Type == csession.CompactionTriggered {
			compactionEvents++
		}
	}
	if compactionEvents == 0 {
		t.Fatal("expected at least one durable compaction event")
	}
}

func requestContains(req *llm.Request, needle string) bool {
	if req == nil {
		return false
	}
	for _, msg := range req.Messages {
		if strings.Contains(msg.Content, needle) {
			return true
		}
	}
	return false
}

func TestSubmitTurnProactivelyCompactsBeforeOverflow(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	storageSession, err := store.OpenSession(ctx, "/tmp/ion-proactive", "openai/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	appendCantoHistory(t, ctx, store, storageSession.ID(),
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("alpha ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("beta ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("gamma ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: "recent answer"},
		llm.Message{Role: llm.RoleUser, Content: "recent question"},
	)

	provider := &overflowRecoveryProvider{
		FauxProvider: ctesting.NewMockProvider(
			"openai",
			ctesting.Step{Content: "compacted summary"},
			ctesting.Step{Content: "recovered reply"},
		),
	}

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "openai" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(&proactiveUsageSession{
		id: storageSession.ID(),
		meta: storage.Metadata{
			ID:     storageSession.ID(),
			CWD:    "/tmp/ion-proactive",
			Model:  "model-a",
			Branch: "main",
		},
		usageIn:  72,
		usageOut: 8,
	})
	b.SetConfig(&config.Config{Provider: "openai", Model: "model-a", ContextLimit: 100})
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	if err := b.SubmitTurn(ctx, "proactive compaction please"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	calls := provider.Calls()
	if len(calls) != 2 {
		t.Fatalf("provider calls = %d, want 2 (compact, turn)", len(calls))
	}

	resumed, err := store.ResumeSession(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("resume session: %v", err)
	}
	entries, err := resumed.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if !entryExists(entries, ionsession.System, "<conversation_summary>") {
		t.Fatalf("expected proactive compaction to add a conversation summary, got %#v", entries)
	}
	if !entryExists(entries, ionsession.Agent, "recovered reply") {
		t.Fatalf("expected final reply after proactive compaction, got %#v", entries)
	}
}

func TestSubmitTurnStopsWhenProactiveCompactionFails(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	root := t.TempDir()
	store, err := storage.NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	storageSession, err := store.OpenSession(
		ctx,
		"/tmp/ion-proactive-fail",
		"openai/model-a",
		"main",
	)
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	appendCantoHistory(t, ctx, store, storageSession.ID(),
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("alpha ", 60)},
	)

	provider := &overflowRecoveryProvider{
		FauxProvider: ctesting.NewMockProvider(
			"openai",
			ctesting.Step{Err: errors.New("compaction provider failed")},
			ctesting.Step{Content: "turn should not run"},
		),
	}

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "openai" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(&proactiveUsageSession{
		id: storageSession.ID(),
		meta: storage.Metadata{
			ID:     storageSession.ID(),
			CWD:    "/tmp/ion-proactive-fail",
			Model:  "model-a",
			Branch: "main",
		},
		usageIn:  72,
		usageOut: 8,
	})
	b.SetConfig(&config.Config{Provider: "openai", Model: "model-a", ContextLimit: 100})
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	if err := b.SubmitTurn(ctx, "do not send this after compaction failure"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}

	errEvent := waitForSessionError(t, b.Events())
	if !strings.Contains(errEvent.Err.Error(), "compaction provider failed") {
		t.Fatalf("error = %v, want compaction provider failure", errEvent.Err)
	}
	waitForTurnFinishedAfterError(t, b.Events())

	calls := provider.Calls()
	if len(calls) != 1 {
		t.Fatalf("provider calls = %d, want 1 compaction call only", len(calls))
	}
}

func TestOpenLoadsLayeredProjectInstructions(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	root := t.TempDir()
	if err := os.Mkdir(filepath.Join(root, ".git"), 0o755); err != nil {
		t.Fatalf("mkdir .git: %v", err)
	}
	nested := filepath.Join(root, "pkg", "feature")
	if err := os.MkdirAll(nested, 0o755); err != nil {
		t.Fatalf("mkdir nested: %v", err)
	}
	if err := os.WriteFile(filepath.Join(root, "AGENTS.md"), []byte("root instruction"), 0o644); err != nil {
		t.Fatalf("write root AGENTS: %v", err)
	}
	if err := os.WriteFile(filepath.Join(root, "pkg", "AGENTS.md"), []byte("pkg instruction"), 0o644); err != nil {
		t.Fatalf("write pkg AGENTS: %v", err)
	}

	ctx := context.Background()
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	storageSession, err := store.OpenSession(ctx, nested, "openai/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	mockProvider := ctesting.NewMockProvider("openai", ctesting.Step{Content: "ok"})
	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "openai" {
			return mockProvider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(&config.Config{Provider: "openai", Model: "model-a", ContextLimit: 100})
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	if err := b.SubmitTurn(ctx, "load instructions"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForTurnFinished(t, b.Events())

	calls := mockProvider.Calls()
	if len(calls) != 1 {
		t.Fatalf("provider calls = %d, want 1", len(calls))
	}
	req := calls[0]
	if !requestHasMessage(req.Messages, llm.RoleSystem, "root instruction") {
		t.Fatalf("provider request missing root instruction: %#v", req.Messages)
	}
	if !requestHasMessage(req.Messages, llm.RoleSystem, "pkg instruction") {
		t.Fatalf("provider request missing nested layer: %#v", req.Messages)
	}
	if !requestHasMessage(req.Messages, llm.RoleSystem, "## Project Instructions") {
		t.Fatalf("provider request missing project section: %#v", req.Messages)
	}
}

func waitForTurnFinished(t *testing.T, events <-chan ionsession.Event) {
	t.Helper()

	timeout := time.After(2 * time.Second)
	for {
		select {
		case ev, ok := <-events:
			if !ok {
				t.Fatal("event stream closed before turn finished")
			}
			switch msg := ev.(type) {
			case ionsession.Error:
				t.Fatalf("unexpected session error: %v", msg.Err)
			case ionsession.TurnFinished:
				return
			}
		case <-timeout:
			t.Fatal("timed out waiting for turn to finish")
		}
	}
}

func waitForSessionError(t *testing.T, events <-chan ionsession.Event) ionsession.Error {
	t.Helper()

	timeout := time.After(2 * time.Second)
	for {
		select {
		case ev, ok := <-events:
			if !ok {
				t.Fatal("event stream closed before session error")
			}
			if msg, ok := ev.(ionsession.Error); ok {
				return msg
			}
		case <-timeout:
			t.Fatal("timed out waiting for session error")
			return ionsession.Error{}
		}
	}
}

func waitForTurnFinishedAfterError(t *testing.T, events <-chan ionsession.Event) {
	t.Helper()

	timeout := time.After(2 * time.Second)
	for {
		select {
		case ev, ok := <-events:
			if !ok {
				t.Fatal("event stream closed before turn finished")
			}
			if _, ok := ev.(ionsession.TurnFinished); ok {
				return
			}
		case <-timeout:
			t.Fatal("timed out waiting for turn to finish after session error")
		}
	}
}

func receiveEvent(t *testing.T, events <-chan ionsession.Event) ionsession.Event {
	t.Helper()

	select {
	case ev, ok := <-events:
		if !ok {
			t.Fatal("event stream closed")
		}
		return ev
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for event")
		return nil
	}
}

func requestHasMessage(messages []llm.Message, role llm.Role, content string) bool {
	for _, msg := range messages {
		if msg.Role == role && strings.Contains(msg.Content, content) {
			return true
		}
	}
	return false
}

func specNames(specs []*llm.Spec) []string {
	names := make([]string, 0, len(specs))
	for _, spec := range specs {
		if spec != nil {
			names = append(names, spec.Name)
		}
	}
	return names
}

func entryExists(entries []ionsession.Entry, role ionsession.Role, content string) bool {
	for _, entry := range entries {
		if entry.Role == role && strings.Contains(entry.Content, content) {
			return true
		}
	}
	return false
}

func appendCantoHistory(
	t *testing.T,
	ctx context.Context,
	store storage.Store,
	sessionID string,
	messages ...llm.Message,
) {
	t.Helper()
	cantoStore, ok := store.(interface{ Canto() *csession.SQLiteStore })
	if !ok {
		t.Fatalf("store %T does not expose Canto history", store)
	}
	for _, msg := range messages {
		if err := cantoStore.Canto().Save(
			ctx,
			csession.NewEvent(sessionID, csession.MessageAdded, msg),
		); err != nil {
			t.Fatalf("append canto history: %v", err)
		}
	}
}
