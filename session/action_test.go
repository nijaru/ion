package session

import (
	"context"
	"errors"
	"path/filepath"
	"slices"
	"strings"
	"testing"
)

func testActionRecord() ActionRecord {
	return ActionRecord{
		ID:            "action-1",
		InvocationID:  "tool-call-1",
		SessionID:     "session-1",
		TurnID:        "turn-1",
		Tool:          "bash",
		Operation:     "run",
		Arguments:     []byte(`{"command":"go test ./..."}`),
		Metadata:      []byte(`{"source":"test","revision":1}`),
		Preimages:     []byte(`[{"path":"/workspace/project","exists":true,"size":4}]`),
		Fingerprint:   "sha256:action-1",
		CWD:           "/workspace/project",
		Paths:         []string{"/workspace/project"},
		Environment:   []string{"PATH"},
		NetworkIntent: "none",
		MCPIdentity:   "",
		PolicyMode:    "confirm",
	}
}

func TestActionJournalLifecycleIsDurableAndIdempotent(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	record := testActionRecord()

	prepared, err := store.PrepareAction(ctx, record)
	if err != nil {
		t.Fatalf("prepare: %v", err)
	}
	if prepared.State != ActionPrepared || prepared.PreparedAt.IsZero() {
		t.Fatalf("prepared record = %#v", prepared)
	}
	prepared.Paths[0] = "/mutated-by-caller"
	repeated, err := store.PrepareAction(ctx, record)
	if err != nil {
		t.Fatalf("repeat prepare: %v", err)
	}
	if repeated.State != ActionPrepared || repeated.Paths[0] != "/workspace/project" {
		t.Fatalf("repeat prepare = %#v", repeated)
	}

	authorized, err := store.AuthorizeAction(ctx, record.ID, "confirm")
	if err != nil {
		t.Fatalf("authorize: %v", err)
	}
	if authorized.State != ActionAuthorized || authorized.Authorization != ActionAllow {
		t.Fatalf("authorized record = %#v", authorized)
	}
	started, err := store.StartAction(ctx, record.ID, "pg-1")
	if err != nil {
		t.Fatalf("start: %v", err)
	}
	if started.State != ActionStarted || started.ProcessGroupID != "pg-1" {
		t.Fatalf("started record = %#v", started)
	}
	completed, err := store.FinishAction(ctx, record.ID, ActionCompleted, "result-1", "", "clean")
	if err != nil {
		t.Fatalf("finish: %v", err)
	}
	if completed.State != ActionCompleted || completed.ResultIdentity != "result-1" {
		t.Fatalf("completed record = %#v", completed)
	}
	repeatedFinish, err := store.FinishAction(ctx, record.ID, ActionCompleted, "result-1", "", "clean")
	if err != nil {
		t.Fatalf("repeat finish: %v", err)
	}
	if repeatedFinish.State != ActionCompleted {
		t.Fatalf("repeat finish = %#v", repeatedFinish)
	}
	if unsettled, err := store.UnsettledActions(ctx); err != nil {
		t.Fatalf("unsettled: %v", err)
	} else if len(unsettled) != 0 {
		t.Fatalf("unsettled actions = %#v", unsettled)
	}

	got, err := store.GetAction(ctx, record.ID)
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if !slices.Equal(got.Paths, record.Paths) || !slices.Equal(got.Environment, record.Environment) {
		t.Fatalf("round-trip lists changed: %#v", got)
	}
	if string(got.Arguments) != string(record.Arguments) || string(got.Metadata) != string(record.Metadata) || string(got.Preimages) != string(record.Preimages) {
		t.Fatalf("round-trip payloads = arguments %s metadata %s preimages %s, want arguments %s metadata %s preimages %s", got.Arguments, got.Metadata, got.Preimages, record.Arguments, record.Metadata, record.Preimages)
	}
	transitions, err := store.ActionTransitions(ctx, record.ID)
	if err != nil {
		t.Fatalf("transitions: %v", err)
	}
	if len(transitions) != 4 {
		t.Fatalf("transition count = %d, want 4: %#v", len(transitions), transitions)
	}
	wantStates := []ActionState{ActionPrepared, ActionAuthorized, ActionStarted, ActionCompleted}
	for i, want := range wantStates {
		if transitions[i].To != want {
			t.Fatalf("transition %d = %#v, want %s", i, transitions[i], want)
		}
	}
}

