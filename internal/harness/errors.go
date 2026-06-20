package harness

import "fmt"

// HarnessError represents a typed error from the Harness.
type HarnessError struct {
	Code    ErrorCode
	Message string
	Cause   error
}

func (e *HarnessError) Error() string {
	if e.Cause != nil {
		return fmt.Sprintf("%s: %s: %v", e.Code, e.Message, e.Cause)
	}
	return fmt.Sprintf("%s: %s", e.Code, e.Message)
}

func (e *HarnessError) Unwrap() error {
	return e.Cause
}

// ErrorCode represents the type of harness error.
type ErrorCode string

const (
	// ErrCodeBusy indicates another operation is in progress.
	ErrCodeBusy ErrorCode = "busy"
	// ErrCodeHookFailed indicates a hook callback failed.
	ErrCodeHookFailed ErrorCode = "hook_failed"
	// ErrCodeHookAborted indicates a hook aborted the operation.
	ErrCodeHookAborted ErrorCode = "hook_aborted"
	// ErrCodeAgentFailed indicates the agent loop failed.
	ErrCodeAgentFailed ErrorCode = "agent_failed"
	// ErrCodeNotRunning indicates the harness is not running.
	ErrCodeNotRunning ErrorCode = "not_running"
	// ErrCodeAlreadyRunning indicates the harness is already running.
	ErrCodeAlreadyRunning ErrorCode = "already_running"
	// ErrCodeInvalidInput indicates invalid input was provided.
	ErrCodeInvalidInput ErrorCode = "invalid_input"
)

// NewBusyError creates a busy error.
func NewBusyError() *HarnessError {
	return &HarnessError{
		Code:    ErrCodeBusy,
		Message: "another operation is in progress",
	}
}

// NewHookFailedError creates a hook failure error.
func NewHookFailedError(hookName string, cause error) *HarnessError {
	return &HarnessError{
		Code:    ErrCodeHookFailed,
		Message: fmt.Sprintf("%s hook failed", hookName),
		Cause:   cause,
	}
}

// NewHookAbortedError creates a hook abort error.
func NewHookAbortedError(hookName string, reason string) *HarnessError {
	return &HarnessError{
		Code:    ErrCodeHookAborted,
		Message: fmt.Sprintf("%s hook aborted: %s", hookName, reason),
	}
}

// NewAgentFailedError creates an agent failure error.
func NewAgentFailedError(cause error) *HarnessError {
	return &HarnessError{
		Code:    ErrCodeAgentFailed,
		Message: "agent loop failed",
		Cause:   cause,
	}
}

// IsErrorCode checks if an error is a HarnessError with the given code.
func IsErrorCode(err error, code ErrorCode) bool {
	if err == nil {
		return false
	}
	var he *HarnessError
	if As(err, &he) {
		return he.Code == code
	}
	return false
}

// As is a convenience wrapper around errors.As.
func As(err error, target interface{}) bool {
	if err == nil {
		return false
	}
	// Use type assertion for common types
	switch t := target.(type) {
	case **HarnessError:
		if he, ok := err.(*HarnessError); ok {
			*t = he
			return true
		}
		// Check wrapped errors
		if u, ok := err.(interface{ Unwrap() error }); ok {
			return As(u.Unwrap(), target)
		}
	}
	return false
}
