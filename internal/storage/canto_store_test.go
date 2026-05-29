package storage

import (
	"context"
	"encoding/json"
	"errors"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/internal/llm"
	csession "github.com/nijaru/ion/internal/storage/session"
	ionsession "github.com/nijaru/ion/internal/session"
)

func appendCantoMessage(
	t testing.TB,
	store *cantoStore,
	ctx context.Context,
	sessionID string,
	msg llm.Message,
) {
	t.Helper()
	if err := store.canto.Save(ctx, csession.NewEvent(sessionID, csession.MessageAdded, msg)); err != nil {
		t.Fatalf("append canto message: %v", err)
	}
}

func appendLegacyCantoMessage(
	t testing.TB,
	store *cantoStore,
	ctx context.Context,
	sessionID string,
	msg llm.Message,
) {
	t.Helper()
	event := csession.NewEvent(sessionID, csession.MessageAdded, msg)
	if _, err := store.db.ExecContext(
		ctx,
		"INSERT INTO events (id, session_id, type, timestamp, data, metadata, cost) VALUES (?, ?, ?, ?, ?, ?, ?)",
		event.ID.String(),
		event.SessionID,
		string(event.Type),
		event.Timestamp.Format(time.RFC3339Nano),
		event.Data,
		nil,
		event.Cost,
	); err != nil {
		t.Fatalf("append legacy canto message: %v", err)
	}
}

func assistantToolCall(id, name string) llm.Call {
	call := llm.Call{ID: id, Type: "function"}
	call.Function.Name = name
	call.Function.Arguments = `{}`
	return call
}

func withCantoTimestamp(event csession.Event, timestamp time.Time) csession.Event {
	event.Timestamp = timestamp.UTC()
	return event
}

func TestNewEphemeralCantoStorePersistsWithinProcessOnly(t *testing.T) {
	storeAny, err := NewEphemeralCantoStore()
	if err != nil {
		t.Fatalf("new ephemeral canto store: %v", err)
	}
	defer storeAny.Close()

	store := storeAny.(*cantoStore)
	if store.dbPath != "" {
		t.Fatalf("ephemeral dbPath = %q, want empty durable path", store.dbPath)
	}

	ctx := t.Context()
	sess, err := store.OpenSession(ctx, "/tmp/ion-ephemeral", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	if err := sess.Append(ctx, Status{Status: "Ready"}); err != nil {
		t.Fatalf("append status: %v", err)
	}

	sessions, err := store.ListSessions(ctx, "/tmp/ion-ephemeral")
	if err != nil {
		t.Fatalf("list sessions: %v", err)
	}
	if len(sessions) != 1 || sessions[0].ID != sess.ID() {
		t.Fatalf("sessions = %#v, want ephemeral session %s", sessions, sess.ID())
	}
}

func TestCantoStoreConfiguresMetadataSQLitePool(t *testing.T) {
	storeAny, err := NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	defer storeAny.Close()

	store := storeAny.(*cantoStore)
	if got := store.db.Stats().MaxOpenConnections; got != metadataSQLiteMaxOpenConns {
		t.Fatalf("metadata db max open connections = %d, want %d", got, metadataSQLiteMaxOpenConns)
	}
}

func TestEphemeralCantoStoreUsesSingleMetadataSQLiteConnection(t *testing.T) {
	storeAny, err := NewEphemeralCantoStore()
	if err != nil {
		t.Fatalf("new ephemeral canto store: %v", err)
	}
	defer storeAny.Close()

	store := storeAny.(*cantoStore)
	if got := store.db.Stats().MaxOpenConnections; got != 1 {
		t.Fatalf("ephemeral metadata db max open connections = %d, want 1", got)
	}
}

func TestCantoStoreGetInputsOrdersSameSecondByInsertion(t *testing.T) {
	storeAny, err := NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	defer storeAny.Close()
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	cwd := "/tmp/ion-input-history"
	ts := time.Date(2026, time.May, 14, 12, 0, 0, 0, time.UTC).Unix()
	for _, content := range []string{"first", "second", "third"} {
		if _, err := store.db.ExecContext(
			ctx,
			"INSERT INTO inputs (cwd, content, created_at) VALUES (?, ?, ?)",
			cwd,
			content,
			ts,
		); err != nil {
			t.Fatalf("insert input: %v", err)
		}
	}

	got, err := store.GetInputs(ctx, cwd, 3)
	if err != nil {
		t.Fatalf("get inputs: %v", err)
	}
	want := []string{"third", "second", "first"}
	if !slices.Equal(got, want) {
		t.Fatalf("inputs = %#v, want %#v", got, want)
	}
}

func TestCantoSessionUsagePrefersTurnCompletedUsage(t *testing.T) {
	storeAny, err := NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	defer storeAny.Close()

	store := storeAny.(*cantoStore)
	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-usage", "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	if err := store.canto.Save(ctx, csession.NewTurnStartedEvent(
		sess.ID(),
		csession.TurnStartedData{AgentID: "ion"},
	)); err != nil {
		t.Fatalf("save turn started: %v", err)
	}
	if err := sess.Append(ctx, TokenUsage{Input: 10, Output: 2, Cost: 0.01}); err != nil {
		t.Fatalf("append token usage: %v", err)
	}
	if err := store.canto.Save(ctx, csession.NewTurnCompletedEvent(
		sess.ID(),
		csession.TurnCompletedData{
			AgentID: "ion",
			Usage: llm.Usage{
				InputTokens:  10,
				OutputTokens: 2,
				Cost:         0.01,
			},
		},
	)); err != nil {
		t.Fatalf("save turn completed: %v", err)
	}

	input, output, cost, err := sess.Usage(ctx)
	if err != nil {
		t.Fatalf("usage: %v", err)
	}
	if input != 10 || output != 2 || cost != 0.01 {
		t.Fatalf("usage = %d/%d/%f, want 10/2/0.01", input, output, cost)
	}
}

func TestCantoSessionUsageFallsBackToTokenUsageWhenTerminalUsageMissing(t *testing.T) {
	storeAny, err := NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	defer storeAny.Close()

	store := storeAny.(*cantoStore)
	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-usage-fallback", "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	if err := store.canto.Save(ctx, csession.NewTurnStartedEvent(
		sess.ID(),
		csession.TurnStartedData{AgentID: "ion"},
	)); err != nil {
		t.Fatalf("save turn started: %v", err)
	}
	if err := sess.Append(ctx, TokenUsage{Input: 5, Output: 1, Cost: 0.02}); err != nil {
		t.Fatalf("append token usage: %v", err)
	}
	if err := store.canto.Save(ctx, csession.NewTurnCompletedEvent(
		sess.ID(),
		csession.TurnCompletedData{
			AgentID: "ion",
			Error:   "stream failed before final usage",
		},
	)); err != nil {
		t.Fatalf("save turn completed: %v", err)
	}

	input, output, cost, err := sess.Usage(ctx)
	if err != nil {
		t.Fatalf("usage: %v", err)
	}
	if input != 5 || output != 1 || cost != 0.02 {
		t.Fatalf("usage = %d/%d/%f, want 5/1/0.02", input, output, cost)
	}
}

func TestCantoStoreAppendUpdatesRecentSession(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	cwd := "/tmp/ion-storage-test"

	first, err := store.OpenSession(ctx, cwd, "model-a", "main")
	if err != nil {
		t.Fatalf("open first session: %v", err)
	}

	time.Sleep(1100 * time.Millisecond)

	second, err := store.OpenSession(ctx, cwd, "model-b", "main")
	if err != nil {
		t.Fatalf("open second session: %v", err)
	}

	recent, err := store.GetRecentSession(ctx, cwd)
	if err != nil {
		t.Fatalf("recent session before append: %v", err)
	}
	if recent.ID != second.ID() {
		t.Fatalf("recent session before append = %q, want %q", recent.ID, second.ID())
	}

	time.Sleep(1100 * time.Millisecond)

	if err := first.Append(ctx, Status{Status: "Network error. Retrying in 2s... Ctrl+C stops."}); err != nil {
		t.Fatalf("append status: %v", err)
	}

	recent, err = store.GetRecentSession(ctx, cwd)
	if err != nil {
		t.Fatalf("recent session after append: %v", err)
	}
	if recent.ID != first.ID() {
		t.Fatalf("recent session after append = %q, want %q", recent.ID, first.ID())
	}
}

func TestCantoStoreUpdateSessionFailsWhenMetadataMissing(t *testing.T) {
	storeAny, err := NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	defer storeAny.Close()

	store := storeAny.(*cantoStore)
	err = store.UpdateSession(context.Background(), SessionInfo{
		ID:          "missing-session",
		Model:       "openrouter/test-model",
		Branch:      "main",
		LastPreview: "hello",
	})
	if err == nil {
		t.Fatal("update missing session succeeded")
	}
	if !strings.Contains(err.Error(), "missing-session metadata not found") {
		t.Fatalf("update error = %v, want missing metadata error", err)
	}
}

func TestCantoStoreForkSessionCopiesEventsAndIndexesChild(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)
	ctx := context.Background()
	cwd := "/tmp/ion-storage-test"

	parent, err := store.OpenSession(ctx, cwd, "openrouter/test-model", "main")
	if err != nil {
		t.Fatalf("open parent session: %v", err)
	}
	parentAt := time.Date(2026, 5, 2, 15, 0, 0, 0, time.UTC)
	parentEvent := withCantoTimestamp(
		csession.NewEvent(parent.ID(), csession.MessageAdded, llm.Message{
			Role:    llm.RoleUser,
			Content: "debug the flaky test",
		}),
		parentAt,
	)
	if err := store.canto.Save(ctx, parentEvent); err != nil {
		t.Fatalf("append parent message: %v", err)
	}
	if err := store.UpdateSession(ctx, SessionInfo{
		ID:          parent.ID(),
		Title:       "debug task",
		LastPreview: "debug the flaky test",
	}); err != nil {
		t.Fatalf("update parent session: %v", err)
	}

	child, err := store.ForkSession(ctx, parent.ID(), ForkOptions{
		Label:  "try alternate fix",
		Reason: "test fork",
	})
	if err != nil {
		t.Fatalf("fork session: %v", err)
	}
	if child.ID() == parent.ID() {
		t.Fatal("fork returned parent session id")
	}
	if child.Meta().Model != "openrouter/test-model" {
		t.Fatalf("child model = %q, want parent model", child.Meta().Model)
	}

	entries, err := child.Entries(ctx)
	if err != nil {
		t.Fatalf("child entries: %v", err)
	}
	if len(entries) != 1 || entries[0].Content != "debug the flaky test" {
		t.Fatalf("child entries = %#v, want copied parent transcript", entries)
	}
	if !entries[0].Timestamp.Equal(parentAt) {
		t.Fatalf("child copied timestamp = %s, want %s", entries[0].Timestamp, parentAt)
	}

	children, err := store.canto.Children(ctx, parent.ID())
	if err != nil {
		t.Fatalf("load Canto children: %v", err)
	}
	if len(children) != 1 || children[0].SessionID != child.ID() {
		t.Fatalf("children = %#v, want child %s", children, child.ID())
	}
	if children[0].BranchLabel != "try alternate fix" {
		t.Fatalf("branch label = %q, want label", children[0].BranchLabel)
	}

	listed, err := store.ListSessions(ctx, cwd)
	if err != nil {
		t.Fatalf("list sessions: %v", err)
	}
	var found bool
	for _, info := range listed {
		if info.ID == child.ID() {
			found = true
			if info.Title != "try alternate fix" {
				t.Fatalf("child title = %q, want label", info.Title)
			}
			if info.LastPreview != "debug the flaky test" {
				t.Fatalf("child preview = %q, want parent preview", info.LastPreview)
			}
		}
	}
	if !found {
		t.Fatalf("forked session %s missing from ListSessions: %#v", child.ID(), listed)
	}

	tree, err := store.SessionTree(ctx, child.ID())
	if err != nil {
		t.Fatalf("session tree: %v", err)
	}
	if len(tree.Lineage) != 2 {
		t.Fatalf("lineage = %#v, want parent and child", tree.Lineage)
	}
	if tree.Lineage[0].ID != parent.ID() || tree.Lineage[1].ID != child.ID() {
		t.Fatalf("lineage ids = %#v, want parent then child", tree.Lineage)
	}
	if tree.Current.ID != child.ID() {
		t.Fatalf("current = %q, want child", tree.Current.ID)
	}
}

