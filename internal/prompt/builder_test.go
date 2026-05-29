package prompt

import (
	"context"
	"strings"
	"testing"

	"github.com/nijaru/ion/internal/llm"
	"github.com/nijaru/ion/internal/storage/session"
	"github.com/nijaru/ion/internal/tool"
)

func TestBuilder_Build(t *testing.T) {
	sess := session.New("test-session")
	_ = sess.Append(
		context.Background(),
		session.NewEvent(sess.ID(), session.MessageAdded, llm.Message{
			Role:    llm.RoleUser,
			Content: "Hello world",
		}),
	)

	reg := tool.NewRegistry()
	// Add a mock tool
	// ... (assuming registry works)

	builder := NewBuilder(
		Instructions("You are a helpful assistant."),
		History(),
		Tools(reg),
	)

	req := &llm.Request{
		Model: "gpt-4o",
	}

	err := builder.Build(context.Background(), nil, "", sess, req)
	if err != nil {
		t.Fatalf("build failed: %v", err)
	}

	// Verify messages
	if len(req.Messages) != 2 {
		t.Errorf("expected 2 messages, got %d", len(req.Messages))
	}
	if req.Messages[0].Role != llm.RoleSystem {
		t.Errorf("expected first message to be system, got %s", req.Messages[0].Role)
	}
	if req.Messages[1].Content != "Hello world" {
		t.Errorf("expected second message to be 'Hello world', got %s", req.Messages[1].Content)
	}
}

func TestHistoryUsesLatestCompactionSnapshot(t *testing.T) {
	sess := session.New("compacted-session")
	oldUser := llm.Message{Role: llm.RoleUser, Content: "old user"}
	oldAssistant := llm.Message{Role: llm.RoleAssistant, Content: "old assistant"}
	recent := llm.Message{Role: llm.RoleUser, Content: "recent"}

	for _, msg := range []llm.Message{oldUser, oldAssistant, recent} {
		if err := sess.Append(context.Background(), session.NewMessage(sess.ID(), msg)); err != nil {
			t.Fatalf("append history: %v", err)
		}
	}

	events := sess.Events()
	snapshot := session.CompactionSnapshot{
		Strategy:      "summarize",
		CutoffEventID: events[len(events)-1].ID.String(),
		Messages: []llm.Message{
			{
				Role:    llm.RoleSystem,
				Content: "<conversation_summary>\nsummary\n</conversation_summary>",
			},
			recent,
		},
	}
	if err := sess.Append(context.Background(), session.NewCompactionEvent(sess.ID(), snapshot)); err != nil {
		t.Fatalf("append compaction: %v", err)
	}

	after := llm.Message{Role: llm.RoleAssistant, Content: "after"}
	if err := sess.Append(context.Background(), session.NewMessage(sess.ID(), after)); err != nil {
		t.Fatalf("append after: %v", err)
	}

	req := &llm.Request{}
	if err := History().ApplyRequest(context.Background(), nil, "", sess, req); err != nil {
		t.Fatalf("history process: %v", err)
	}

	if len(req.Messages) != 3 {
		t.Fatalf("expected 3 messages from compacted history, got %d", len(req.Messages))
	}
	if req.Messages[0].Content != "<conversation_summary>\nsummary\n</conversation_summary>" {
		t.Fatalf("unexpected summary message: %q", req.Messages[0].Content)
	}
	if req.Messages[1].Content != "recent" || req.Messages[2].Content != "after" {
		t.Fatalf("unexpected compacted history: %#v", req.Messages)
	}
}

