package canto

import (
	"testing"

	"github.com/nijaru/ion/internal/backend"
	ionsession "github.com/nijaru/ion/internal/session"
)

func TestBackendPolicyDefaultsToTrustedAuto(t *testing.T) {
	b := New()

	policy, reason := b.policy.Authorize(
		t.Context(),
		"write",
		`{"file_path":"handoff.md"}`,
	)
	if policy != backend.PolicyAllow {
		t.Fatalf("default write policy = %s (%q), want allow", policy, reason)
	}
}

func TestBackendSetModeClearsTrustedAuto(t *testing.T) {
	b := New()
	b.SetMode(ionsession.ModeEdit)

	policy, reason := b.policy.Authorize(
		t.Context(),
		"write",
		`{"file_path":"handoff.md"}`,
	)
	if policy != backend.PolicyAsk {
		t.Fatalf("EDIT write policy = %s (%q), want ask", policy, reason)
	}
}