func TestCantoStoreSessionBundleExportsAndImportsLineage(t *testing.T) {
	exportAny, err := NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new export store: %v", err)
	}
	exportStore := exportAny.(*cantoStore)

	ctx := context.Background()
	cwd := "/tmp/ion-storage-test"
	parent, err := exportStore.OpenSession(ctx, cwd, "openrouter/test-model", "main")
	if err != nil {
		t.Fatalf("open parent session: %v", err)
	}
	parentAt := time.Date(2026, 5, 2, 16, 0, 0, 0, time.UTC)
	parentEvent := withCantoTimestamp(
		csession.NewEvent(parent.ID(), csession.MessageAdded, llm.Message{
			Role:    llm.RoleUser,
			Content: "debug the flaky test",
		}),
		parentAt,
	)
	if err := exportStore.canto.Save(ctx, parentEvent); err != nil {
		t.Fatalf("append parent message: %v", err)
	}
	if err := exportStore.UpdateSession(ctx, SessionInfo{
		ID:          parent.ID(),
		Title:       "debug task",
		LastPreview: "debug the flaky test",
	}); err != nil {
		t.Fatalf("update parent session: %v", err)
	}

	child, err := exportStore.ForkSession(ctx, parent.ID(), ForkOptions{
		Label:  "try alternate fix",
		Reason: "test fork",
	})
	if err != nil {
		t.Fatalf("fork session: %v", err)
	}
	childAt := parentAt.Add(time.Minute)
	childEvent := withCantoTimestamp(
		csession.NewEvent(child.ID(), csession.MessageAdded, llm.Message{
			Role:    llm.RoleAssistant,
			Content: "alternate fix works",
		}),
		childAt,
	)
	if err := exportStore.canto.Save(ctx, childEvent); err != nil {
		t.Fatalf("append child message: %v", err)
	}

	bundle, err := exportStore.ExportSessionBundle(ctx, child.ID())
	if err != nil {
		t.Fatalf("export bundle: %v", err)
	}
	if bundle.RootSessionID != child.ID() {
		t.Fatalf("root session = %q, want child %q", bundle.RootSessionID, child.ID())
	}
	if len(bundle.Sessions) != 2 {
		t.Fatalf("bundle sessions = %d, want parent and child", len(bundle.Sessions))
	}
	if bundle.Checksum == "" {
		t.Fatal("bundle checksum is empty")
	}
	rawBundle, err := json.Marshal(bundle)
	if err != nil {
		t.Fatalf("marshal bundle: %v", err)
	}
	var decoded SessionBundle
	if err := json.Unmarshal(rawBundle, &decoded); err != nil {
		t.Fatalf("unmarshal bundle: %v", err)
	}

	importAny, err := NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new import store: %v", err)
	}
	importStore := importAny.(*cantoStore)
	imported, err := importStore.ImportSessionBundle(ctx, decoded)
	if err != nil {
		t.Fatalf("import bundle: %v", err)
	}
	if len(imported) != 2 {
		t.Fatalf("imported sessions = %d, want 2", len(imported))
	}

	tree, err := importStore.SessionTree(ctx, child.ID())
	if err != nil {
		t.Fatalf("imported tree: %v", err)
	}
	if len(tree.Lineage) != 2 ||
		tree.Lineage[0].ID != parent.ID() ||
		tree.Lineage[1].ID != child.ID() ||
		tree.Lineage[1].Title != "try alternate fix" {
		t.Fatalf("imported lineage = %#v, want parent and child", tree.Lineage)
	}
	resumedChild, err := importStore.ResumeSession(ctx, child.ID())
	if err != nil {
		t.Fatalf("resume imported child: %v", err)
	}
	entries, err := resumedChild.Entries(ctx)
	if err != nil {
		t.Fatalf("imported child entries: %v", err)
	}
	if len(entries) != 2 ||
		entries[0].Content != "debug the flaky test" ||
		entries[1].Content != "alternate fix works" {
		t.Fatalf("entries = %#v, want exported transcript", entries)
	}
	if !entries[0].Timestamp.Equal(parentAt) || !entries[1].Timestamp.Equal(childAt) {
		t.Fatalf(
			"imported timestamps = [%s, %s], want [%s, %s]",
			entries[0].Timestamp,
			entries[1].Timestamp,
			parentAt,
			childAt,
		)
	}

	if _, err := importStore.ImportSessionBundle(ctx, decoded); !errors.Is(
		err,
		ErrSessionBundleConflict,
	) {
		t.Fatalf("second import error = %v, want conflict", err)
	}
}

