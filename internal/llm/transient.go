package llm

import (
	"errors"
	"io"
	"net"
	"net/url"
	"syscall"
)

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
	if errors.As(err, &netErr) && (netErr.Timeout() || netErr.Temporary()) {
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
