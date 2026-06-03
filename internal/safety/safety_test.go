package safety_test

import (
	"context"
	"testing"

	"github.com/nijaru/ion/internal/approval"
	"github.com/nijaru/ion/internal/safety"
	"github.com/nijaru/ion/session"
)

func TestPolicy_Decide(t *testing.T) {
	ctx := context.Background()
	sess := session.New("test")

	tests := []struct {
		name         string
		mode         safety.Mode
		req          approval.Request
		wantDecision approval.Decision
		wantHandled  bool
	}{
		{
			name: "Auto mode handles everything",
			mode: safety.ModeAuto,
			req: approval.Request{
				Category: string(safety.CategoryExecute),
			},
			wantDecision: approval.DecisionAllow,
			wantHandled:  true,
		},
		{
			name: "Read mode allows read",
			mode: safety.ModeRead,
			req: approval.Request{
				Category: string(safety.CategoryRead),
			},
			wantDecision: approval.DecisionAllow,
			wantHandled:  true,
		},
		{
			name: "Read mode denies write",
			mode: safety.ModeRead,
			req: approval.Request{
				Category: string(safety.CategoryWrite),
			},
			wantDecision: approval.DecisionDeny,
			wantHandled:  true,
		},
		{
			name: "Read mode denies execute",
			mode: safety.ModeRead,
			req: approval.Request{
				Category: string(safety.CategoryExecute),
			},
			wantDecision: approval.DecisionDeny,
			wantHandled:  true,
		},
		{
			name: "Edit mode allows read",
			mode: safety.ModeEdit,
			req: approval.Request{
				Category: string(safety.CategoryRead),
			},
			wantDecision: approval.DecisionAllow,
			wantHandled:  true,
		},
		{
			name: "Edit mode defers write",
			mode: safety.ModeEdit,
			req: approval.Request{
				Category: string(safety.CategoryWrite),
			},
			wantHandled: false, // Requires manual approval
		},
		{
			name: "Edit mode defers execute",
			mode: safety.ModeEdit,
			req: approval.Request{
				Category: string(safety.CategoryExecute),
			},
			wantHandled: false, // Requires manual approval
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			policy := safety.NewConfig(tt.mode)
			res, handled, err := policy.Decide(ctx, sess, tt.req)
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if handled != tt.wantHandled {
				t.Errorf("got handled = %v, want %v", handled, tt.wantHandled)
			}
			if handled && res.Decision != tt.wantDecision {
				t.Errorf("got decision = %v, want %v", res.Decision, tt.wantDecision)
			}
		})
	}
}
