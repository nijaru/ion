package openai

import (
	"errors"
	"strings"

	"github.com/nijaru/ion/llm"
	"github.com/sashabaranov/go-openai"
)

// IsTransient returns true if the error is a rate limit or server error.
func (b *Base) IsTransient(err error) bool {
	if err == nil {
		return false
	}
	if llm.IsTransientTransportError(err) {
		return true
	}
	var apiErr *openai.APIError
	if errors.As(err, &apiErr) {
		switch apiErr.HTTPStatusCode {
		case 429, 500, 502, 503, 504:
			return true
		}
	}
	return false
}

// IsContextOverflow returns true if the error indicates the model's context
// window was exceeded.
func (b *Base) IsContextOverflow(err error) bool {
	if err == nil {
		return false
	}
	var apiErr *openai.APIError
	if errors.As(err, &apiErr) {
		if apiErr.Code == "context_length_exceeded" {
			return true
		}
		if apiErr.HTTPStatusCode == 400 && isContextOverflowMessage(apiErr.Message) {
			return true
		}
	}
	return false
}

func isContextOverflowMessage(message string) bool {
	normalized := strings.ToLower(message)
	return strings.Contains(normalized, "context") && strings.Contains(normalized, "token")
}
