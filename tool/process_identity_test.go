//go:build darwin || linux

package tool

import (
	"context"
	"errors"
	"os/exec"
	"syscall"
	"testing"

	"golang.org/x/sys/unix"
)

func startTestProcessGroup(t *testing.T) *exec.Cmd {
	t.Helper()
	cmd := exec.Command("/bin/sh", "-c", "sleep 30")
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if cmd.ProcessState != nil {
			return
		}
		_ = unix.Kill(-cmd.Process.Pid, unix.SIGKILL)
		_, _ = cmd.Process.Wait()
	})
	return cmd
}

func TestProcessIdentityRoundTripAndReconcilesMatchingGroup(t *testing.T) {
	cmd := startTestProcessGroup(t)
	encoded, err := CaptureProcessIdentity(cmd.Process.Pid)
	if err != nil {
		t.Fatal(err)
	}
	identity, err := DecodeProcessIdentity(encoded)
	if err != nil {
		t.Fatal(err)
	}
	if identity.PID != cmd.Process.Pid || identity.PGID != cmd.Process.Pid || identity.StartToken == "" {
		t.Fatalf("captured identity = %#v", identity)
	}

	result, err := NewProcessReconciler().ReconcileProcess(context.Background(), encoded)
	if err != nil {
		t.Fatal(err)
	}
	if result.Status != ProcessRecoveryTerminated {
		t.Fatalf("recovery result = %#v, want terminated", result)
	}
	_, _ = cmd.Process.Wait()
}

func TestProcessReconcilerRefusesChangedIdentity(t *testing.T) {
	cmd := startTestProcessGroup(t)
	encoded, err := CaptureProcessIdentity(cmd.Process.Pid)
	if err != nil {
		t.Fatal(err)
	}
	identity, err := DecodeProcessIdentity(encoded)
	if err != nil {
		t.Fatal(err)
	}
	identity.StartToken = "not-the-recorded-process"
	changed, err := EncodeProcessIdentity(identity)
	if err != nil {
		t.Fatal(err)
	}
	result, err := NewProcessReconciler().ReconcileProcess(context.Background(), changed)
	if err != nil {
		t.Fatal(err)
	}
	if result.Status != ProcessRecoveryIdentityChanged {
		t.Fatalf("recovery result = %#v, want identity-changed", result)
	}
	if err := unix.Kill(cmd.Process.Pid, 0); err != nil {
		t.Fatalf("changed-identity process was not left alive: %v", err)
	}
}

func TestProcessReconcilerReportsMissingProcess(t *testing.T) {
	cmd := startTestProcessGroup(t)
	encoded, err := CaptureProcessIdentity(cmd.Process.Pid)
	if err != nil {
		t.Fatal(err)
	}
	if err := unix.Kill(-cmd.Process.Pid, unix.SIGKILL); err != nil {
		t.Fatal(err)
	}
	_, _ = cmd.Process.Wait()

	identity, err := DecodeProcessIdentity(encoded)
	if err != nil {
		t.Fatal(err)
	}
	identity.PID = 1<<30 + 7
	identity.PGID = identity.PID
	missing, err := EncodeProcessIdentity(identity)
	if err != nil {
		t.Fatal(err)
	}
	result, err := NewProcessReconciler().ReconcileProcess(context.Background(), missing)
	if err != nil {
		t.Fatal(err)
	}
	if result.Status != ProcessRecoveryGone {
		t.Fatalf("recovery result = %#v, want gone", result)
	}
}

func TestProcessIdentityRejectsMalformedTokens(t *testing.T) {
	for _, encoded := range []string{"", "pid:123", "ion-process-v1.not-base64"} {
		if _, err := DecodeProcessIdentity(encoded); !errors.Is(err, ErrProcessIdentityInvalid) {
			t.Fatalf("DecodeProcessIdentity(%q) error = %v, want invalid identity", encoded, err)
		}
	}
}

func TestProcessReconcilerHonorsCanceledContext(t *testing.T) {
	cmd := startTestProcessGroup(t)
	encoded, err := CaptureProcessIdentity(cmd.Process.Pid)
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	result, err := NewProcessReconciler().ReconcileProcess(ctx, encoded)
	if err != nil {
		t.Fatal(err)
	}
	if result.Status != ProcessRecoveryFailed {
		t.Fatalf("recovery result = %#v, want failed cancellation", result)
	}
}
