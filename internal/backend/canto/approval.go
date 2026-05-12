package canto

import (
	"context"
	"fmt"
)

func (b *Backend) Approve(_ context.Context, requestID string, _ bool) error {
	return fmt.Errorf("native backend has no pending approval request %q", requestID)
}
