package agent

import (
	"context"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
	"golang.org/x/sys/unix"
)

type recordingProcessReconciler struct {
	result tool.ProcessRecoveryResult
	seen   []string
}

type cancelingProcessReconciler struct {
	started chan struct{}
}

func (r *cancelingProcessReconciler) ReconcileProcess(ctx context.Context, _ string) (tool.ProcessRecoveryResult, error) {
	close(r.started)
	<-ctx.Done()
	return tool.ProcessRecoveryResult{Status: tool.ProcessRecoveryTerminated, Detail: "cleanup completed after caller cancellation"}, nil
}

func (r *recordingProcessReconciler) ReconcileProcess(_ context.Context, identity string) (tool.ProcessRecoveryResult, error) {
	r.seen = append(r.seen, identity)
	return r.result, nil
}

func TestControllerProcessRecoveryPersistsCleanupWithoutResolvingAction(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	record := session.ActionRecord{
		ID:           "recovery-action",
		InvocationID: "recovery-call",
		SessionID:    "recovery-session",
		TurnID:       "recovery-turn",
		Tool:         "bash",
		Operation:    "run",
		Arguments:    []byte(`{"command":"sleep 30"}`),
		Fingerprint:  "recovery-fingerprint",
	}
	if _, err := store.PrepareAction(ctx, record); err != nil {
		t.Fatal(err)
	}
	if _, err := store.AuthorizeAction(ctx, record.ID, "confirm"); err != nil {
		t.Fatal(err)
	}
	if _, err := store.StartAction(ctx, record.ID, "recorded-process"); err != nil {
		t.Fatal(err)
	}
	reconciler := &recordingProcessReconciler{result: tool.ProcessRecoveryResult{
		Status: tool.ProcessRecoveryTerminated,
		Detail: "matching group terminated",
	}}
	controller := NewController(ControllerConfig{
		Session:           session.NewSession(store, 64),
		Store:             store,
		ActionJournal:     store,
		ProcessReconciler: reconciler,
	})
	t.Cleanup(func() { _ = controller.Close() })

	if err := controller.RecoverProcessActions(ctx); err != nil {
		t.Fatal(err)
	}
	action, err := store.GetAction(ctx, record.ID)
	if err != nil {
		t.Fatal(err)
	}
	if action.State != session.ActionIndeterminate {
		t.Fatalf("recovered action state = %s, want indeterminate", action.State)
	}
	if !strings.Contains(action.Error, "restart process recovery: matching group terminated") || !strings.Contains(action.CleanupOutcome, "restart process recovery status: terminated") {
		t.Fatalf("recovery evidence = %#v", action)
	}
	if len(reconciler.seen) != 1 || reconciler.seen[0] != "recorded-process" {
		t.Fatalf("reconciler calls = %#v", reconciler.seen)
	}

	transitions, err := store.ActionTransitions(ctx, record.ID)
	if err != nil {
		t.Fatal(err)
	}
	if len(transitions) != 5 {
		t.Fatalf("transition count = %d, want 5: %#v", len(transitions), transitions)
	}
}

func TestControllerRestartRecoveryTerminatesRecordedOrphanGroup(t *testing.T) {
	ctx := context.Background()
	dbPath := filepath.Join(t.TempDir(), "ion.db")
	process := exec.Command("/bin/sh", "-c", "sleep 30")
	process.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := process.Start(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if process.ProcessState != nil {
			return
		}
		_ = unix.Kill(-process.Process.Pid, unix.SIGKILL)
		_, _ = process.Process.Wait()
	})
	identity, err := tool.CaptureProcessIdentity(process.Process.Pid)
	if err != nil {
		t.Fatal(err)
	}

	store, err := session.NewSQLiteStore(dbPath, "restart-session")
	if err != nil {
		t.Fatal(err)
	}
	record := session.ActionRecord{
		ID:              "orphan-action",
		InvocationID:    "orphan-call",
		SessionID:       "restart-session",
		TurnID:          "orphan-turn",
		Tool:            "bash",
		Operation:       "run",
		Arguments:       []byte(`{"command":"sleep 30"}`),
		Fingerprint:     "orphan-fingerprint",
		ProcessIdentity: identity,
	}
	if _, err := store.PrepareAction(ctx, record); err != nil {
		store.Close()
		t.Fatal(err)
	}
	if _, err := store.AuthorizeAction(ctx, record.ID, "confirm"); err != nil {
		store.Close()
		t.Fatal(err)
	}
	if _, err := store.StartAction(ctx, record.ID, identity); err != nil {
		store.Close()
		t.Fatal(err)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}

	reopened, err := session.NewSQLiteStore(dbPath, "restart-session")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = reopened.Close() })
	reconciler := tool.NewProcessReconciler()
	controller := NewController(ControllerConfig{
		Session:           session.NewSession(reopened, 64),
		Store:             reopened,
		ActionJournal:     reopened,
		ProcessReconciler: reconciler,
	})
	if err := controller.RecoverProcessActions(ctx); err != nil {
		controller.Close()
		t.Fatal(err)
	}
	if err := controller.Close(); err != nil {
		t.Fatal(err)
	}
	_, _ = process.Process.Wait()
	action, err := reopened.GetAction(ctx, record.ID)
	if err != nil {
		t.Fatal(err)
	}
	if action.State != session.ActionIndeterminate {
		t.Fatalf("restart-recovered action state = %s, want indeterminate", action.State)
	}
	if !strings.Contains(action.CleanupOutcome, "terminated") {
		t.Fatalf("restart cleanup evidence = %#v", action)
	}
}

func TestControllerRecoveryPersistsAfterCallerCancellation(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	store := newTestStore(t)
	record := session.ActionRecord{
		ID:              "cancel-recovery-action",
		InvocationID:    "cancel-recovery-call",
		SessionID:       "cancel-recovery-session",
		TurnID:          "cancel-recovery-turn",
		Tool:            "bash",
		Operation:       "run",
		Arguments:       []byte(`{"command":"sleep 30"}`),
		Fingerprint:     "cancel-recovery-fingerprint",
		ProcessIdentity: "opaque-process",
	}
	if _, err := store.PrepareAction(ctx, record); err != nil {
		t.Fatal(err)
	}
	if _, err := store.AuthorizeAction(ctx, record.ID, "confirm"); err != nil {
		t.Fatal(err)
	}
	if _, err := store.StartAction(ctx, record.ID, record.ProcessIdentity); err != nil {
		t.Fatal(err)
	}
	if _, err := store.FinishAction(ctx, record.ID, session.ActionIndeterminate, "", "unknown effect", "cleanup pending"); err != nil {
		t.Fatal(err)
	}
	reconciler := &cancelingProcessReconciler{started: make(chan struct{})}
	controller := NewController(ControllerConfig{
		Session:           session.NewSession(store, 64),
		Store:             store,
		ActionJournal:     store,
		ProcessReconciler: reconciler,
	})
	t.Cleanup(func() { _ = controller.Close() })

	done := make(chan error, 1)
	go func() { done <- controller.RecoverProcessActions(ctx) }()
	select {
	case <-reconciler.started:
		cancel()
	case <-time.After(2 * time.Second):
		t.Fatal("process recovery did not start")
	}
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("canceled process recovery did not return")
	}
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		action, err := store.GetAction(context.Background(), record.ID)
		if err != nil {
			t.Fatal(err)
		}
		if strings.Contains(action.Error, "cleanup completed after caller cancellation") {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	action, _ := store.GetAction(context.Background(), record.ID)
	t.Fatalf("canceled recovery did not persist evidence: %#v", action)
}