func TestCantoStoreSessionBundleRejectsChecksumMismatch(t *testing.T) {
	storeAny, err := NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "hello",
	})
	bundle, err := store.ExportSessionBundle(ctx, sess.ID())
	if err != nil {
		t.Fatalf("export bundle: %v", err)
	}
	bundle.Sessions[0].Info.Title = "tampered"

	importStoreAny, err := NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new import store: %v", err)
	}
	importStore := importStoreAny.(*cantoStore)
	if _, err := importStore.ImportSessionBundle(ctx, bundle); !errors.Is(
		err,
		ErrSessionBundleIntegrity,
	) {
		t.Fatalf("import error = %v, want integrity error", err)
	}
}

func TestCantoStoreSessionBundleExportsEmptySession(t *testing.T) {
	exportAny, err := NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new export store: %v", err)
	}
	exportStore := exportAny.(*cantoStore)

	ctx := context.Background()
	sess, err := exportStore.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	bundle, err := exportStore.ExportSessionBundle(ctx, sess.ID())
	if err != nil {
		t.Fatalf("export empty session: %v", err)
	}
	if len(bundle.Sessions) != 1 || len(bundle.Sessions[0].Events) != 0 {
		t.Fatalf("empty bundle = %#v, want one zero-event session", bundle.Sessions)
	}

	importAny, err := NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new import store: %v", err)
	}
	importStore := importAny.(*cantoStore)
	if _, err := importStore.ImportSessionBundle(ctx, bundle); err != nil {
		t.Fatalf("import empty session: %v", err)
	}
	if _, err := importStore.ResumeSession(ctx, sess.ID()); err != nil {
		t.Fatalf("resume empty imported session: %v", err)
	}
}

func TestCantoStoreLastStatusIgnoresTransientProgress(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	ctx := context.Background()
	sess, err := storeAny.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	if err := sess.Append(ctx, Status{Status: "Running read..."}); err != nil {
		t.Fatalf("append running status: %v", err)
	}

	got, err := sess.LastStatus(ctx)
	if err != nil {
		t.Fatalf("last status: %v", err)
	}
	if got != "" {
		t.Fatalf("last status = %q, want empty", got)
	}
}

func TestCantoStoreLastStatusClearsRetryAfterTerminalEvent(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	ctx := context.Background()
	sess, err := storeAny.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	status := "Network error. Retrying in 2s... Ctrl+C stops."
	if err := sess.Append(ctx, Status{Status: status}); err != nil {
		t.Fatalf("append retry status: %v", err)
	}
	if err := sess.Append(ctx, System{Content: "Canceled by user"}); err != nil {
		t.Fatalf("append terminal system event: %v", err)
	}

	got, err := sess.LastStatus(ctx)
	if err != nil {
		t.Fatalf("last status: %v", err)
	}
	if got != "" {
		t.Fatalf("last status = %q, want empty", got)
	}
}

func TestCantoStoreLastStatusKeepsRetryStatus(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	ctx := context.Background()
	sess, err := storeAny.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	status := "Network error. Retrying in 2s... Ctrl+C stops."
	if err := sess.Append(ctx, Status{Status: status}); err != nil {
		t.Fatalf("append retry status: %v", err)
	}

	got, err := sess.LastStatus(ctx)
	if err != nil {
		t.Fatalf("last status: %v", err)
	}
	if got != status {
		t.Fatalf("last status = %q, want %q", got, status)
	}
}

func TestCantoStoreListSessionsToleratesNullName(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	cwd := "/tmp/ion-storage-test"
	sess, err := store.OpenSession(ctx, cwd, "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	if _, err := store.db.ExecContext(ctx, "UPDATE session_meta SET name = NULL WHERE id = ?", sess.ID()); err != nil {
		t.Fatalf("null session name: %v", err)
	}

	sessions, err := store.ListSessions(ctx, cwd)
	if err != nil {
		t.Fatalf("list sessions: %v", err)
	}
	if len(sessions) != 1 {
		t.Fatalf("sessions = %d, want 1", len(sessions))
	}
	if sessions[0].Title != "" {
		t.Fatalf("title = %q, want empty", sessions[0].Title)
	}
}

func TestCantoStoreListSessionsIncludesWorkspace(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	cwd := "/tmp/ion-storage-test"
	if _, err := store.OpenSession(ctx, cwd, "model-a", "main"); err != nil {
		t.Fatalf("open session: %v", err)
	}

	sessions, err := store.ListSessions(ctx, cwd)
	if err != nil {
		t.Fatalf("list sessions: %v", err)
	}
	if len(sessions) != 1 {
		t.Fatalf("sessions = %d, want 1", len(sessions))
	}
	if sessions[0].CWD != cwd {
		t.Fatalf("cwd = %q, want %q", sessions[0].CWD, cwd)
	}
}

func TestCantoStoreListSessionsUsesSubsecondMetadataTimestamps(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	cwd := "/tmp/ion-storage-test"
	first, err := store.OpenSession(ctx, cwd, "model-a", "main")
	if err != nil {
		t.Fatalf("open first session: %v", err)
	}
	second, err := store.OpenSession(ctx, cwd, "model-b", "main")
	if err != nil {
		t.Fatalf("open second session: %v", err)
	}

	base := time.Date(2026, 5, 11, 12, 0, 0, 123_000_000, time.UTC)
	if _, err := store.db.ExecContext(
		ctx,
		"UPDATE session_meta SET updated_at = ? WHERE id = ?",
		metadataTimestamp(base),
		first.ID(),
	); err != nil {
		t.Fatalf("update first timestamp: %v", err)
	}
	if _, err := store.db.ExecContext(
		ctx,
		"UPDATE session_meta SET updated_at = ? WHERE id = ?",
		metadataTimestamp(base.Add(time.Millisecond)),
		second.ID(),
	); err != nil {
		t.Fatalf("update second timestamp: %v", err)
	}

	sessions, err := store.ListSessions(ctx, cwd)
	if err != nil {
		t.Fatalf("list sessions: %v", err)
	}
	if len(sessions) != 2 {
		t.Fatalf("sessions = %d, want 2", len(sessions))
	}
	if sessions[0].ID != second.ID() || sessions[1].ID != first.ID() {
		t.Fatalf("session order = %#v, want newest subsecond timestamp first", sessions)
	}
	if !sessions[0].UpdatedAt.Equal(base.Add(time.Millisecond)) {
		t.Fatalf("updated_at = %s, want %s", sessions[0].UpdatedAt, base.Add(time.Millisecond))
	}
}

