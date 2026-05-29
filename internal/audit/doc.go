// Package audit provides append-only structured logging for security events.
//
// The logger is intentionally generic so approval, safety, and execution
// boundaries can emit the same event shape without depending on each other.
package audit
