package main

import (
	"bytes"
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/app"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
)

func TestRuntimeLoggerDoesNotInterleaveWithInteractiveTUI(t *testing.T) {
	var interactive bytes.Buffer
	runtimeLogger(true, &interactive).Info("hidden from terminal")
	if interactive.Len() != 0 {
		t.Fatalf("interactive logger wrote %q, want no terminal output", interactive.String())
	}

	var print bytes.Buffer
	runtimeLogger(false, &print).Info("print diagnostic")
	if print.Len() == 0 {
		t.Fatal("print logger discarded diagnostic output")
	}
}

func TestCloseRuntimeResourcesAfterErrorPreservesBothFailures(t *testing.T) {
	openErr := errors.New("open failed")
	closeErr := errors.New("close failed")

	got := closeRuntimeResourcesAfterError(openErr, func() error { return closeErr })
	if !errors.Is(got, openErr) {
		t.Fatalf("combined error = %v, want original error", got)
	}
	if !errors.Is(got, closeErr) {
		t.Fatalf("combined error = %v, want cleanup error", got)
	}
	if !strings.Contains(got.Error(), "close runtime resources after failed setup") {
		t.Fatalf("combined error = %v, want cleanup context", got)
	}
}

func TestCloseRuntimeResourcesAfterErrorReturnsOpenErrorWhenCleanupSucceeds(t *testing.T) {
	openErr := errors.New("open failed")
	got := closeRuntimeResourcesAfterError(openErr, func() error { return nil })
	if got != openErr {
		t.Fatalf("combined error = %v, want original error", got)
	}
}

func TestOpenRuntimeReturnsActionableProviderError(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	t.Setenv("OPENAI_API_KEY", "")
	store, err := session.NewSQLiteStore(":memory:", "ion")
	if err != nil {
		t.Fatalf("new sqlite store: %v", err)
	}
	defer store.Close()

	b, sess, runner, err := openRuntime(
		context.Background(),
		store,
		nil,
		"/tmp/ion-test",
		"main",
		&config.Config{Provider: "openai", Model: "gpt-4.1"},
		llm.NewEndpointResolver(llm.EndpointResolverOptions{}),
		"target-session",
		false,
		"",
		"",
		"",
		nil,
	)
	if err == nil || !strings.Contains(err.Error(), "OPENAI_API_KEY not set") {
		t.Fatalf("openRuntime error = %v, want actionable credential error", err)
	}
	if b == nil || b.Name() != "setup" {
		t.Fatalf("runtime info = %#v, want setup runtime", b)
	}
	if sess != nil || runner != nil {
		t.Fatalf("incomplete runtime handles = (%v, %v), want nil", sess, runner)
	}
	if status := b.Bootstrap().Status; status != "OPENAI_API_KEY not set" {
		t.Fatalf("setup status = %q, want original provider error", status)
	}
	if leaf := store.GetLeafID(); leaf != "" {
		t.Fatalf("failed provider initialization moved store leaf to %q", leaf)
	}
}