func TestCantoStoreDecodesLegacySecondMetadataTimestamps(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	cwd := "/tmp/ion-storage-test"
	sess, err := store.OpenSession(ctx, cwd, "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	legacy := time.Date(2024, 2, 3, 4, 5, 6, 0, time.UTC)
	if _, err := store.db.ExecContext(
		ctx,
		"UPDATE session_meta SET created_at = ?, updated_at = ? WHERE id = ?",
		legacy.Unix(),
		legacy.Unix(),
		sess.ID(),
	); err != nil {
		t.Fatalf("write legacy timestamps: %v", err)
	}

	listed, err := store.ListSessions(ctx, cwd)
	if err != nil {
		t.Fatalf("list sessions: %v", err)
	}
	if len(listed) != 1 {
		t.Fatalf("sessions = %d, want 1", len(listed))
	}
	if !listed[0].CreatedAt.Equal(legacy) || !listed[0].UpdatedAt.Equal(legacy) {
		t.Fatalf(
			"listed timestamps = %s/%s, want %s",
			listed[0].CreatedAt,
			listed[0].UpdatedAt,
			legacy,
		)
	}

	resumed, err := store.ResumeSession(ctx, sess.ID())
	if err != nil {
		t.Fatalf("resume session: %v", err)
	}
	defer resumed.Close()
	if !resumed.Meta().CreatedAt.Equal(legacy) {
		t.Fatalf("resumed created_at = %s, want %s", resumed.Meta().CreatedAt, legacy)
	}
}

func TestCantoStoreListSessionsBreaksLegacyTimestampTiesByInsertOrder(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	cwd := "/tmp/ion-storage-test"
	first, err := store.OpenSession(ctx, cwd, "model-a", "main")
	if err != nil {
		t.Fatalf("open first session: %v", err)
	}
	second, err := store.OpenSession(ctx, cwd, "model-b", "main")
	if err != nil {
		t.Fatalf("open second session: %v", err)
	}

	legacy := time.Date(2024, 2, 3, 4, 5, 6, 0, time.UTC).Unix()
	if _, err := store.db.ExecContext(ctx, "UPDATE session_meta SET updated_at = ?", legacy); err != nil {
		t.Fatalf("write legacy timestamps: %v", err)
	}

	sessions, err := store.ListSessions(ctx, cwd)
	if err != nil {
		t.Fatalf("list sessions: %v", err)
	}
	if len(sessions) != 2 {
		t.Fatalf("sessions = %d, want 2", len(sessions))
	}
	if sessions[0].ID != second.ID() || sessions[1].ID != first.ID() {
		t.Fatalf("session order = %#v, want later inserted row first", sessions)
	}
}

func TestLazySessionDoesNotAppearUntilEnsure(t *testing.T) {
	root := t.TempDir()
	store, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	ctx := context.Background()
	cwd := "/tmp/ion-storage-test"
	lazy := NewLazySession(store, cwd, "model-a", "main")
	lazyID := lazy.ID()
	if strings.TrimSpace(lazyID) == "" {
		t.Fatal("lazy session did not allocate an ID")
	}

	recent, err := store.GetRecentSession(ctx, cwd)
	if err != nil {
		t.Fatalf("recent before append: %v", err)
	}
	if recent != nil {
		t.Fatalf("recent before append = %#v, want nil", recent)
	}
	if IsMaterialized(lazy) {
		t.Fatal("lazy session materialized before append")
	}

	if err := lazy.Append(ctx, System{Content: "local notice"}); err != nil {
		t.Fatalf("append: %v", err)
	}
	if IsMaterialized(lazy) {
		t.Fatal("lazy session materialized after display-only append")
	}

	recent, err = store.GetRecentSession(ctx, cwd)
	if err != nil {
		t.Fatalf("recent after display-only append: %v", err)
	}
	if recent != nil {
		t.Fatalf("recent after display-only append = %#v, want nil", recent)
	}

	created, err := lazy.Ensure(ctx)
	if err != nil {
		t.Fatalf("ensure: %v", err)
	}
	if lazy.ID() != lazyID {
		t.Fatalf("lazy ID changed from %q to %q", lazyID, lazy.ID())
	}
	if created.ID() != lazy.ID() {
		t.Fatalf("created ID = %q, want lazy ID %q", created.ID(), lazy.ID())
	}
	if !IsMaterialized(lazy) {
		t.Fatal("lazy session did not materialize after ensure")
	}

	if err := lazy.Append(ctx, System{Content: "local notice after turn"}); err != nil {
		t.Fatalf("append after ensure: %v", err)
	}
	recent, err = store.GetRecentSession(ctx, cwd)
	if err != nil {
		t.Fatalf("recent after ensure: %v", err)
	}
	if recent == nil || recent.ID != lazy.ID() {
		t.Fatalf("recent after ensure = %#v, want %q", recent, lazy.ID())
	}
}

func TestLazySessionSkipsEmptyAgentAppend(t *testing.T) {
	root := t.TempDir()
	store, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	ctx := context.Background()
	cwd := "/tmp/ion-storage-test"
	lazy := NewLazySession(store, cwd, "model-a", "main")
	if err := lazy.Append(ctx, Agent{
		Type:    "agent",
		Content: []Block{},
		TS:      time.Now().Unix(),
	}); err != nil {
		t.Fatalf("append empty agent: %v", err)
	}

	if IsMaterialized(lazy) {
		t.Fatal("lazy session materialized after empty agent append")
	}
	recent, err := store.GetRecentSession(ctx, cwd)
	if err != nil {
		t.Fatalf("recent after empty append: %v", err)
	}
	if recent != nil {
		t.Fatalf("recent after empty append = %#v, want nil", recent)
	}
}

func TestCantoStoreAppendPreservesIonEventTimestamp(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	eventAt := time.Date(2026, 5, 4, 12, 34, 56, 0, time.UTC)
	if err := sess.Append(ctx, System{
		Type:    "system",
		Content: "Paused for review",
		TS:      eventAt.Unix(),
	}); err != nil {
		t.Fatalf("append system: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("entries len = %d, want 1: %#v", len(entries), entries)
	}
	if !entries[0].Timestamp.Equal(eventAt) {
		t.Fatalf("system timestamp = %s, want %s", entries[0].Timestamp, eventAt)
	}
}

func TestCantoStoreAppendReturnsPersistenceErrors(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	workdir := filepath.Join(t.TempDir(), "repo")
	sess, err := store.OpenSession(ctx, workdir, "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	if err := store.db.Close(); err != nil {
		t.Fatalf("close db: %v", err)
	}

	if err := sess.Append(ctx, Status{Status: "Network error. Retrying in 2s... Ctrl+C stops."}); err == nil {
		t.Fatal("expected append to return an error when session metadata update fails")
	}
}

func TestCantoStoreEntriesMapToolMessages(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	workdir := filepath.Join(t.TempDir(), "repo")
	sess, err := store.OpenSession(ctx, workdir, "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	cantoSess, err := store.canto.Load(ctx, sess.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}

	userAt := time.Date(2026, 5, 2, 10, 0, 0, 0, time.UTC)
	agentAt := userAt.Add(time.Minute)
	toolAt := agentAt.Add(time.Minute)
	if err := cantoSess.Append(ctx, withCantoTimestamp(csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:    llm.RoleUser,
		Content: "hello",
	}), userAt)); err != nil {
		t.Fatalf("append user: %v", err)
	}
	if err := cantoSess.Append(ctx, withCantoTimestamp(csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:      llm.RoleAssistant,
		Content:   "hi there",
		Reasoning: "reasoning",
		Calls:     []llm.Call{assistantToolCall("tool-bash", "bash")},
	}), agentAt)); err != nil {
		t.Fatalf("append agent: %v", err)
	}
	if err := cantoSess.Append(ctx, withCantoTimestamp(csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:    llm.RoleTool,
		ToolID:  "tool-bash",
		Name:    "bash",
		Content: "tool output",
	}), toolAt)); err != nil {
		t.Fatalf("append tool: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 3 {
		t.Fatalf("entries length = %d, want 3", len(entries))
	}
	if entries[0].Role != ionsession.User || entries[0].Content != "hello" {
		t.Fatalf("user entry = %#v", entries[0])
	}
	if !entries[0].Timestamp.Equal(userAt) {
		t.Fatalf("user timestamp = %s, want %s", entries[0].Timestamp, userAt)
	}
	if entries[1].Role != ionsession.Agent || entries[1].Content != "hi there" ||
		entries[1].Reasoning != "reasoning" {
		t.Fatalf("agent entry = %#v", entries[1])
	}
	if !entries[1].Timestamp.Equal(agentAt) {
		t.Fatalf("agent timestamp = %s, want %s", entries[1].Timestamp, agentAt)
	}
	if entries[2].Role != ionsession.Tool || entries[2].Title != "Bash" ||
		entries[2].Content != "tool output" {
		t.Fatalf("tool entry = %#v", entries[2])
	}
	if !entries[2].Timestamp.Equal(toolAt) {
		t.Fatalf("tool timestamp = %s, want %s", entries[2].Timestamp, toolAt)
	}
}

