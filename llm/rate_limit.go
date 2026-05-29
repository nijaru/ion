package llm

import (
	"errors"
	"net/http"

	sdk "github.com/anthropics/anthropic-sdk-go"
	"github.com/sashabaranov/go-openai"
)

// IsRateLimit returns true if the error is a rate limit error (429).
func IsRateLimit(err error) bool {
	if err == nil {
		return false
	}

	var apiErr *openai.APIError
	if errors.As(err, &apiErr) && apiErr.HTTPStatusCode == http.StatusTooManyRequests {
		return true
	}

	var anthropicErr *sdk.Error
	if errors.As(err, &anthropicErr) && anthropicErr.StatusCode == http.StatusTooManyRequests {
		return true
	}

	type statusCoder interface {
		StatusCode() int
	}
	var sc statusCoder
	if errors.As(err, &sc) && sc.StatusCode() == http.StatusTooManyRequests {
		return true
	}

	return false
}
