package anthropic

import (
	"errors"
	"strings"

	sdk "github.com/anthropics/anthropic-sdk-go"
	"github.com/nijaru/ion/llm"
)

// IsTransient returns true if the error is a rate limit or server error.
func (p *Provider) IsTransient(err error) bool {
	if err == nil {
		return false
	}
	if llm.IsTransientTransportError(err) {
		return true
	}
	var sdkErr *sdk.Error
	if errors.As(err, &sdkErr) {
		switch sdkErr.StatusCode {
		case 429, 500, 502, 503, 504:
			return true
		}
	}
	return false
}

// IsContextOverflow returns true if the error indicates the model's context
// window was exceeded.
func (p *Provider) IsContextOverflow(err error) bool {
	if err == nil {
		return false
	}
	var sdkErr *sdk.Error
	if errors.As(err, &sdkErr) && sdkErr.StatusCode == 400 {
		return isContextOverflowMessage(sdkErr.Error())
	}
	return false
}

func isContextOverflowMessage(message string) bool {
	normalized := strings.ToLower(message)
	return strings.Contains(normalized, "prompt") &&
		strings.Contains(normalized, "token")
}