func TestCantoStoreDisplayReplaySharesProviderHistorySource(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, filepath.Join(t.TempDir(), "repo"), "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "first",
	})
	if err := sess.Append(ctx, TokenUsage{Input: 4, Output: 1, Cost: 0.01}); err != nil {
		t.Fatalf("append first usage: %v", err)
	}
	if err := sess.Append(ctx, Status{
		Status: "Network error. Retrying in 2s... Ctrl+C stops.",
	}); err != nil {
		t.Fatalf("append retry status: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("seed entries: %v", err)
	}
	if len(entries) != 1 || entries[0].Content != "first" {
		t.Fatalf("seed entries = %#v, want first message", entries)
	}
	input, output, cost, err := sess.Usage(ctx)
	if err != nil {
		t.Fatalf("seed usage: %v", err)
	}
	if input != 4 || output != 1 || cost != 0.01 {
		t.Fatalf("seed usage = %d/%d/%f, want 4/1/0.01", input, output, cost)
	}
	status, err := sess.LastStatus(ctx)
	if err != nil {
		t.Fatalf("seed status: %v", err)
	}
	if status == "" {
		t.Fatal("seed status is empty, want retry status")
	}

	if _, err := store.db.ExecContext(
		ctx,
		"UPDATE events SET data = ? WHERE session_id = ? AND seq = 1",
		[]byte("{not-json"),
		sess.ID(),
	); err != nil {
		t.Fatalf("corrupt projected prefix: %v", err)
	}
	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:    llm.RoleAssistant,
		Content: "second",
	})
	if err := sess.Append(ctx, TokenUsage{Input: 6, Output: 2, Cost: 0.02}); err != nil {
		t.Fatalf("append second usage: %v", err)
	}

	_, err = sess.Entries(ctx)
	if err == nil {
		t.Fatal("incremental entries succeeded from stale projection cache after provider history corruption")
	}
	if !strings.Contains(err.Error(), "effective history") {
		t.Fatalf("incremental entries error = %v, want effective history decode error", err)
	}
	input, output, cost, err = sess.Usage(ctx)
	if err != nil {
		t.Fatalf("incremental usage: %v", err)
	}
	if input != 10 || output != 3 || cost != 0.03 {
		t.Fatalf("incremental usage = %d/%d/%f, want 10/3/0.03", input, output, cost)
	}
	status, err = sess.LastStatus(ctx)
	if err != nil {
		t.Fatalf("incremental status: %v", err)
	}
	if status != "" {
		t.Fatalf("incremental status = %q, want cleared by new message", status)
	}
}

func TestCantoStoreEntriesApplyIncrementalContext(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, filepath.Join(t.TempDir(), "repo"), "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "seed",
	})
	if _, err := sess.Entries(ctx); err != nil {
		t.Fatalf("seed entries: %v", err)
	}

	cantoSess, err := store.canto.Load(ctx, sess.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}
	if err := cantoSess.Append(ctx, csession.NewContext(sess.ID(), csession.ContextEntry{
		Kind:    csession.ContextKindSummary,
		Content: "summary context",
	})); err != nil {
		t.Fatalf("append context: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 2 {
		t.Fatalf("entries length = %d, want 2: %#v", len(entries), entries)
	}
	if entries[1].Role != ionsession.System || entries[1].Content != "summary context" {
		t.Fatalf("context entry = %#v", entries[1])
	}
}

func TestCantoStoreEntriesPreserveIncrementalToolLifecycleDisplay(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	workdir := filepath.Join(t.TempDir(), "repo")
	sess, err := store.OpenSession(ctx, workdir, "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "seed",
	})
	if _, err := sess.Entries(ctx); err != nil {
		t.Fatalf("seed entries: %v", err)
	}

	cantoSess, err := store.canto.Load(ctx, sess.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}
	if err := cantoSess.Append(ctx, csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:  llm.RoleAssistant,
		Calls: []llm.Call{assistantToolCall("tool-read", "read")},
	})); err != nil {
		t.Fatalf("append read call: %v", err)
	}
	if err := cantoSess.Append(ctx, csession.NewToolStartedEvent(sess.ID(), csession.ToolStartedData{
		Tool:      "read",
		ID:        "tool-read",
		Arguments: `{"file_path":` + strconv.Quote(filepath.Join(workdir, "AGENTS.md")) + `}`,
	})); err != nil {
		t.Fatalf("append read start: %v", err)
	}
	if err := cantoSess.Append(ctx, csession.NewToolCompletedEvent(sess.ID(), csession.ToolCompletedData{
		Tool:   "read",
		ID:     "tool-read",
		Output: "tool output",
	})); err != nil {
		t.Fatalf("append read completion: %v", err)
	}
	if err := cantoSess.Append(ctx, csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:    llm.RoleTool,
		ToolID:  "tool-read",
		Name:    "read",
		Content: "tool output",
	})); err != nil {
		t.Fatalf("append read message: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 2 {
		t.Fatalf("entries length = %d, want 2: %#v", len(entries), entries)
	}
	if entries[1].Role != ionsession.Tool ||
		entries[1].Title != "Read(AGENTS.md)" ||
		entries[1].Content != "tool output" {
		t.Fatalf("incremental tool entry = %#v", entries[1])
	}
}

