package approval

import (
	"context"

	"github.com/nijaru/ion/internal/storage/session"
)

// PolicyFunc adapts a function into a Policy.
type PolicyFunc func(ctx context.Context, sess *session.Session, req Request) (Result, bool, error)

// Decide implements Policy.
func (f PolicyFunc) Decide(
	ctx context.Context,
	sess *session.Session,
	req Request,
) (Result, bool, error) {
	if f == nil {
		return Result{}, false, nil
	}
	return f(ctx, sess, req)
}

// Chain composes multiple policies in order.
//
// Later policies can override earlier ones by returning handled=true. If no
// policy handles the request, the chain returns handled=false.
type Chain []Policy

// NewChain creates a composable policy chain.
func NewChain(policies ...Policy) Policy {
	return Chain(policies)
}

// Decide implements Policy.
func (c Chain) Decide(
	ctx context.Context,
	sess *session.Session,
	req Request,
) (Result, bool, error) {
	var (
		res     Result
		handled bool
	)

	for _, policy := range c {
		if policy == nil {
			continue
		}
		next, ok, err := policy.Decide(ctx, sess, req)
		if err != nil {
			return Result{}, false, err
		}
		if ok {
			res = next
			handled = true
		}
	}

	if !handled {
		return Result{}, false, nil
	}
	return res, true, nil
}