func TestHistoryPreservesRecoveredToolContentParts(t *testing.T) {
	sess := session.New("content-tool-history")
	call := llm.Call{ID: "call-image", Type: "function"}
	call.Function.Name = "read"
	call.Function.Arguments = `{"path":"screen.png"}`

	if err := sess.Append(t.Context(), session.NewMessage(sess.ID(), llm.Message{
		Role:  llm.RoleAssistant,
		Calls: []llm.Call{call},
	})); err != nil {
		t.Fatalf("append assistant call: %v", err)
	}
	if err := sess.Append(t.Context(), session.NewToolStartedEvent(sess.ID(), session.ToolStartedData{
		ID:        "call-image",
		Tool:      "read",
		Arguments: `{"path":"screen.png"}`,
	})); err != nil {
		t.Fatalf("append tool started: %v", err)
	}
	if err := sess.Append(t.Context(), session.NewToolCompletedEvent(sess.ID(), session.ToolCompletedData{
		ID:     "call-image",
		Tool:   "read",
		Output: "image read",
		Parts: []llm.ContentPart{
			llm.TextPart("image read"),
			llm.ImagePart("image/png", "aW1hZ2U="),
		},
	})); err != nil {
		t.Fatalf("append tool completed: %v", err)
	}

	req := &llm.Request{}
	if err := History().ApplyRequest(t.Context(), nil, "", sess, req); err != nil {
		t.Fatalf("History: %v", err)
	}

	if len(req.Messages) != 2 {
		t.Fatalf("request history = %#v, want assistant plus recovered tool result", req.Messages)
	}
	toolResult := req.Messages[1]
	if toolResult.Role != llm.RoleTool ||
		toolResult.ToolID != "call-image" ||
		toolResult.Name != "read" ||
		toolResult.Content != "image read" {
		t.Fatalf("unexpected recovered tool result: %#v", toolResult)
	}
	if len(toolResult.Parts) != 2 ||
		toolResult.Parts[1].Type != llm.ContentPartImage ||
		toolResult.Parts[1].Data != "aW1hZ2U=" {
		t.Fatalf("recovered request parts = %+v, want text plus image", toolResult.Parts)
	}
}

func TestHistoryPlacesPrefixContextBeforeTranscript(t *testing.T) {
	sess := session.New("prefix-context")
	if err := sess.AppendUser(t.Context(), "first user"); err != nil {
		t.Fatalf("AppendUser: %v", err)
	}
	if err := sess.AppendContext(t.Context(), session.ContextEntry{
		Kind:      session.ContextKindGeneric,
		Placement: session.ContextPlacementPrefix,
		Content:   "stable workspace context",
	}); err != nil {
		t.Fatalf("AppendContext: %v", err)
	}
	if err := sess.AppendUser(t.Context(), "second user"); err != nil {
		t.Fatalf("AppendUser: %v", err)
	}

	req := &llm.Request{
		Messages: []llm.Message{{Role: llm.RoleSystem, Content: "system"}},
	}
	if err := History().ApplyRequest(t.Context(), nil, "", sess, req); err != nil {
		t.Fatalf("History: %v", err)
	}

	if req.CachePrefixMessages != 2 {
		t.Fatalf("expected system plus stable context prefix, got %d", req.CachePrefixMessages)
	}
	if got := req.Messages[1].Content; got != "stable workspace context" {
		t.Fatalf("expected stable context before transcript, got %q", got)
	}
	if req.Messages[2].Content != "first user" || req.Messages[3].Content != "second user" {
		t.Fatalf("expected transcript after stable context, got %#v", req.Messages)
	}
}

func TestLateInstructionsPreserveCachePrefixBoundary(t *testing.T) {
	sess := session.New("late-instructions")
	if err := sess.AppendContext(t.Context(), session.ContextEntry{
		Kind:    session.ContextKindBootstrap,
		Content: "stable context",
	}); err != nil {
		t.Fatalf("AppendContext: %v", err)
	}
	if err := sess.AppendUser(t.Context(), "hello"); err != nil {
		t.Fatalf("AppendUser: %v", err)
	}

	req := &llm.Request{}
	if err := History().ApplyRequest(t.Context(), nil, "", sess, req); err != nil {
		t.Fatalf("History: %v", err)
	}
	if err := Instructions("late system").ApplyRequest(t.Context(), nil, "", sess, req); err != nil {
		t.Fatalf("Instructions: %v", err)
	}

	if req.CachePrefixMessages != 2 {
		t.Fatalf("expected system plus context cache prefix, got %d", req.CachePrefixMessages)
	}
	if req.Messages[0].Role != llm.RoleSystem ||
		req.Messages[1].Content != "stable context" ||
		req.Messages[2].Content != "hello" {
		t.Fatalf("unexpected message order: %#v", req.Messages)
	}
}