func TestCantoStoreEntriesRecoverIncrementalToolCompletion(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "seed",
	})
	if _, err := sess.Entries(ctx); err != nil {
		t.Fatalf("seed entries: %v", err)
	}

	cantoSess, err := store.canto.Load(ctx, sess.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}
	if err := cantoSess.Append(ctx, csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:  llm.RoleAssistant,
		Calls: []llm.Call{assistantToolCall("tool-read", "read")},
	})); err != nil {
		t.Fatalf("append read call: %v", err)
	}
	if err := cantoSess.Append(ctx, csession.NewToolStartedEvent(sess.ID(), csession.ToolStartedData{
		Tool:      "read",
		ID:        "tool-read",
		Arguments: `{"file_path":"AGENTS.md"}`,
	})); err != nil {
		t.Fatalf("append read start: %v", err)
	}
	completedAt := time.Date(2026, 5, 2, 12, 0, 0, 0, time.UTC)
	if err := cantoSess.Append(ctx, withCantoTimestamp(csession.NewToolCompletedEvent(sess.ID(), csession.ToolCompletedData{
		Tool:   "read",
		ID:     "tool-read",
		Output: "recovered contents",
	}), completedAt)); err != nil {
		t.Fatalf("append read completion: %v", err)
	}
	if err := cantoSess.Append(ctx, csession.NewTurnCompletedEvent(sess.ID(), csession.TurnCompletedData{})); err != nil {
		t.Fatalf("append turn completion: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 2 {
		t.Fatalf("entries length = %d, want 2: %#v", len(entries), entries)
	}
	if entries[1].Role != ionsession.Tool ||
		entries[1].Title != "Read(AGENTS.md)" ||
		entries[1].Content != "recovered contents" {
		t.Fatalf("recovered incremental tool entry = %#v", entries[1])
	}
	if !entries[1].Timestamp.Equal(completedAt) {
		t.Fatalf("recovered tool timestamp = %s, want %s", entries[1].Timestamp, completedAt)
	}
}

func TestCantoStoreEntriesDoNotDuplicateRecoveredToolMessage(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "seed",
	})

	cantoSess, err := store.canto.Load(ctx, sess.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}
	if err := cantoSess.Append(ctx, csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:  llm.RoleAssistant,
		Calls: []llm.Call{assistantToolCall("tool-read", "read")},
	})); err != nil {
		t.Fatalf("append read call: %v", err)
	}
	if err := cantoSess.Append(ctx, csession.NewToolStartedEvent(sess.ID(), csession.ToolStartedData{
		Tool:      "read",
		ID:        "tool-read",
		Arguments: `{"file_path":"AGENTS.md"}`,
	})); err != nil {
		t.Fatalf("append read start: %v", err)
	}
	if err := cantoSess.Append(ctx, csession.NewToolCompletedEvent(sess.ID(), csession.ToolCompletedData{
		Tool:   "read",
		ID:     "tool-read",
		Output: "recovered contents",
	})); err != nil {
		t.Fatalf("append read completion: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("seed projection entries: %v", err)
	}
	if toolEntryCount(entries) != 1 {
		t.Fatalf("seed entries = %#v, want one recovered tool entry", entries)
	}

	if err := cantoSess.Append(ctx, csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:    llm.RoleTool,
		ToolID:  "tool-read",
		Name:    "read",
		Content: "recovered contents",
	})); err != nil {
		t.Fatalf("append provider-visible tool message: %v", err)
	}

	entries, err = sess.Entries(ctx)
	if err != nil {
		t.Fatalf("incremental entries: %v", err)
	}
	if toolEntryCount(entries) != 1 {
		t.Fatalf(
			"incremental entries = %#v, want one tool entry after matching tool message",
			entries,
		)
	}
}

func toolEntryCount(entries []ionsession.Entry) int {
	count := 0
	for _, entry := range entries {
		if entry.Role == ionsession.Tool {
			count++
		}
	}
	return count
}

func BenchmarkCantoStoreEntriesEffectiveReplay(b *testing.B) {
	root := b.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		b.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, filepath.Join(b.TempDir(), "repo"), "model-a", "main")
	if err != nil {
		b.Fatalf("open session: %v", err)
	}
	for i := range 2_000 {
		appendCantoMessage(b, store, ctx, sess.ID(), llm.Message{
			Role:    llm.RoleUser,
			Content: "message " + strconv.Itoa(i),
		})
	}
	if _, err := sess.Entries(ctx); err != nil {
		b.Fatalf("seed entries: %v", err)
	}

	b.ResetTimer()
	for b.Loop() {
		entries, err := sess.Entries(ctx)
		if err != nil {
			b.Fatalf("entries: %v", err)
		}
		if len(entries) != 2_000 {
			b.Fatalf("entries = %d, want 2000", len(entries))
		}
	}
}

func BenchmarkCantoStoreListSessionsLargeWorkspace(b *testing.B) {
	root := b.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		b.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	cwd := filepath.Join(b.TempDir(), "repo")
	for i := range 1_200 {
		id := "bench-session-" + strconv.Itoa(i)
		if _, err := store.OpenSessionWithID(ctx, id, cwd, "model-a", "main"); err != nil {
			b.Fatalf("open session %d: %v", i, err)
		}
		if err := store.UpdateSession(ctx, SessionInfo{
			ID:          id,
			Title:       "feature investigation " + strconv.Itoa(i),
			LastPreview: "last preview message " + strconv.Itoa(i),
		}); err != nil {
			b.Fatalf("update session %d: %v", i, err)
		}
	}
	b.ReportMetric(1_200, "sessions/op")

	b.ResetTimer()
	for b.Loop() {
		sessions, err := store.ListSessions(ctx, cwd)
		if err != nil {
			b.Fatalf("list sessions: %v", err)
		}
		if len(sessions) != 1_200 {
			b.Fatalf("sessions = %d, want 1200", len(sessions))
		}
	}
}

func TestCantoStoreEntriesPreserveRoutineToolOutput(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	workdir := filepath.Join(t.TempDir(), "repo")
	sess, err := store.OpenSession(ctx, workdir, "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	cantoSess, err := store.canto.Load(ctx, sess.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}

	if err := cantoSess.Append(ctx, csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:    llm.RoleUser,
		Content: "hello",
	})); err != nil {
		t.Fatalf("append user: %v", err)
	}
	if err := cantoSess.Append(ctx, csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:  llm.RoleAssistant,
		Calls: []llm.Call{assistantToolCall("tool-read", "read")},
	})); err != nil {
		t.Fatalf("append read call: %v", err)
	}
	if err := cantoSess.Append(ctx, csession.NewToolStartedEvent(sess.ID(), csession.ToolStartedData{
		Tool:      "read",
		ID:        "tool-read",
		Arguments: `{"file_path":` + strconv.Quote(filepath.Join(workdir, "AGENTS.md")) + `}`,
	})); err != nil {
		t.Fatalf("append read start: %v", err)
	}
	if err := cantoSess.Append(ctx, csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:    llm.RoleTool,
		ToolID:  "tool-read",
		Name:    "read",
		Content: strings.Join([]string{"line 1", "line 2", "line 3"}, "\n"),
	})); err != nil {
		t.Fatalf("append read: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 2 {
		t.Fatalf("entries length = %d, want 2: %#v", len(entries), entries)
	}
	if entries[0].Role != ionsession.User || entries[0].Content != "hello" {
		t.Fatalf("user entry = %#v", entries[0])
	}
	wantContent := strings.Join([]string{"line 1", "line 2", "line 3"}, "\n")
	if entries[1].Role != ionsession.Tool || entries[1].Title != "Read(AGENTS.md)" ||
		entries[1].Content != wantContent {
		t.Fatalf("read entry = %#v", entries[1])
	}
}

