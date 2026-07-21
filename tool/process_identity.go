package tool

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"
)

// ProcessIdentity is the opaque, durable identity of a process-group leader.
// PID and PGID alone are not identities: both can be reused after a process
// exits. StartToken is supplied by the host operating system and must match
// before Ion will signal a recovered process group.
type ProcessIdentity struct {
	Version    int    `json:"version"`
	Platform   string `json:"platform"`
	PID        int    `json:"pid"`
	PGID       int    `json:"pgid"`
	StartToken string `json:"start_token"`
}

const processIdentityVersion = 1

var (
	ErrProcessIdentityUnsupported = errors.New("process identity is unsupported on this platform")
	ErrProcessIdentityInvalid     = errors.New("invalid process identity")
	ErrProcessIdentityChanged     = errors.New("process identity changed")
	ErrProcessNotFound            = errors.New("process not found")
)

// ProcessRecoveryStatus describes what happened when startup inspected a
// previously recorded process group. Recovery never turns an action into a
// successful result; it only makes cleanup evidence durable before the user
// explicitly reconciles the external outcome.
type ProcessRecoveryStatus string

const (
	ProcessRecoveryGone            ProcessRecoveryStatus = "gone"
	ProcessRecoveryTerminated      ProcessRecoveryStatus = "terminated"
	ProcessRecoveryIdentityChanged ProcessRecoveryStatus = "identity-changed"
	ProcessRecoveryUnavailable     ProcessRecoveryStatus = "unavailable"
	ProcessRecoveryInvalid         ProcessRecoveryStatus = "invalid"
	ProcessRecoveryFailed          ProcessRecoveryStatus = "failed"
)

// ProcessRecoveryResult is the bounded evidence returned by process
// reconciliation. Detail is suitable for the local action journal, not for
// model-visible prompt content.
type ProcessRecoveryResult struct {
	Status ProcessRecoveryStatus
	Detail string
}

// ProcessReconciler is injected by the host and called by the runtime
// controller during startup recovery. It is deliberately narrow so the
// controller owns sequencing and persistence while the tool layer owns OS
// process inspection and signaling.
type ProcessReconciler interface {
	ReconcileProcess(ctx context.Context, encodedIdentity string) (ProcessRecoveryResult, error)
}

type processPlatform interface {
	name() string
	capture(pid int) (ProcessIdentity, error)
	inspect(pid int) (ProcessIdentity, error)
	terminateGroup(ctx context.Context, identity ProcessIdentity) error
	groupExists(pgid int) (bool, error)
}

// CaptureProcessIdentity captures the canonical identity used by the action
// journal. The process must be a process-group leader so recovery can clean up
// its descendants without confusing an unrelated group.
func CaptureProcessIdentity(pid int) (string, error) {
	if pid <= 0 {
		return "", fmt.Errorf("%w: pid must be positive", ErrProcessIdentityInvalid)
	}
	identity, err := hostProcessPlatform().capture(pid)
	if err != nil {
		return "", err
	}
	return EncodeProcessIdentity(identity)
}

// CaptureProcessLaunchIdentity captures identity only from an executor-issued
// launch capability. Callers cannot provide a raw PID through this boundary.
func CaptureProcessLaunchIdentity(launch ProcessLaunch) (string, error) {
	pid, err := launch.pidForIdentity()
	if err != nil {
		return "", err
	}
	return CaptureProcessIdentity(pid)
}

// EncodeProcessIdentity turns an identity into a versioned opaque token for
// durable storage. The token contains no command line, environment, or path.
func EncodeProcessIdentity(identity ProcessIdentity) (string, error) {
	if err := validateProcessIdentity(identity); err != nil {
		return "", err
	}
	payload, err := json.Marshal(identity)
	if err != nil {
		return "", fmt.Errorf("encode process identity: %w", err)
	}
	return "ion-process-v1." + base64.RawURLEncoding.EncodeToString(payload), nil
}

// DecodeProcessIdentity validates and decodes a durable identity token.
func DecodeProcessIdentity(encoded string) (ProcessIdentity, error) {
	const prefix = "ion-process-v1."
	if !strings.HasPrefix(encoded, prefix) {
		return ProcessIdentity{}, fmt.Errorf("%w: unknown encoding", ErrProcessIdentityInvalid)
	}
	payload, err := base64.RawURLEncoding.DecodeString(strings.TrimPrefix(encoded, prefix))
	if err != nil {
		return ProcessIdentity{}, fmt.Errorf("%w: decode token: %v", ErrProcessIdentityInvalid, err)
	}
	var identity ProcessIdentity
	if err := json.Unmarshal(payload, &identity); err != nil {
		return ProcessIdentity{}, fmt.Errorf("%w: decode payload: %v", ErrProcessIdentityInvalid, err)
	}
	if err := validateProcessIdentity(identity); err != nil {
		return ProcessIdentity{}, err
	}
	return identity, nil
}