func TestHistoryDemotesSessionSystemMessagesToTranscriptContext(t *testing.T) {
	sess := session.New("system-history")
	// System prompt is now stored separately, not as a message event
	sess.SetSystemPrompt("app-local notice")
	for _, msg := range []llm.Message{
		{Role: llm.RoleUser, Content: "hello"},
	} {
		if err := sess.Append(t.Context(), session.NewMessage(sess.ID(), msg)); err != nil {
			t.Fatalf("append history: %v", err)
		}
	}

	req := &llm.Request{}
	builder := NewBuilder(
		Instructions("privileged instruction"),
		History(),
	)
	if err := builder.Build(t.Context(), nil, "", sess, req); err != nil {
		t.Fatalf("Build: %v", err)
	}

	// System prompt from session is combined with instructions
	if len(req.Messages) != 2 {
		t.Fatalf("expected 2 messages, got %d", len(req.Messages))
	}
	if req.Messages[0].Role != llm.RoleSystem {
		t.Fatalf("expected leading system message, got %#v", req.Messages[0])
	}
	if !strings.Contains(req.Messages[0].Content, "privileged instruction") {
		t.Fatalf("expected instructions in system message, got %#v", req.Messages[0])
	}
	if !strings.Contains(req.Messages[0].Content, "app-local notice") {
		t.Fatalf("expected session system prompt in system message, got %#v", req.Messages[0])
	}
	if req.Messages[1].Role != llm.RoleUser {
		t.Fatalf("expected user message, got %#v", req.Messages[1])
	}
}

func TestRequestProcessorFuncIsRequestOnly(t *testing.T) {
	proc := RequestProcessorFunc(
		func(ctx context.Context, p llm.Provider, model string, sess *session.Session, req *llm.Request) error {
			req.Messages = append(req.Messages, llm.Message{Role: llm.RoleSystem, Content: "hi"})
			return nil
		},
	)

	effects := requestProcessorEffects(proc)
	if effects.HasSideEffects() {
		t.Fatalf("expected request-only processor, got %#v", effects)
	}
}

func TestBuilderEffectsAggregatesProcessorSideEffects(t *testing.T) {
	builder := NewBuilder(Instructions("system"))
	builder.AppendMutators(
		&dummyMutator{strategy: "offload"},
		&dummyMutator{strategy: "summarize"},
	)

	effects := builder.Effects()
	if !effects.Session {
		t.Fatalf("expected session side effects, got %#v", effects)
	}
	if !effects.External {
		t.Fatalf("expected external side effects from offloader, got %#v", effects)
	}
}

func TestBuilderBuildPreviewSkipsSideEffects(t *testing.T) {
	builder := NewBuilder(Instructions("system"))
	builder.AppendMutators(&dummyMutator{strategy: "offload"})

	err := builder.BuildPreview(t.Context(), nil, "", session.New("preview"), &llm.Request{})
	if err != nil {
		t.Fatalf("BuildPreview expected success, got error: %v", err)
	}
}

func TestPipelineBuildCommitRunsMutatorsBeforeRequestProcessors(t *testing.T) {
	sess := session.New("pipeline")
	pipeline := NewPipeline(RequestProcessorFunc(
		func(ctx context.Context, p llm.Provider, model string, sess *session.Session, req *llm.Request) error {
			msgs, err := sess.EffectiveMessages()
			if err != nil {
				return err
			}
			req.Messages = append(req.Messages, msgs...)
			return nil
		},
	))
	pipeline.AddMutator(ContextMutatorFunc(
		func(ctx context.Context, p llm.Provider, model string, sess *session.Session) error {
			return sess.Append(ctx, session.NewMessage(sess.ID(), llm.Message{
				Role:    llm.RoleUser,
				Content: "mutated first",
			}))
		},
	))

	req := &llm.Request{}
	if err := pipeline.BuildCommit(t.Context(), nil, "", sess, req); err != nil {
		t.Fatalf("BuildCommit: %v", err)
	}
	if len(req.Messages) != 1 || req.Messages[0].Content != "mutated first" {
		t.Fatalf("unexpected commit-built messages: %#v", req.Messages)
	}
}