func TestOpenRuntimeStartsFreshWithoutSelectedSession(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	store, err := session.NewSQLiteStore(":memory:", "ion")
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()
	sess := session.NewSession(store, 8)
	oldID, err := sess.AppendMessage(context.Background(), session.NewUserText("old", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	if err := store.SetLeafID(oldID); err != nil {
		t.Fatal(err)
	}

	jobs := tool.NewJobManager()
	defer jobs.Close()
	_, _, runner, err := openRuntime(
		context.Background(), store, jobs, t.TempDir(), "main",
		&config.Config{Provider: "ollama", Model: "llama3"},
		llm.NewEndpointResolver(llm.EndpointResolverOptions{}), "", false,
		"", "", "", nil, false,
	)
	if err != nil {
		t.Fatalf("openRuntime: %v", err)
	}
	defer func() {
		if err := closeRuntimeHandles(runner, store); err != nil {
			t.Fatalf("close runtime: %v", err)
		}
	}()
	if got := store.GetLeafID(); got != "" {
		t.Fatalf("fresh runtime leaf = %q, want virtual root", got)
	}
}

func TestOpenRuntimeUncommittedActivationRollsBackOnClose(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	store, err := session.NewSQLiteStore(":memory:", "ion")
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()
	sess := session.NewSession(store, 8)
	oldID, err := sess.AppendMessage(context.Background(), session.NewUserText("old", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	if err := store.SetLeafID(oldID); err != nil {
		t.Fatal(err)
	}
	previousIdentity := store.Meta().ID

	jobs := tool.NewJobManager()
	defer jobs.Close()
	_, _, runner, err := openRuntime(
		context.Background(), store, jobs, t.TempDir(), "main",
		&config.Config{Provider: "ollama", Model: "llama3"},
		llm.NewEndpointResolver(llm.EndpointResolverOptions{}), "", false,
		"", "", "", nil, false,
	)
	if err != nil {
		t.Fatalf("openRuntime: %v", err)
	}
	if got := store.GetLeafID(); got != "" {
		t.Fatalf("staged fresh leaf = %q, want virtual root", got)
	}
	if err := runner.Close(); err != nil {
		t.Fatalf("close uncommitted runtime: %v", err)
	}
	if got := store.GetLeafID(); got != oldID {
		t.Fatalf("rolled-back leaf = %q, want %q", got, oldID)
	}
	if got := store.Meta().ID; got != previousIdentity {
		t.Fatalf("rolled-back identity = %q, want %q", got, previousIdentity)
	}
}

func TestOpenRuntimeResumesStableIdentityBeforeCatalogPublication(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	store, err := session.NewSQLiteStore(":memory:", "ion")
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()
	sess := session.NewSession(store, 8)
	leafID, err := sess.AppendMessage(context.Background(), session.NewUserText("durable", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	if err := store.SetLeafID(leafID); err != nil {
		t.Fatal(err)
	}
	stableID := store.Meta().ID

	jobs := tool.NewJobManager()
	defer jobs.Close()
	_, _, runner, err := openRuntime(
		context.Background(), store, jobs, t.TempDir(), "main",
		&config.Config{Provider: "ollama", Model: "llama3"},
		llm.NewEndpointResolver(llm.EndpointResolverOptions{}), stableID, false,
		"", "", "", nil, false,
	)
	if err != nil {
		t.Fatalf("openRuntime: %v", err)
	}
	commitSessionActivation(runner)
	defer func() {
		if err := closeRuntimeHandles(runner, store); err != nil {
			t.Fatalf("close runtime: %v", err)
		}
	}()
	if got := store.GetLeafID(); got != leafID {
		t.Fatalf("pre-catalog stable resume leaf = %q, want %q", got, leafID)
	}
	if got := runtimeSessionID(runner); got != stableID {
		t.Fatalf("pre-catalog stable resume ID = %q, want %q", got, stableID)
	}
}

func TestOpenRuntimeCommittedActivationSurvivesClose(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	store, err := session.NewSQLiteStore(":memory:", "ion")
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()
	sess := session.NewSession(store, 8)
	targetID, err := sess.AppendMessage(context.Background(), session.NewUserText("target", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	previousIdentity := store.Meta().ID

	jobs := tool.NewJobManager()
	defer jobs.Close()
	_, _, runner, err := openRuntime(
		context.Background(), store, jobs, t.TempDir(), "main",
		&config.Config{Provider: "ollama", Model: "llama3"},
		llm.NewEndpointResolver(llm.EndpointResolverOptions{}), targetID, false,
		"", "", "", nil, false,
	)
	if err != nil {
		t.Fatalf("openRuntime: %v", err)
	}
	commitSessionActivation(runner)
	if err := runner.Close(); err != nil {
		t.Fatalf("close committed runtime: %v", err)
	}
	if got := store.GetLeafID(); got != targetID {
		t.Fatalf("committed leaf = %q, want %q", got, targetID)
	}
	if got := store.Meta().ID; got == previousIdentity {
		t.Fatalf("committed identity = %q, want a new selected identity", got)
	}
}

func TestOpenRuntimePersistsExplicitModelOverrideOnResume(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	store, err := session.NewSQLiteStore(":memory:", "ion")
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()
	sess := session.NewSession(store, 8)
	if _, err := sess.AppendModelChange(context.Background(), "ollama", "old-model"); err != nil {
		t.Fatal(err)
	}
	targetID, err := sess.AppendMessage(context.Background(), session.NewUserText("resume", time.Now()))
	if err != nil {
		t.Fatal(err)
	}

	jobs := tool.NewJobManager()
	defer jobs.Close()
	_, _, runner, err := openRuntime(
		context.Background(), store, jobs, t.TempDir(), "main",
		&config.Config{Provider: "ollama", Model: "llama3"},
		llm.NewEndpointResolver(llm.EndpointResolverOptions{}), targetID, true,
		"", "", "", nil, false,
	)
	if err != nil {
		t.Fatalf("openRuntime: %v", err)
	}
	defer func() {
		if err := closeRuntimeHandles(runner, store); err != nil {
			t.Fatalf("close runtime: %v", err)
		}
	}()
	resumedLeaf := store.GetLeafID()
	if resumedLeaf == targetID || resumedLeaf == "" {
		t.Fatalf("resumed leaf = %q, want model override entry after %q", resumedLeaf, targetID)
	}
	branch, err := store.Branch(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	last, ok := branch[len(branch)-1].(*session.ModelChangeEntry)
	if !ok || last.ParentID() != targetID {
		t.Fatalf(
			"resumed branch tail = %T parent %q, want model change parent %q",
			branch[len(branch)-1],
			branch[len(branch)-1].ParentID(),
			targetID,
		)
	}
	replayed, err := sess.BuildContext(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if replayed.ActiveProvider != "ollama" || replayed.ActiveModel != "llama3" {
		t.Fatalf("resumed model = %s/%s, want ollama/llama3", replayed.ActiveProvider, replayed.ActiveModel)
	}
}

func TestOpenRuntimeDoesNotMoveLeafWhenMaterializationFails(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	store, err := session.NewSQLiteStore(":memory:", "ion")
	if err != nil {
		t.Fatalf("new sqlite store: %v", err)
	}
	defer store.Close()

	sess := session.NewSession(store, 8)
	oldID, err := sess.AppendMessage(context.Background(), session.NewUserText("old", time.Now()))
	if err != nil {
		t.Fatalf("append old entry: %v", err)
	}
	targetID, err := sess.AppendMessage(context.Background(), session.NewUserText("target", time.Now()))
	if err != nil {
		t.Fatalf("append target entry: %v", err)
	}
	if err := store.SetLeafID(oldID); err != nil {
		t.Fatalf("restore old leaf: %v", err)
	}

	promptDir := filepath.Join(t.TempDir(), "prompt")
	if err := os.Mkdir(promptDir, 0o755); err != nil {
		t.Fatalf("mkdir prompt path: %v", err)
	}
	_, _, _, err = openRuntime(
		context.Background(),
		store,
		nil,
		"/tmp/ion-test",
		"main",
		&config.Config{Provider: "ollama", Model: "llama3"},
		llm.NewEndpointResolver(llm.EndpointResolverOptions{}),
		targetID,
		false,
		promptDir,
		"",
		"",
		nil,
	)
	if err == nil || !strings.Contains(err.Error(), "build system prompt") {
		t.Fatalf("openRuntime error = %v, want system prompt failure", err)
	}
	if leaf := store.GetLeafID(); leaf != oldID {
		t.Fatalf("failed materialization moved leaf to %q, want %q", leaf, oldID)
	}
}

func TestStartupSetupRequiredRecognizesSetupBackend(t *testing.T) {
	if !startupSetupRequired(app.NewSetupRuntime(&config.Config{Provider: "openai"}, "missing")) {
		t.Fatal("setup backend should require startup setup")
	}
	if startupSetupRequired(providerRuntimeInfo{provider: "openai"}) {
		t.Fatal("materialized backend should not require startup setup")
	}
}

func TestRuntimeInfoBootstrapSurfacesUnsettledActions(t *testing.T) {
	store, err := session.NewSQLiteStore(":memory:", "ion")
	if err != nil {
		t.Fatalf("new sqlite store: %v", err)
	}
	defer store.Close()

	info, err := runtimeInfoForProvider("ollama", &config.Config{Provider: "ollama", Model: "llama3"})
	if err != nil {
		t.Fatalf("runtime info: %v", err)
	}
	native, ok := info.(*runtimeInfo)
	if !ok {
		t.Fatalf("runtime info type = %T, want *runtimeInfo", info)
	}
	native.recovery = []session.ActionRecord{{
		ID:    "action-1",
		Tool:  "bash",
		State: session.ActionIndeterminate,
	}}
	native.interruptedTurns = []session.TurnRecord{{
		ID: "turn-1", State: session.TurnInterrupted, Input: "draft after restart",
	}}

	boot := info.Bootstrap()
	if !strings.Contains(boot.Status, "1 unsettled external action(s); use /actions to inspect") {
		t.Fatalf("bootstrap status = %q, want recovery warning", boot.Status)
	}
	if !strings.Contains(boot.Status, "1 interrupted turn(s) retained; use /turns to inspect") {
		t.Fatalf("bootstrap status = %q, want interrupted-turn warning", boot.Status)
	}
	if len(boot.Recovery) != 1 || boot.Recovery[0].ID != "action-1" {
		t.Fatalf("bootstrap recovery = %#v, want copied action", boot.Recovery)
	}
	if len(boot.InterruptedTurns) != 1 || boot.InterruptedTurns[0].ID != "turn-1" {
		t.Fatalf("bootstrap interrupted turns = %#v, want copied turn", boot.InterruptedTurns)
	}
	boot.Recovery[0].ID = "mutated"
	if native.recovery[0].ID != "action-1" {
		t.Fatal("bootstrap recovery aliases runtime state")
	}
	boot.InterruptedTurns[0].ID = "mutated"
	if native.interruptedTurns[0].ID != "turn-1" {
		t.Fatal("bootstrap interrupted turns aliases runtime state")
	}
}

func TestOpenRuntimeFailsClosedForPrintModeWithUnsettledActionWithoutMovingLeaf(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	store, err := session.NewSQLiteStore(":memory:", "ion")
	if err != nil {
		t.Fatalf("new sqlite store: %v", err)
	}
	defer store.Close()

	sess := session.NewSession(store, 8)
	oldID, err := sess.AppendMessage(context.Background(), session.NewUserText("old", time.Now()))
	if err != nil {
		t.Fatalf("append old entry: %v", err)
	}
	targetID, err := sess.AppendMessage(context.Background(), session.NewUserText("target", time.Now()))
	if err != nil {
		t.Fatalf("append target entry: %v", err)
	}
	if err := store.SetLeafID(oldID); err != nil {
		t.Fatalf("restore old leaf: %v", err)
	}

	record := session.ActionRecord{
		ID:           "action-1",
		InvocationID: "tool-call-1",
		SessionID:    "session-1",
		TurnID:       "turn-1",
		Tool:         "bash",
		Operation:    "run",
		Arguments:    []byte(`{"command":"go test ./..."}`),
		Metadata:     []byte(`{"source":"test"}`),
		Preimages:    []byte(`[]`),
		Fingerprint:  "sha256:action-1",
		CWD:          t.TempDir(),
		PolicyMode:   "confirm",
	}
	var journal session.ActionJournal = store
	if _, err := journal.PrepareAction(context.Background(), record); err != nil {
		t.Fatalf("prepare action: %v", err)
	}
	if _, err := journal.AuthorizeAction(context.Background(), record.ID, record.PolicyMode); err != nil {
		t.Fatalf("authorize action: %v", err)
	}
	if _, err := journal.StartAction(context.Background(), record.ID, "test-process-group"); err != nil {
		t.Fatalf("start action: %v", err)
	}

	jobs := tool.NewJobManager()
	defer jobs.Close()
	_, sess, runner, err := openRuntime(
		context.Background(),
		store,
		jobs,
		t.TempDir(),
		"main",
		&config.Config{Provider: "ollama", Model: "llama3"},
		llm.NewEndpointResolver(llm.EndpointResolverOptions{}),
		targetID,
		false,
		"",
		"",
		"",
		nil,
		false,
	)
	if err == nil || !strings.Contains(err.Error(), "unsettled external action") {
		t.Fatalf("openRuntime error = %v, want print-mode recovery error", err)
	}
	if sess != nil || runner != nil {
		t.Fatalf("failed runtime handles = (%v, %v), want nil", sess, runner)
	}
	if leaf := store.GetLeafID(); leaf != oldID {
		t.Fatalf("failed runtime replacement moved leaf to %q, want %q", leaf, oldID)
	}
}

func TestOpenRuntimeFailsClosedForPrintModeWithInterruptedTurn(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	path := filepath.Join(t.TempDir(), "ion.db")
	initial, err := session.NewSQLiteStore(path, "ion")
	if err != nil {
		t.Fatalf("open initial store: %v", err)
	}
	if _, err := initial.BeginTurn(
		context.Background(),
		"interrupted-turn",
		"draft after restart",
		nil,
		"context-1",
	); err != nil {
		t.Fatalf("begin interrupted turn: %v", err)
	}
	if err := initial.Close(); err != nil {
		t.Fatalf("close initial store: %v", err)
	}
	store, err := session.NewSQLiteStore(path, "ion")
	if err != nil {
		t.Fatalf("reopen store: %v", err)
	}
	defer store.Close()

	jobs := tool.NewJobManager()
	defer jobs.Close()
	_, sess, runner, err := openRuntime(
		context.Background(),
		store,
		jobs,
		t.TempDir(),
		"main",
		&config.Config{Provider: "ollama", Model: "llama3"},
		llm.NewEndpointResolver(llm.EndpointResolverOptions{}),
		"",
		false,
		"",
		"",
		"",
		nil,
		false,
	)
	if err == nil || !strings.Contains(err.Error(), "interrupted turn") {
		t.Fatalf("openRuntime error = %v, want print-mode interrupted-turn recovery error", err)
	}
	if sess != nil || runner != nil {
		t.Fatalf("failed runtime handles = (%v, %v), want nil", sess, runner)
	}
}
