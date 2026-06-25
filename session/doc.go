// Package session owns the domain model: the messages the agent produces and
// consumes, the entries that form a session tree, and the events the agent loop
// emits. It is a leaf in the dependency graph — nothing it imports imports it
// back. The agent package is the only writer; the TUI is a read-only projection.
//
// The model is translated from Pi's @earendil-works/pi-ai types (the reference
// implementation). There is one Message type, discriminated by role. The
// provider wire-format (llm.Message) is a boundary transform applied in the
// agent package, not a domain type stored here.
package session