func TestCantoStoreEntriesRecoverToolResultFromLifecycle(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	cantoSess, err := store.canto.Load(ctx, sess.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}

	call := assistantToolCall("tool-read", "read")
	call.Function.Arguments = `{"file_path":"AGENTS.md"}`
	if err := cantoSess.Append(ctx, csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:  llm.RoleAssistant,
		Calls: []llm.Call{call},
	})); err != nil {
		t.Fatalf("append read call: %v", err)
	}
	if err := cantoSess.Append(ctx, csession.NewToolStartedEvent(sess.ID(), csession.ToolStartedData{
		Tool:      "read",
		ID:        "tool-read",
		Arguments: `{"file_path":"AGENTS.md"}`,
	})); err != nil {
		t.Fatalf("append read start: %v", err)
	}
	completedAt := time.Date(2026, 5, 2, 12, 0, 0, 0, time.UTC)
	if err := cantoSess.Append(ctx, withCantoTimestamp(csession.NewToolCompletedEvent(sess.ID(), csession.ToolCompletedData{
		Tool:   "read",
		ID:     "tool-read",
		Output: "recovered contents",
	}), completedAt)); err != nil {
		t.Fatalf("append read completion: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("entries length = %d, want 1: %#v", len(entries), entries)
	}
	if entries[0].Role != ionsession.Tool ||
		entries[0].Title != "Read(AGENTS.md)" ||
		entries[0].Content != "recovered contents" {
		t.Fatalf("recovered tool entry = %#v", entries[0])
	}
	if !entries[0].Timestamp.Equal(completedAt) {
		t.Fatalf("recovered tool timestamp = %s, want %s", entries[0].Timestamp, completedAt)
	}
}

func TestCantoStoreEntriesDropDanglingToolCall(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	appendLegacyCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:  llm.RoleAssistant,
		Calls: []llm.Call{assistantToolCall("tool-missing", "read")},
	})
	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "next turn",
	})

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("entries length = %d, want 1: %#v", len(entries), entries)
	}
	if entries[0].Role != ionsession.User || entries[0].Content != "next turn" {
		t.Fatalf("remaining entry = %#v", entries[0])
	}
}

func TestCantoStoreAppendSkipsEmptyAgentMessages(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	if err := sess.Append(ctx, Agent{
		Type:    "agent",
		Content: []Block{},
		TS:      time.Now().Unix(),
	}); err != nil {
		t.Fatalf("append empty agent: %v", err)
	}

	cantoSess, err := store.canto.Load(ctx, sess.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}
	for _, ev := range cantoSess.Events() {
		if ev.Type != csession.MessageAdded {
			continue
		}
		var msg llm.Message
		if err := ev.UnmarshalData(&msg); err != nil {
			t.Fatalf("unmarshal message: %v", err)
		}
		if msg.Role == llm.RoleAssistant {
			t.Fatalf("empty assistant message was appended: %#v", msg)
		}
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 0 {
		t.Fatalf("entries = %#v, want none", entries)
	}
}

func TestCantoStoreEntriesPreserveReasoningOnlyAgentMessages(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := storeAny.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	reasoning := "thinking through it"
	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:      llm.RoleAssistant,
		Reasoning: reasoning,
	})

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("entries length = %d, want 1", len(entries))
	}
	if entries[0].Role != ionsession.Agent || entries[0].Content != "" ||
		entries[0].Reasoning != reasoning {
		t.Fatalf("reasoning-only agent entry = %#v", entries[0])
	}
}

func TestCantoStoreEntriesDropEmptyAgentMessages(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	cantoSess, err := store.canto.Load(ctx, sess.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}

	appendMessage := func(role llm.Role, content string) {
		t.Helper()
		if err := cantoSess.Append(ctx, csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
			Role:    role,
			Content: content,
		})); err != nil {
			t.Fatalf("append %s message: %v", role, err)
		}
	}
	appendMessage(llm.RoleUser, "first")
	appendLegacyCantoMessage(t, store, ctx, sess.ID(), llm.Message{Role: llm.RoleAssistant})
	appendMessage(llm.RoleAssistant, "same")

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 2 {
		t.Fatalf("entries length = %d, want 2: %#v", len(entries), entries)
	}
	if entries[1].Role != ionsession.Agent || entries[1].Content != "same" {
		t.Fatalf("agent entry = %#v", entries[1])
	}
}

func TestCantoStoreEntriesMapSystemMessages(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	if err := sess.Append(ctx, System{Content: "Error: backend unavailable"}); err != nil {
		t.Fatalf("append system: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("entries length = %d, want 1", len(entries))
	}
	if entries[0].Role != ionsession.System || entries[0].Content != "Error: backend unavailable" {
		t.Fatalf("system entry = %#v", entries[0])
	}
}

func TestCantoStoreEntriesInterleaveSystemMessagesWithHistory(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := storeAny.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "first turn",
	})
	if err := sess.Append(ctx, System{Content: "Canceled by user"}); err != nil {
		t.Fatalf("append system: %v", err)
	}
	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "second turn",
	})

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 3 {
		t.Fatalf("entries length = %d, want 3", len(entries))
	}
	if entries[0].Role != ionsession.User || entries[0].Content != "first turn" {
		t.Fatalf("first entry = %#v", entries[0])
	}
	if entries[1].Role != ionsession.System || entries[1].Content != "Canceled by user" {
		t.Fatalf("second entry = %#v", entries[1])
	}
	if entries[2].Role != ionsession.User || entries[2].Content != "second turn" {
		t.Fatalf("third entry = %#v", entries[2])
	}
}

func TestCantoStoreRejectsModelVisibleAppends(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	ctx := context.Background()
	sess, err := storeAny.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	content := "agent"
	cases := []any{
		User{Type: "user", Content: "user"},
		Agent{Type: "agent", Content: []Block{{Type: "text", Text: &content}}},
		ToolUse{Type: "tool_use", ID: "tool-123", Name: "bash"},
		ToolResult{Type: "tool_result", ToolUseID: "tool-123", Content: "ok"},
	}
	for _, event := range cases {
		err := sess.Append(ctx, event)
		if err == nil {
			t.Fatalf("append %T returned nil, want model-visible error", event)
		}
		if !strings.Contains(err.Error(), "cannot append model-visible") {
			t.Fatalf("append %T error = %q, want model-visible error", event, err)
		}
	}
}

func TestCantoStoreEntriesPreserveFullAgentContent(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := storeAny.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	content := strings.Repeat("full assistant content ", 12)
	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:    llm.RoleAssistant,
		Content: content,
	})

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("entries length = %d, want 1", len(entries))
	}
	if entries[0].Role != ionsession.Agent {
		t.Fatalf("agent entry role = %q, want %q", entries[0].Role, ionsession.Agent)
	}
	if entries[0].Content != content {
		t.Fatalf("agent content = %q, want full content %q", entries[0].Content, content)
	}
}

func TestCantoStoreEntriesPreserveToolResultErrors(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := storeAny.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:  llm.RoleAssistant,
		Calls: []llm.Call{assistantToolCall("tool-err", "bash")},
	})
	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:    llm.RoleTool,
		ToolID:  "tool-err",
		Name:    "bash",
		Content: "exit status 1",
	})
	if err := store.canto.Save(ctx, csession.NewToolCompletedEvent(sess.ID(), csession.ToolCompletedData{
		Tool:   "bash",
		ID:     "tool-err",
		Output: "exit status 1",
		Error:  "exit status 1",
	})); err != nil {
		t.Fatalf("save tool completed: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("entries length = %d, want 1", len(entries))
	}
	if entries[0].Role != ionsession.Tool {
		t.Fatalf("tool entry role = %q, want %q", entries[0].Role, ionsession.Tool)
	}
	if !entries[0].IsError {
		t.Fatal("tool entry IsError = false, want true")
	}
}

func TestCantoStoreEntriesDoNotCompactRoutineToolErrors(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := storeAny.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	fullError := "permission denied\nError: exit status 1"
	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:  llm.RoleAssistant,
		Calls: []llm.Call{assistantToolCall("tool-list-error", "list")},
	})
	appendCantoMessage(t, store, ctx, sess.ID(), llm.Message{
		Role:    llm.RoleTool,
		ToolID:  "tool-list-error",
		Name:    "list",
		Content: fullError,
	})
	if err := store.canto.Save(ctx, csession.NewToolCompletedEvent(sess.ID(), csession.ToolCompletedData{
		Tool:   "list",
		ID:     "tool-list-error",
		Output: fullError,
		Error:  fullError,
	})); err != nil {
		t.Fatalf("save tool completed: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("entries length = %d, want 1", len(entries))
	}
	if entries[0].Content != fullError {
		t.Fatalf("tool error content = %q, want full error", entries[0].Content)
	}
	if !entries[0].IsError {
		t.Fatal("tool entry IsError = false, want true")
	}
}