func TestActionJournalRejectsIdentityChangesAndIllegalTransitions(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	record := testActionRecord()
	if _, err := store.PrepareAction(ctx, record); err != nil {
		t.Fatal(err)
	}
	changed := record
	changed.Arguments = []byte(`{"command":"rm -rf /"}`)
	if _, err := store.PrepareAction(ctx, changed); !errors.Is(err, ErrActionConflict) {
		t.Fatalf("changed prepare error = %v, want ErrActionConflict", err)
	}
	changed = record
	changed.Metadata = []byte(`{"source":"changed"}`)
	if _, err := store.PrepareAction(ctx, changed); !errors.Is(err, ErrActionConflict) {
		t.Fatalf("changed metadata prepare error = %v, want ErrActionConflict", err)
	}
	changed = record
	changed.Preimages = []byte(`[{"path":"/workspace/project","exists":false}]`)
	if _, err := store.PrepareAction(ctx, changed); !errors.Is(err, ErrActionConflict) {
		t.Fatalf("changed preimages prepare error = %v, want ErrActionConflict", err)
	}
	if _, err := store.StartAction(ctx, record.ID, ""); !errors.Is(err, ErrActionState) {
		t.Fatalf("start before authorize error = %v, want ErrActionState", err)
	}
	if _, err := store.FinishAction(ctx, record.ID, ActionCompleted, "", "", ""); !errors.Is(err, ErrActionState) {
		t.Fatalf("complete before start error = %v, want ErrActionState", err)
	}
	denied, err := store.DenyAction(ctx, record.ID, "approval denied")
	if err != nil {
		t.Fatalf("deny: %v", err)
	}
	if denied.State != ActionDenied || denied.Authorization != ActionDeny {
		t.Fatalf("denied record = %#v", denied)
	}
	if _, err := store.AuthorizeAction(ctx, record.ID, "confirm"); !errors.Is(err, ErrActionState) {
		t.Fatalf("authorize denied action error = %v, want ErrActionState", err)
	}
}

func TestActionJournalRejectsWrongStructuredPayloadKinds(t *testing.T) {
	ctx := context.Background()
	for name, mutate := range map[string]func(*ActionRecord){
		"metadata":  func(record *ActionRecord) { record.Metadata = []byte(`[]`) },
		"preimages": func(record *ActionRecord) { record.Preimages = []byte(`{}`) },
	} {
		t.Run(name, func(t *testing.T) {
			record := testActionRecord()
			mutate(&record)
			if _, err := newTestStore(t).PrepareAction(ctx, record); err == nil {
				t.Fatalf("PrepareAction accepted invalid %s payload", name)
			}
		})
	}
}

