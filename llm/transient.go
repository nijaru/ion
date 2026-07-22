package llm

import (
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"syscall"
	"time"
)

// HTTPError is a provider-neutral HTTP failure. Adapters should return it
// when they implement their own wire protocol so the runtime can apply the
// same retry and rate-limit policy as SDK-backed providers.
type HTTPError struct {
	Provider   string
	Code       int
	Body       string
	retryAfter time.Duration
}

// NewHTTPError creates a bounded provider HTTP error. Provider response bodies
// are diagnostic input, not an unbounded error channel.
func NewHTTPError(provider string, code int, body []byte) *HTTPError {
	return NewHTTPErrorWithRetryAfter(provider, code, body, 0)
}

// NewHTTPErrorWithRetryAfter creates a bounded provider HTTP error with an
// optional server-provided retry delay.
func NewHTTPErrorWithRetryAfter(provider string, code int, body []byte, retryAfter time.Duration) *HTTPError {
	const maxBody = 8 << 10
	if len(body) > maxBody {
		body = append(append([]byte(nil), body[:maxBody]...), "..."...)
	}
	if retryAfter < 0 {
		retryAfter = 0
	}
	return &HTTPError{Provider: provider, Code: code, Body: string(body), retryAfter: retryAfter}
}

func (e *HTTPError) Error() string {
	if e == nil {
		return "provider HTTP request failed"
	}
	if e.Body == "" {
		return fmt.Sprintf("%s: HTTP %d", e.Provider, e.Code)
	}
	return fmt.Sprintf("%s: HTTP %d: %s", e.Provider, e.Code, e.Body)
}

// StatusCode implements the statusCoder contract used by rate-limit policy.
func (e *HTTPError) StatusCode() int {
	if e == nil {
		return 0
	}
	return e.Code
}

// RetryAfter returns the positive server-provided retry delay, when present.
// It implements the optional retryAfter contract used by RetryProvider.
func (e *HTTPError) RetryAfter() time.Duration {
	if e == nil {
		return 0
	}
	return e.retryAfter
}

// ParseRetryAfter parses the HTTP Retry-After header as either delay-seconds
// or an HTTP date. Invalid, stale, and unreasonably large values are ignored.
func ParseRetryAfter(value string, now time.Time) (time.Duration, bool) {
	const maxRetryAfter = 24 * time.Hour

	value = strings.TrimSpace(value)
	if value == "" {
		return 0, false
	}
	if seconds, err := strconv.ParseInt(value, 10, 64); err == nil {
		if seconds < 0 || seconds > int64(maxRetryAfter/time.Second) {
			return 0, false
		}
		return time.Duration(seconds) * time.Second, true
	}

	when, err := http.ParseTime(value)
	if err != nil {
		return 0, false
	}
	delay := when.Sub(now)
	if delay < 0 || delay > maxRetryAfter {
		return 0, false
	}
	return delay, true
}

// HTTPStatusCode extracts a status from any provider-neutral or SDK-like
// error that exposes the statusCoder contract.
func HTTPStatusCode(err error) (int, bool) {
	if err == nil {
		return 0, false
	}
	var sc interface{ StatusCode() int }
	if errors.As(err, &sc) {
		code := sc.StatusCode()
		return code, code != 0
	}
	return 0, false
}

// IsTransientTransportError reports whether err looks like a retryable
// network/transport failure rather than a provider-declared terminal error.
func IsTransientTransportError(err error) bool {
	if err == nil {
		return false
	}
	if errors.Is(err, io.ErrUnexpectedEOF) ||
		errors.Is(err, syscall.ECONNREFUSED) ||
		errors.Is(err, syscall.ECONNRESET) ||
		errors.Is(err, syscall.EPIPE) ||
		errors.Is(err, syscall.ETIMEDOUT) {
		return true
	}

	var netErr net.Error
	if errors.As(err, &netErr) && netErr.Timeout() {
		return true
	}

	var dnsErr *net.DNSError
	if errors.As(err, &dnsErr) && (dnsErr.IsTimeout || dnsErr.IsTemporary) {
		return true
	}

	var urlErr *url.Error
	if errors.As(err, &urlErr) {
		return IsTransientTransportError(urlErr.Err)
	}

	return false
}
