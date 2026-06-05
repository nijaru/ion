package agent

import "regexp"

// retryableErrorPattern matches transient errors that should be retried.
// Matches Pi's _isRetryableError logic:
// - Overloaded/rate limit errors
// - Server errors (500, 502, 503, 504)
// - Network/connection errors
// - Timeout errors
// - Stream interruption errors
var retryableErrorPattern = regexp.MustCompile(`(?i)` +
	`overloaded|` +
	`provider.?returned.?error|` +
	`rate.?limit|` +
	`too many requests|` +
	`429|` +
	`500|` +
	`502|` +
	`503|` +
	`504|` +
	`service.?unavailable|` +
	`server.?error|` +
	`internal.?error|` +
	`network.?error|` +
	`connection.?error|` +
	`connection.?refused|` +
	`connection.?lost|` +
	`websocket.?closed|` +
	`websocket.?error|` +
	`other side closed|` +
	`fetch failed|` +
	`upstream.?connect|` +
	`reset before headers|` +
	`socket hang up|` +
	`ended without|` +
	`stream ended before message_stop|` +
	`http2 request did not get a response|` +
	`timed? out|` +
	`timeout|` +
	`terminated|` +
	`retry delay`,
)

// nonRetryableBillingPattern matches billing/quota errors that should NOT be retried.
// These are permanent failures, not transient.
var nonRetryableBillingPattern = regexp.MustCompile(`(?i)` +
	`GoUsageLimitError|` +
	`FreeUsageLimitError|` +
	`Monthly usage limit reached|` +
	`available balance|` +
	`insufficient_quota|` +
	`out of budget|` +
	`quota exceeded|` +
	`billing`,
)

// IsRetryableError checks if an error message indicates a transient error
// that should be retried with exponential backoff.
//
// Returns false for:
// - Context overflow errors (handled by compaction)
// - Billing/quota errors (permanent failures)
// - Empty errors
func IsRetryableError(errorMessage string) bool {
	if errorMessage == "" {
		return false
	}

	// Context overflow is handled by compaction, not retry
	if IsContextOverflow(errorMessage) {
		return false
	}

	// Billing/quota errors are permanent failures
	if nonRetryableBillingPattern.MatchString(errorMessage) {
		return false
	}

	return retryableErrorPattern.MatchString(errorMessage)
}
