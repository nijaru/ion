package ctxerr

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"time"
)

type TimeoutError struct {
	Operation string
	Timeout   time.Duration
	Err       error
}

func (e TimeoutError) Error() string {
	operation := normalizeOperation(e.Operation)
	if e.Timeout > 0 {
		return fmt.Sprintf("%s timed out after %s", operation, e.Timeout)
	}
	return operation + " timed out"
}

func (e TimeoutError) Unwrap() error {
	if e.Err != nil {
		return e.Err
	}
	return context.DeadlineExceeded
}

func Timeout(operation string, timeout time.Duration, err error) error {
	if err == nil {
		err = context.DeadlineExceeded
	}
	return TimeoutError{
		Operation: operation,
		Timeout:   timeout,
		Err:       err,
	}
}

func WrapContext(operation string, err error) error {
	if err == nil {
		return nil
	}
	if errors.Is(err, context.DeadlineExceeded) {
		return Timeout(operation, 0, err)
	}
	if errors.Is(err, context.Canceled) {
		return fmt.Errorf("%s canceled: %w", normalizeOperation(operation), err)
	}
	return err
}

func normalizeOperation(operation string) string {
	operation = strings.TrimSpace(operation)
	if operation == "" {
		return "operation"
	}
	return operation
}