func TestBuilderPhasedHelpersSupportRequestProcessorsAndMutators(t *testing.T) {
	sess := session.New("builder-phases")
	builder := NewBuilder()
	builder.AppendMutators(ContextMutatorFunc(
		func(ctx context.Context, p llm.Provider, model string, sess *session.Session) error {
			return sess.Append(ctx, session.NewMessage(sess.ID(), llm.Message{
				Role:    llm.RoleUser,
				Content: "from mutator",
			}))
		},
	))
	builder.AppendRequestProcessors(RequestProcessorFunc(
		func(ctx context.Context, p llm.Provider, model string, sess *session.Session, req *llm.Request) error {
			msgs, err := sess.EffectiveMessages()
			if err != nil {
				return err
			}
			req.Messages = append(req.Messages, msgs...)
			return nil
		},
	))

	if err := builder.BuildPreview(t.Context(), nil, "", sess, &llm.Request{}); err != nil {
		t.Fatalf("BuildPreview expected success, got error: %v", err)
	}

	req := &llm.Request{}
	if err := builder.BuildCommit(t.Context(), nil, "", sess, req); err != nil {
		t.Fatalf("BuildCommit: %v", err)
	}
	if len(req.Messages) != 1 || req.Messages[0].Content != "from mutator" {
		t.Fatalf("unexpected commit-built messages: %#v", req.Messages)
	}
}

func TestBuilderInsertRequestProcessorsBeforeCache(t *testing.T) {
	builder := NewBuilder(
		Instructions("base"),
		History(),
		CacheAligner(2),
	)
	builder.InsertRequestProcessorsBeforeCache(RequestProcessorFunc(
		func(ctx context.Context, p llm.Provider, model string, sess *session.Session, req *llm.Request) error {
			return Instructions("custom").ApplyRequest(ctx, p, model, sess, req)
		},
	))

	req := &llm.Request{}
	if err := builder.BuildPreview(t.Context(), nil, "", session.New("cache-order"), req); err != nil {
		t.Fatalf("BuildPreview: %v", err)
	}
	if len(req.Messages) != 1 {
		t.Fatalf("messages = %d, want 1", len(req.Messages))
	}
	if got, want := req.Messages[0].Content, "custom\n\nbase"; got != want {
		t.Fatalf("system content = %q, want %q", got, want)
	}
	if req.Messages[0].CacheControl == nil {
		t.Fatal("expected cache alignment to see custom system content")
	}
}

func TestBuilderAppendRequestProcessorsBeforeCacheFinalizers(t *testing.T) {
	builder := NewBuilder(
		Instructions("base"),
		History(),
		CacheAligner(2),
	)
	builder.AppendRequestProcessors(RequestProcessorFunc(
		func(ctx context.Context, p llm.Provider, model string, sess *session.Session, req *llm.Request) error {
			return Instructions("appended").ApplyRequest(ctx, p, model, sess, req)
		},
	))

	req := &llm.Request{}
	if err := builder.BuildPreview(t.Context(), nil, "", session.New("append-cache-order"), req); err != nil {
		t.Fatalf("BuildPreview: %v", err)
	}
	if got, want := req.Messages[0].Content, "appended\n\nbase"; got != want {
		t.Fatalf("system content = %q, want %q", got, want)
	}
	if req.Messages[0].CacheControl == nil {
		t.Fatal("expected cache alignment to run after appended processor")
	}
}

type dummyMutator struct{ strategy string }

func (m *dummyMutator) Mutate(
	ctx context.Context,
	pr llm.Provider,
	model string,
	sess *session.Session,
) error {
	return nil
}

func (m *dummyMutator) Effects() SideEffects {
	if m.strategy == "offload" {
		return SideEffects{Session: true, External: true}
	}
	if m.strategy == "summarize" {
		return SideEffects{Session: true, External: false}
	}
	return SideEffects{}
}
func (m *dummyMutator) CompactionStrategy() string { return m.strategy }
