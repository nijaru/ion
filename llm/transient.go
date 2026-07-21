package llm

import (
	"errors"
	"fmt"
	"io"
	"net"
	"net/url"
	"syscall"
)

// HTTPError is a provider-neutral HTTP failure. Adapters should return it
// when they implement their own wire protocol so the runtime can apply the
// same retry and rate-limit policy as SDK-backed providers.
type HTTPError struct {
	Provider string
	Code     int
	Body     string
}

// NewHTTPError creates a bounded provider HTTP error. Provider response bodies
// are diagnostic input, not an unbounded error channel.
func NewHTTPError(provider string, code int, body []byte) *HTTPError {
	const maxBody = 8 << 10
	if len(body) > maxBody {
		body = append(append([]byte(nil), body[:maxBody]...), "..."...)
	}
	return &HTTPError{Provider: provider, Code: code, Body: string(body)}
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
