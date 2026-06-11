package agent

import "fmt"

// ErrorCode categorizes agent errors for programmatic handling.
type ErrorCode string

const (
	ErrCodeOverflow    ErrorCode = "context_overflow"
	ErrCodeRateLimit   ErrorCode = "rate_limit"
	ErrCodeAuth        ErrorCode = "auth_error"
	ErrCodeTimeout     ErrorCode = "timeout"
	ErrCodeProvider    ErrorCode = "provider_error"
	ErrCodeTool        ErrorCode = "tool_error"
	ErrCodeValidation  ErrorCode = "validation_error"
	ErrCodeCancelled   ErrorCode = "cancelled"
	ErrCodeUnknown     ErrorCode = "unknown"
)

// AgentError is a structured error with code, message, cause, and retryability.
// Pi equivalent: Error with code, message, cause, isRetryable, statusCode.
type AgentError struct {
	Code        ErrorCode `json:"code"`
	Message     string    `json:"message"`
	Cause       error     `json:"cause,omitempty"`
	IsRetryable bool      `json:"is_retryable"`
	StatusCode  int       `json:"status_code,omitempty"`
}

func (e *AgentError) Error() string {
	if e.Cause != nil {
		return fmt.Sprintf("%s: %s: %v", e.Code, e.Message, e.Cause)
	}
	return fmt.Sprintf("%s: %s", e.Code, e.Message)
}

func (e *AgentError) Unwrap() error { return e.Cause }

// NewAgentError creates a new AgentError from an error message.
// It auto-detects the error code from the message content.
func NewAgentError(msg string, cause error) *AgentError {
	code, retryable := classifyError(msg)
	return &AgentError{
		Code:        code,
		Message:     msg,
		Cause:       cause,
		IsRetryable: retryable,
	}
}

// classifyError categorizes an error message into an ErrorCode and retryability.
func classifyError(msg string) (ErrorCode, bool) {
	if IsContextOverflow(msg) {
		return ErrCodeOverflow, true
	}
	if isRateLimitError(msg) {
		return ErrCodeRateLimit, true
	}
	if isAuthError(msg) {
		return ErrCodeAuth, false
	}
	if isTimeoutError(msg) {
		return ErrCodeTimeout, true
	}
	return ErrCodeProvider, false
}

// isRateLimitError checks if the error indicates rate limiting.
func isRateLimitError(msg string) bool {
	return containsIgnoreCase(msg, "rate limit") ||
		containsIgnoreCase(msg, "too many requests") ||
		containsIgnoreCase(msg, "429")
}

// isAuthError checks if the error indicates an authentication failure.
func isAuthError(msg string) bool {
	return containsIgnoreCase(msg, "unauthorized") ||
		containsIgnoreCase(msg, "invalid api key") ||
		containsIgnoreCase(msg, "authentication") ||
		containsIgnoreCase(msg, "401")
}

// isTimeoutError checks if the error indicates a timeout.
func isTimeoutError(msg string) bool {
	return containsIgnoreCase(msg, "timeout") ||
		containsIgnoreCase(msg, "timed out") ||
		containsIgnoreCase(msg, "deadline exceeded")
}

func containsIgnoreCase(s, substr string) bool {
	return len(s) >= len(substr) &&
		(s == substr || len(s) > 0 && containsLower(s, substr))
}

func containsLower(s, substr string) bool {
	for i := 0; i <= len(s)-len(substr); i++ {
		match := true
		for j := 0; j < len(substr); j++ {
			sc := s[i+j]
			tc := substr[j]
			if sc >= 'A' && sc <= 'Z' {
				sc += 32
			}
			if tc >= 'A' && tc <= 'Z' {
				tc += 32
			}
			if sc != tc {
				match = false
				break
			}
		}
		if match {
			return true
		}
	}
	return false
}
