package safety_test

import (
	"context"
	"testing"

	"github.com/nijaru/ion/approval"
	"github.com/nijaru/ion/safety"
	"github.com/nijaru/ion/session"
)

func TestProtectedPaths(t *testing.T) {
	sess := session.New("test")
	// Base policy that auto-approves everything
	autoPolicy := approval.PolicyFunc(
		func(ctx context.Context, sess *session.Session, req approval.Request) (approval.Result, bool, error) {
			return approval.Result{Decision: approval.DecisionAllow}, true, nil
		},
	)

	protected := safety.ProtectedPaths(autoPolicy, []string{".git", ".env", "secrets/"})

	tests := []struct {
		name         string
		req          approval.Request
		wantHandled  bool
		wantDecision approval.Decision
	}{
		{
			name: "Read allowed on protected path",
			req: approval.Request{
				Category: string(safety.CategoryRead),
				Resource: ".git/config",
			},
			wantHandled:  true,
			wantDecision: approval.DecisionAllow,
		},
		{
			name: "Write allowed on normal path",
			req: approval.Request{
				Category: string(safety.CategoryWrite),
				Resource: "src/main.go",
			},
			wantHandled:  true,
			wantDecision: approval.DecisionAllow,
		},
		{
			name: "Write deferred on exact protected path",
			req: approval.Request{
				Category: string(safety.CategoryWrite),
				Resource: ".env",
			},
			wantHandled: false,
		},
		{
			name: "Write deferred on protected subdirectory",
			req: approval.Request{
				Category: string(safety.CategoryWrite),
				Resource: ".git/config",
			},
			wantHandled: false,
		},
		{
			name: "Write allowed on similarly named path",
			req: approval.Request{
				Category: string(safety.CategoryWrite),
				Resource: ".gitignore", // Should not match ".git"
			},
			wantHandled:  true,
			wantDecision: approval.DecisionAllow,
		},
		{
			name: "Write allowed on similarly named path 2",
			req: approval.Request{
				Category: string(safety.CategoryWrite),
				Resource: ".env.example", // Should not match ".env"
			},
			wantHandled:  true,
			wantDecision: approval.DecisionAllow,
		},
		{
			name: "Write deferred on nested protected directory",
			req: approval.Request{
				Category: string(safety.CategoryWrite),
				Resource: "secrets/api_key.txt",
			},
			wantHandled: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			res, handled, err := protected.Decide(context.Background(), sess, tt.req)
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
