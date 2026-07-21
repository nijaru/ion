package main

import (
	"bytes"
	"context"
	"encoding/json"
	"strings"
	"testing"

	"github.com/nijaru/ion/session"
)

func seedCLIUnsettledAction(t *testing.T, dir string) string {
	t.Helper()
	store, err := session.NewSQLiteStore(dir, "ion")
	if err != nil {
		t.Fatalf("new store: %v", err)
	}
	record := session.ActionRecord{
		ID:            "action-cli-1",
		InvocationID:  "call-cli-1",
		SessionID:     "session-cli-1",
		TurnID:        "turn-cli-1",
		Tool:          "bash",
		Category:      "execute",
		Operation:     "bash",
		Arguments:     []byte(`{"command":"echo secret"}`),
		Metadata:      []byte(`{"sandbox":"denied-by-sandbox"}`),
		Preimages:     []byte(`[]`),
		Fingerprint:   "sha256:cli-action",
		CWD:           dir,
		NetworkIntent: "denied-by-sandbox",
		PolicyMode:    "confirm",
	}
	journal := session.ActionJournal(store)
	ctx := context.Background()
	if _, err := journal.PrepareAction(ctx, record); err != nil {
		t.Fatalf("prepare: %v", err)
	}
	if _, err := journal.AuthorizeAction(ctx, record.ID, record.PolicyMode); err != nil {
		t.Fatalf("authorize: %v", err)
	}
	if _, err := journal.StartAction(ctx, record.ID, "cli-process-group"); err != nil {
		t.Fatalf("start: %v", err)
	}
	if err := store.Close(); err != nil {
		t.Fatalf("close seed store: %v", err)
	}
	return record.ID
}

func TestActionCLIListsRedactedRecoveryJSON(t *testing.T) {
	dir := t.TempDir()
	actionID := seedCLIUnsettledAction(t, dir)
	var stdout, stderr bytes.Buffer
	handled, code := runTopLevelCommand(
		[]string{"actions", "--json", "--session-dir", dir, "list"}, &stdout, &stderr,
	)
	if !handled || code != 0 {
		t.Fatalf("handled/code = %v/%d, stderr=%q", handled, code, stderr.String())
	}
	var result struct {
		Actions []actionCommandView `json:"actions"`
	}
	if err := json.Unmarshal(stdout.Bytes(), &result); err != nil {
		t.Fatalf("decode JSON %q: %v", stdout.String(), err)
	}
	if len(result.Actions) != 1 || result.Actions[0].ID != actionID || result.Actions[0].State != session.ActionIndeterminate {
		t.Fatalf("actions = %#v, want one indeterminate action", result.Actions)
	}
	if strings.Contains(stdout.String(), "echo secret") || strings.Contains(stdout.String(), "arguments") {
		t.Fatalf("recovery JSON leaked raw action payload: %q", stdout.String())
	}
}

func TestActionCLIReconcilesThroughRecoveryController(t *testing.T) {
	dir := t.TempDir()
	actionID := seedCLIUnsettledAction(t, dir)
	var stdout, stderr bytes.Buffer
	handled, code := runTopLevelCommand(
		[]string{"actions", "--json", "--session-dir", dir, "reconcile", actionID, "completed", "operator verified the external result"},
		&stdout, &stderr,
	)
	if !handled || code != 0 {
		t.Fatalf("handled/code = %v/%d, stderr=%q", handled, code, stderr.String())
	}
	var result struct {
		Action actionCommandView `json:"action"`
	}
	if err := json.Unmarshal(stdout.Bytes(), &result); err != nil {
		t.Fatalf("decode JSON %q: %v", stdout.String(), err)
	}
	if result.Action.ID != actionID || result.Action.State != session.ActionCompleted {
		t.Fatalf("reconciled action = %#v, want completed", result.Action)
	}

	store, err := session.NewSQLiteStore(dir, "ion")
	if err != nil {
		t.Fatalf("reopen store: %v", err)
	}
	defer store.Close()
	action, err := store.GetAction(context.Background(), actionID)
	if err != nil {
		t.Fatalf("get reconciled action: %v", err)
	}
	if action.State != session.ActionCompleted || !strings.Contains(action.Error, "operator verified") {
		t.Fatalf("stored action = %#v, want durable verified completion", action)
	}
}

func TestActionCLIJSONErrorsStayOnStdout(t *testing.T) {
	var stdout, stderr bytes.Buffer
	handled, code := runTopLevelCommand(
		[]string{"actions", "--json", "reconcile", "missing", "completed", "verified"}, &stdout, &stderr,
	)
	if !handled || code != 1 {
		t.Fatalf("handled/code = %v/%d, want handled failure", handled, code)
	}
	if stderr.Len() != 0 {
		t.Fatalf("stderr = %q, want clean JSON mode", stderr.String())
	}
	var result struct {
		Error string `json:"error"`
	}
	if err := json.Unmarshal(stdout.Bytes(), &result); err != nil {
		t.Fatalf("decode JSON error %q: %v", stdout.String(), err)
	}
	if result.Error == "" {
		t.Fatalf("JSON error = %#v, want diagnostic", result)
	}
}
