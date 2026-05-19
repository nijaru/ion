package canto

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/canto/llm"
	csession "github.com/nijaru/canto/session"
	ctesting "github.com/nijaru/canto/x/testing"
	"github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

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

	appendCantoHistory(
		t, ctx, store, storageSession.ID(),
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
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

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

func TestResumedCompactedSessionSendsSummaryFollowUpHistory(t *testing.T) {
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
		"/tmp/ion-compact-resume",
		"openai/model-a",
		"main",
	)
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	appendCantoHistory(
		t, ctx, store, storageSession.ID(),
		llm.Message{Role: llm.RoleUser, Content: strings.Repeat("alpha old context ", 40)},
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("beta old answer ", 40)},
		llm.Message{Role: llm.RoleUser, Content: "recent question"},
		llm.Message{Role: llm.RoleAssistant, Content: "recent answer"},
	)

	compactProvider := &compactProvider{id: "openai"}
	followUpProvider := ctesting.NewFauxProvider(
		"openai",
		ctesting.Step{Content: "continued after compacted resume"},
	)
	var currentProvider llm.Provider = compactProvider

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "openai" {
			return currentProvider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	first := New()
	first.SetStore(store)
	first.SetSession(storageSession)
	first.SetConfig(&config.Config{Provider: "openai", Model: "model-a", ContextLimit: 100})
	if err := first.Session().Open(ctx); err != nil {
		t.Fatalf("open first backend: %v", err)
	}
	if compacted, err := first.Compact(ctx); err != nil {
		t.Fatalf("compact: %v", err)
	} else if !compacted {
		t.Fatal("expected compacted=true")
	}
	if err := first.Session().Close(); err != nil {
		t.Fatalf("close first backend: %v", err)
	}

	resumedSession, err := store.ResumeSession(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("resume storage session: %v", err)
	}
	entries, err := resumedSession.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if !entryExists(entries, ionsession.System, "<conversation_summary>") {
		t.Fatalf("resumed entries missing compaction summary: %#v", entries)
	}

	currentProvider = followUpProvider
	second := New()
	second.SetStore(store)
	second.SetSession(resumedSession)
	second.SetConfig(&config.Config{Provider: "openai", Model: "model-a", ContextLimit: 100_000})
	if err := second.Session().Resume(ctx, storageSession.ID()); err != nil {
		t.Fatalf("resume backend: %v", err)
	}
	defer func() { _ = second.Session().Close() }()

	if err := second.Session().SubmitTurn(ctx, "continue from compacted context"); err != nil {
		t.Fatalf("submit follow-up turn: %v", err)
	}
	waitForTurnFinished(t, second.Session().Events())

	calls := followUpProvider.Calls()
	if len(calls) != 1 {
		t.Fatalf("follow-up provider calls = %d, want 1", len(calls))
	}
	followUp := calls[0]
	if !requestContains(followUp, "<conversation_summary>") {
		t.Fatalf("follow-up request missing compaction summary: %#v", followUp.Messages)
	}
	if requestContains(followUp, "alpha old context") {
		t.Fatalf("follow-up request still contains compacted old context: %#v", followUp.Messages)
	}
	if !requestHasMessage(followUp.Messages, llm.RoleUser, "continue from compacted context") {
		t.Fatalf("follow-up request missing new user turn: %#v", followUp.Messages)
	}
}

