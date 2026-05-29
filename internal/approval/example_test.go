package approval_test

import (
	"context"
	"fmt"
	"strings"

	"github.com/nijaru/ion/internal/approval"
	"github.com/nijaru/ion/internal/storage/session"
)

func ExamplePolicyFunc_shellClassifierSeam() {
	// A host can provide this policy without Canto owning the
	// command heuristics. The example is deliberately tiny: real products can
	// parse POSIX shell, check cwd/path policy, and inspect user trust state.
	shellPolicy := approval.PolicyFunc(
		func(
			ctx context.Context,
			sess *session.Session,
			req approval.Request,
		) (approval.Result, bool, error) {
			if req.Tool != "bash" {
				return approval.Result{}, false, nil
			}
			if strings.HasPrefix(req.Resource, "git status") {
				return approval.Result{
					Decision: approval.DecisionAllow,
					Reason:   "host classifier allows read-only git status",
				}, true, nil
			}
			return approval.Result{}, false, nil
		},
	)

	manager := approval.NewGate(shellPolicy)
	sess := session.New("s")
	res, err := manager.Request(
		context.Background(),
		sess,
		"bash",
		`{"command":"git status --short"}`,
		approval.Requirement{
			Category:  "execute",
			Operation: "exec",
			Resource:  "git status --short",
		},
	)

	fmt.Println(res.Decision, res.Automated, err == nil)
	// Output: allow true true
}
