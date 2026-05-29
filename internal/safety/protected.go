package safety

import (
	"context"
	"path/filepath"
	"strings"

	"github.com/nijaru/ion/internal/approval"
	"github.com/nijaru/ion/internal/audit"
	"github.com/nijaru/ion/internal/storage/session"
)

// DefaultProtectedPaths returns a standard list of paths that should always
// require manual approval before being modified.
func DefaultProtectedPaths() []string {
	return []string{".git", ".env"}
}

// IsProtectedPath reports whether target matches or descends from one of paths.
func IsProtectedPath(target string, paths []string) bool {
	target = filepath.Clean(target)
	for _, p := range paths {
		protected := filepath.Clean(p)
		if target == protected || strings.HasPrefix(target, protected+string(filepath.Separator)) {
			return true
		}
	}
	return false
}

// ProtectedPaths wraps an existing policy and defers to manual approval for
// any write or execute operations targeting the specified paths or their
// subdirectories. It does so by returning handled=false (skipping the wrapped
// policy), which signals the policy chain that human approval is required.
//
// Callers must provide canonical (symlink-resolved) paths. This policy does
// not perform its own symlink resolution; that is the workspace layer's
// responsibility.
func ProtectedPaths(next approval.Policy, paths []string) approval.Policy {
	return ProtectedPathsWithAudit(next, paths, nil)
}

// ProtectedPathsWithAudit is the audit-aware form of ProtectedPaths.
func ProtectedPathsWithAudit(
	next approval.Policy,
	paths []string,
	logger audit.Logger,
) approval.Policy {
	protected := make([]string, 0, len(paths))
	for _, p := range paths {
		protected = append(protected, filepath.Clean(p))
	}

	return approval.PolicyFunc(
		func(ctx context.Context, sess *session.Session, req approval.Request) (approval.Result, bool, error) {
			cat := Category(req.Category)
			if (cat == CategoryWrite || cat == CategoryExecute) && req.Resource != "" {
				if IsProtectedPath(req.Resource, protected) {
					if logger != nil {
						_ = logger.Log(context.Background(), audit.Event{
							Kind:      audit.KindProtectedPathBlocked,
							SessionID: req.SessionID,
							Tool:      req.Tool,
							Category:  req.Category,
							Operation: req.Operation,
							Resource:  req.Resource,
							Decision:  "defer",
							Reason:    "protected path requires manual approval",
							Metadata:  cloneMetadata(req.Metadata),
						})
					}
					// Force manual approval by skipping the next policy
					return approval.Result{}, false, nil
				}
			}

			if next == nil {
				return approval.Result{}, false, nil
			}
			return next.Decide(ctx, sess, req)
		},
	)
}