func TestSubmitTurnDoesNotProactivelyCompactStalePreCompactionUsage(t *testing.T) {
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
		"/tmp/ion-compact-stale-usage",
		"openai/model-a",
		"main",
	)
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	appendCantoHistory(
		t,
		ctx,
		store,
		storageSession.ID(),
		llm.Message{Role: llm.RoleUser, Content: strings.Repeat("old context ", 80)},
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("old answer ", 80)},
	)

	cantoStore, ok := store.(interface{ Canto() *csession.SQLiteStore })
	if !ok {
		t.Fatal("expected canto-backed store")
	}
	sess, err := cantoStore.Canto().Load(ctx, storageSession.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}
	var cutoffID string
	for _, e := range sess.Events() {
		if e.Type == csession.MessageAdded {
			cutoffID = e.ID.String()
		}
	}
	if cutoffID == "" {
		t.Fatal("missing cutoff event")
	}
	if err := cantoStore.Canto().Save(ctx, csession.NewCompactionEvent(
		storageSession.ID(),
		csession.CompactionSnapshot{
			Strategy:      "summarize",
			MaxTokens:     100,
			ThresholdPct:  proactiveCompactThreshold,
			CurrentTokens: 80,
			CutoffEventID: cutoffID,
			Entries: []csession.HistoryEntry{{
				EventType:        csession.ContextAdded,
				ContextKind:      csession.ContextKindSummary,
				ContextPlacement: csession.ContextPlacementPrefix,
				Message: llm.Message{
					Role:    llm.RoleUser,
					Content: "<conversation_summary>\nshort summary\n</conversation_summary>",
				},
			}},
		},
	)); err != nil {
		t.Fatalf("append compaction snapshot: %v", err)
	}
	if err := storageSession.Append(ctx, storage.TokenUsage{Input: 72, Output: 8}); err != nil {
		t.Fatalf("append stale usage: %v", err)
	}

	provider := &heuristicCountProvider{
		FauxProvider: ctesting.NewFauxProvider(
			"openai",
			ctesting.Step{Content: "normal reply"},
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
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "continue from summary"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}

	var statuses []string
	finished := false
	for !finished {
		select {
		case ev := <-b.Session().Events():
			switch msg := ev.(type) {
			case ionsession.StatusChanged:
				statuses = append(statuses, msg.Status)
			case ionsession.TurnFinished:
				finished = true
			}
		case <-time.After(backendEventWaitTimeout):
			t.Fatal("timed out waiting for turn")
		}
	}
	if containsString(statuses, "Compacting context...") {
		t.Fatalf("statuses = %#v, want no stale-usage proactive compaction", statuses)
	}
	if calls := provider.Calls(); len(calls) != 1 {
		t.Fatalf("provider calls = %d, want 1 normal turn call", len(calls))
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

	appendCantoHistory(
		t, ctx, store, storageSession.ID(),
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("alpha ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("beta ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("gamma ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: "recent answer"},
		llm.Message{Role: llm.RoleUser, Content: "recent question"},
	)

	provider := &overflowRecoveryProvider{
		FauxProvider: ctesting.NewFauxProvider(
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
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "overflow recovery please"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	events := b.Session().Events()
	seenCompactingStatus := false
	finished := false
	for !finished {
		select {
		case ev := <-events:
			switch msg := ev.(type) {
			case ionsession.StatusChanged:
				if msg.Status == "Compacting context..." {
					seenCompactingStatus = true
					if msg.Timestamp.IsZero() {
						t.Fatal("overflow compaction status has zero timestamp")
					}
				}
			case ionsession.TurnFinished:
				if !seenCompactingStatus {
					t.Fatal("missing overflow compaction status")
				}
				finished = true
			}
		case <-time.After(backendEventWaitTimeout):
			t.Fatal("timed out waiting for overflow recovery turn")
		}
	}

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

	appendCantoHistory(
		t, ctx, store, storageSession.ID(),
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("alpha ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("beta ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("gamma ", 60)},
		llm.Message{Role: llm.RoleAssistant, Content: "recent answer"},
		llm.Message{Role: llm.RoleUser, Content: "recent question"},
	)

	provider := &fixedCountProvider{
		FauxProvider: ctesting.NewFauxProvider(
			"openai",
			ctesting.Step{Content: "compacted summary"},
			ctesting.Step{Content: "recovered reply"},
		),
		tokens: 80,
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
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "proactive compaction please"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	events := b.Session().Events()
	var statuses []string
	finished := false
	for !finished {
		select {
		case ev := <-events:
			switch msg := ev.(type) {
			case ionsession.StatusChanged:
				statuses = append(statuses, msg.Status)
			case ionsession.TurnFinished:
				finished = true
			}
		case <-time.After(backendEventWaitTimeout):
			t.Fatal("timed out waiting for proactive compaction turn")
		}
	}
	if !containsString(statuses, "Compacting context...") {
		t.Fatalf("statuses = %#v, want proactive compaction status", statuses)
	}
	if containsString(statuses, "Ready") {
		t.Fatalf("statuses = %#v, want no non-terminal Ready during proactive turn", statuses)
	}

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

	appendCantoHistory(
		t, ctx, store, storageSession.ID(),
		llm.Message{Role: llm.RoleAssistant, Content: strings.Repeat("alpha ", 60)},
	)

	provider := &fixedCountProvider{
		FauxProvider: ctesting.NewFauxProvider(
			"openai",
			ctesting.Step{Err: errors.New("compaction provider failed")},
			ctesting.Step{Content: "turn should not run"},
		),
		tokens: 80,
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
	if err := b.Session().Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Session().Close() }()

	if err := b.Session().SubmitTurn(ctx, "do not send this after compaction failure"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}

	errEvent := waitForSessionError(t, b.Session().Events())
	if !strings.Contains(errEvent.Err.Error(), "compaction provider failed") {
		t.Fatalf("error = %v, want compaction provider failure", errEvent.Err)
	}
	waitForTurnFinishedAfterError(t, b.Session().Events())

	calls := provider.Calls()
	if len(calls) != 1 {
		t.Fatalf("provider calls = %d, want 1 compaction call only", len(calls))
	}
}

func containsString(values []string, needle string) bool {
	for _, value := range values {
		if value == needle {
			return true
		}
	}
	return false
}