func TestActionJournalRecoversStartedActionAsIndeterminate(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	dbPath := filepath.Join(dir, "ion.db")
	store, err := NewSQLiteStore(dbPath, "session-1")
	if err != nil {
		t.Fatal(err)
	}
	record := testActionRecord()
	if _, err := store.PrepareAction(ctx, record); err != nil {
		t.Fatal(err)
	}
	if _, err := store.AuthorizeAction(ctx, record.ID, "confirm"); err != nil {
		t.Fatal(err)
	}
	if _, err := store.StartAction(ctx, record.ID, "pg-1"); err != nil {
		t.Fatal(err)
	}
	prepared := testActionRecord()
	prepared.ID, prepared.InvocationID, prepared.TurnID, prepared.Fingerprint = "action-prepared", "call-prepared", "turn-prepared", "sha256:prepared"
	if _, err := store.PrepareAction(ctx, prepared); err != nil {
		t.Fatal(err)
	}
	authorized := testActionRecord()
	authorized.ID, authorized.InvocationID, authorized.TurnID, authorized.Fingerprint = "action-authorized", "call-authorized", "turn-authorized", "sha256:authorized"
	if _, err := store.PrepareAction(ctx, authorized); err != nil {
		t.Fatal(err)
	}
	if _, err := store.AuthorizeAction(ctx, authorized.ID, "confirm"); err != nil {
		t.Fatal(err)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}

	reopened, err := NewSQLiteStore(dbPath, "session-1")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { reopened.Close() })
	got, err := reopened.GetAction(ctx, record.ID)
	if err != nil {
		t.Fatal(err)
	}
	if got.State != ActionIndeterminate {
		t.Fatalf("recovered state = %s, want %s", got.State, ActionIndeterminate)
	}
	for _, tc := range []struct {
		id    string
		state ActionState
	}{
		{prepared.ID, ActionCancelled},
		{authorized.ID, ActionCancelled},
	} {
		recovered, err := reopened.GetAction(ctx, tc.id)
		if err != nil {
			t.Fatal(err)
		}
		if recovered.State != tc.state {
			t.Fatalf("recovered %s state = %s, want %s", tc.id, recovered.State, tc.state)
		}
	}
	if got.Error == "" || got.EndedAt.IsZero() {
		t.Fatalf("recovered record lacks uncertainty evidence: %#v", got)
	}
	if unsettled, err := reopened.UnsettledActions(ctx); err != nil {
		t.Fatal(err)
	} else if len(unsettled) != 1 || unsettled[0].ID != record.ID || unsettled[0].State != ActionIndeterminate {
		t.Fatalf("recovered action is not exposed for reconciliation: %#v", unsettled)
	}
}

func TestActionJournalReconcilesIndeterminateOnlyWithEvidence(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	record := testActionRecord()
	if _, err := store.PrepareAction(ctx, record); err != nil {
		t.Fatal(err)
	}
	if _, err := store.AuthorizeAction(ctx, record.ID, "confirm"); err != nil {
		t.Fatal(err)
	}
	if _, err := store.StartAction(ctx, record.ID, "pg-1"); err != nil {
		t.Fatal(err)
	}
	if _, err := store.FinishAction(ctx, record.ID, ActionIndeterminate, "", "unknown", ""); err != nil {
		t.Fatal(err)
	}
	if _, err := store.ReconcileAction(ctx, record.ID, ActionCompleted, "verified result digest", "result-2", "verified", "clean"); err != nil {
		t.Fatal(err)
	}
	got, err := store.GetAction(ctx, record.ID)
	if err != nil {
		t.Fatal(err)
	}
	if got.State != ActionCompleted || !strings.Contains(got.Error, "verified result digest") {
		t.Fatalf("reconciled record = %#v", got)
	}
	if _, err := store.ReconcileAction(ctx, record.ID, ActionFailed, "", "", "", ""); err == nil {
		t.Fatal("reconciliation without evidence unexpectedly succeeded")
	}
}

func TestActionJournalRejectsCancellationAfterStart(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	record := testActionRecord()
	if _, err := store.PrepareAction(ctx, record); err != nil {
		t.Fatal(err)
	}
	if _, err := store.AuthorizeAction(ctx, record.ID, "confirm"); err != nil {
		t.Fatal(err)
	}
	if _, err := store.StartAction(ctx, record.ID, "pg-1"); err != nil {
		t.Fatal(err)
	}
	if _, err := store.FinishAction(ctx, record.ID, ActionCancelled, "", "user canceled", ""); !errors.Is(err, ErrActionState) {
		t.Fatalf("cancel after start error = %v, want ErrActionState", err)
	}
	if _, err := store.FinishAction(ctx, record.ID, ActionIndeterminate, "", "cancellation crossed start boundary", ""); err != nil {
		t.Fatalf("indeterminate after start: %v", err)
	}
}