func TestCantoStoreEntriesPreserveCantoToolCompletedErrors(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	if err := store.canto.Save(ctx, csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:  llm.RoleAssistant,
		Calls: []llm.Call{assistantToolCall("tool-err", "bash")},
	})); err != nil {
		t.Fatalf("save assistant tool call: %v", err)
	}
	if err := store.canto.Save(ctx, csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:    llm.RoleTool,
		ToolID:  "tool-err",
		Name:    "bash",
		Content: "exit status 1",
	})); err != nil {
		t.Fatalf("save tool message: %v", err)
	}
	if err := store.canto.Save(ctx, csession.NewToolCompletedEvent(sess.ID(), csession.ToolCompletedData{
		Tool:   "bash",
		ID:     "tool-err",
		Output: "exit status 1",
		Error:  "exit status 1",
	})); err != nil {
		t.Fatalf("save tool completed: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("entries length = %d, want 1", len(entries))
	}
	if entries[0].Role != ionsession.Tool {
		t.Fatalf("tool entry role = %q, want %q", entries[0].Role, ionsession.Tool)
	}
	if !entries[0].IsError {
		t.Fatal("tool entry IsError = false, want true")
	}
}

func TestCantoStoreEntriesUseEffectiveHistoryAfterCompaction(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	cantoSess, err := store.canto.Load(ctx, sess.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}

	userEvent := csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:    llm.RoleUser,
		Content: "old question",
	})
	if err := cantoSess.Append(ctx, userEvent); err != nil {
		t.Fatalf("append user: %v", err)
	}
	agentEvent := csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:    llm.RoleAssistant,
		Content: "old answer",
	})
	if err := cantoSess.Append(ctx, agentEvent); err != nil {
		t.Fatalf("append agent: %v", err)
	}
	recentAt := time.Date(2026, 5, 2, 13, 0, 0, 0, time.UTC)
	recentEvent := withCantoTimestamp(
		csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
			Role:    llm.RoleAssistant,
			Content: "recent answer",
		}),
		recentAt,
	)
	if err := cantoSess.Append(ctx, recentEvent); err != nil {
		t.Fatalf("append recent agent: %v", err)
	}

	snapshot := csession.CompactionSnapshot{
		Strategy:      "summarize",
		CutoffEventID: recentEvent.ID.String(),
		Entries: []csession.HistoryEntry{
			{
				Message: llm.Message{
					Role:    llm.RoleSystem,
					Content: "<conversation_summary>\nsummary\n</conversation_summary>",
				},
			},
			{
				EventID: recentEvent.ID.String(),
				Message: llm.Message{Role: llm.RoleAssistant, Content: "recent answer"},
			},
		},
	}
	if err := cantoSess.Append(ctx, csession.NewCompactionEvent(sess.ID(), snapshot)); err != nil {
		t.Fatalf("append compaction: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 2 {
		t.Fatalf("entries length = %d, want 2", len(entries))
	}
	if entries[0].Role != ionsession.System ||
		entries[0].Content != "<conversation_summary>\nsummary\n</conversation_summary>" {
		t.Fatalf("summary entry = %#v", entries[0])
	}
	if entries[1].Role != ionsession.Agent || entries[1].Content != "recent answer" {
		t.Fatalf("recent entry = %#v", entries[1])
	}
	if !entries[1].Timestamp.Equal(recentAt) {
		t.Fatalf("recent entry timestamp = %s, want %s", entries[1].Timestamp, recentAt)
	}
}

func TestCantoStoreEntriesDropDisplayOnlyEventsBeforeCompactionCutoff(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	cantoSess, err := store.canto.Load(ctx, sess.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}
	if err := cantoSess.Append(ctx, csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:    llm.RoleUser,
		Content: "old question",
	})); err != nil {
		t.Fatalf("append old user: %v", err)
	}
	if err := sess.Append(ctx, System{
		Type:    "system",
		Content: "old display marker",
		TS:      time.Now().Unix(),
	}); err != nil {
		t.Fatalf("append old display event: %v", err)
	}
	recentEvent := csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:    llm.RoleAssistant,
		Content: "recent answer",
	})
	if err := cantoSess.Append(ctx, recentEvent); err != nil {
		t.Fatalf("append recent agent: %v", err)
	}

	snapshot := csession.CompactionSnapshot{
		Strategy:      "summarize",
		CutoffEventID: recentEvent.ID.String(),
		Entries: []csession.HistoryEntry{
			{
				Message: llm.Message{
					Role:    llm.RoleSystem,
					Content: "<conversation_summary>\nsummary\n</conversation_summary>",
				},
			},
			{
				EventID: recentEvent.ID.String(),
				Message: llm.Message{Role: llm.RoleAssistant, Content: "recent answer"},
			},
		},
	}
	if err := cantoSess.Append(ctx, csession.NewCompactionEvent(sess.ID(), snapshot)); err != nil {
		t.Fatalf("append compaction: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	for _, entry := range entries {
		if entry.Role == ionsession.System &&
			strings.Contains(entry.Content, "old display marker") {
			t.Fatalf("compacted entries retained display-only event before cutoff: %#v", entries)
		}
	}
	if len(entries) != 2 {
		t.Fatalf("entries length = %d, want 2: %#v", len(entries), entries)
	}
}

func TestCantoStoreEntriesPreserveDisplayOnlyEventsAfterCompactionCutoff(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := t.Context()
	sess, err := store.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	cantoSess, err := store.canto.Load(ctx, sess.ID())
	if err != nil {
		t.Fatalf("load canto session: %v", err)
	}
	recentEvent := csession.NewEvent(sess.ID(), csession.MessageAdded, llm.Message{
		Role:    llm.RoleAssistant,
		Content: "recent answer",
	})
	if err := cantoSess.Append(ctx, recentEvent); err != nil {
		t.Fatalf("append recent agent: %v", err)
	}
	snapshot := csession.CompactionSnapshot{
		Strategy:      "summarize",
		CutoffEventID: recentEvent.ID.String(),
		Entries: []csession.HistoryEntry{
			{
				Message: llm.Message{
					Role:    llm.RoleSystem,
					Content: "<conversation_summary>\nsummary\n</conversation_summary>",
				},
			},
			{
				EventID: recentEvent.ID.String(),
				Message: llm.Message{Role: llm.RoleAssistant, Content: "recent answer"},
			},
		},
	}
	if err := cantoSess.Append(ctx, csession.NewCompactionEvent(sess.ID(), snapshot)); err != nil {
		t.Fatalf("append compaction: %v", err)
	}
	afterAt := time.Date(2026, 5, 2, 14, 0, 0, 0, time.UTC)
	if err := sess.Append(ctx, System{
		Type:    "system",
		Content: "fresh display marker",
		TS:      afterAt.Unix(),
	}); err != nil {
		t.Fatalf("append fresh display event: %v", err)
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 3 {
		t.Fatalf("entries length = %d, want 3: %#v", len(entries), entries)
	}
	if entries[2].Role != ionsession.System ||
		entries[2].Content != "fresh display marker" ||
		!entries[2].Timestamp.Equal(afterAt) {
		t.Fatalf("fresh display entry = %#v, want marker at %s", entries[2], afterAt)
	}
}