func validateProcessIdentity(identity ProcessIdentity) error {
	if identity.Version != processIdentityVersion {
		return fmt.Errorf("%w: unsupported version %d", ErrProcessIdentityInvalid, identity.Version)
	}
	if identity.Platform == "" || identity.PID <= 0 || identity.PGID <= 0 || identity.PID != identity.PGID || identity.StartToken == "" {
		return fmt.Errorf("%w: incomplete process-group identity", ErrProcessIdentityInvalid)
	}
	return nil
}

type processReconciler struct{}

// NewProcessReconciler returns the host process reconciler. Unsupported hosts
// still return a reconciler; it reports an unavailable result and therefore
// leaves the action indeterminate instead of silently skipping recovery.
func NewProcessReconciler() ProcessReconciler {
	return processReconciler{}
}

func (processReconciler) ReconcileProcess(ctx context.Context, encodedIdentity string) (ProcessRecoveryResult, error) {
	identity, err := DecodeProcessIdentity(encodedIdentity)
	if err != nil {
		return ProcessRecoveryResult{Status: ProcessRecoveryInvalid, Detail: err.Error()}, nil
	}
	platform := hostProcessPlatform()
	if identity.Platform != platform.name() {
		return ProcessRecoveryResult{
			Status: ProcessRecoveryUnavailable,
			Detail: fmt.Sprintf("recorded on %s; current host is %s", identity.Platform, platform.name()),
		}, nil
	}
	observed, err := platform.inspect(identity.PID)
	if err != nil {
		switch {
		case errors.Is(err, ErrProcessNotFound):
			return ProcessRecoveryResult{Status: ProcessRecoveryGone, Detail: "recorded process group is no longer present"}, nil
		case errors.Is(err, ErrProcessIdentityUnsupported):
			return ProcessRecoveryResult{Status: ProcessRecoveryUnavailable, Detail: err.Error()}, nil
		default:
			return ProcessRecoveryResult{Status: ProcessRecoveryFailed, Detail: fmt.Sprintf("inspect recorded process: %v", err)}, nil
		}
	}
	if observed != identity {
		return ProcessRecoveryResult{
			Status: ProcessRecoveryIdentityChanged,
			Detail: "recorded PID is present but its process-group identity changed; refused to signal it",
		}, nil
	}
	if err := platform.terminateGroup(ctx, identity); err != nil {
		if errors.Is(err, ErrProcessNotFound) {
			return ProcessRecoveryResult{Status: ProcessRecoveryGone, Detail: "recorded process group exited before cleanup"}, nil
		}
		if errors.Is(err, ErrProcessIdentityChanged) {
			return ProcessRecoveryResult{Status: ProcessRecoveryIdentityChanged, Detail: "process identity changed before cleanup; refused to signal it"}, nil
		}
		return ProcessRecoveryResult{Status: ProcessRecoveryFailed, Detail: fmt.Sprintf("terminate recorded process group: %v", err)}, nil
	}
	// Once signaling has succeeded, cleanup verification must not be abandoned
	// merely because the initiating turn was canceled.
	verifyCtx, cancelVerify := context.WithTimeout(context.Background(), 500*time.Millisecond)
	defer cancelVerify()
	poll := time.NewTicker(10 * time.Millisecond)
	defer poll.Stop()
	for {
		leaderGone := false
		observed, err = platform.inspect(identity.PID)
		if errors.Is(err, ErrProcessNotFound) {
			leaderGone = true
		} else if err != nil {
			return ProcessRecoveryResult{Status: ProcessRecoveryFailed, Detail: fmt.Sprintf("verify process-group cleanup: %v", err)}, nil
		} else if observed != identity {
			return ProcessRecoveryResult{
				Status: ProcessRecoveryIdentityChanged,
				Detail: "process identity changed during cleanup; no further signal sent",
			}, nil
		}
		groupExists, err := platform.groupExists(identity.PGID)
		if err != nil {
			return ProcessRecoveryResult{Status: ProcessRecoveryFailed, Detail: fmt.Sprintf("verify process-group cleanup: %v", err)}, nil
		}
		if leaderGone && !groupExists {
			return ProcessRecoveryResult{Status: ProcessRecoveryTerminated, Detail: "terminated matching recorded process group"}, nil
		}
		select {
		case <-verifyCtx.Done():
			return ProcessRecoveryResult{Status: ProcessRecoveryFailed, Detail: "recorded process group remained alive after termination"}, nil
		case <-poll.C:
		}
	}
}
