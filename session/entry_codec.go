package session

import (
	"encoding/json"
	"fmt"
	"time"
)

// entryEnvelope is the stable transport shape used by session export/import.
// The SQLite payload remains an implementation detail; this envelope carries
// the tree identity needed when a session crosses a process or store boundary.
type entryEnvelope struct {
	ID        string          `json:"id"`
	ParentID  string          `json:"parent_id,omitempty"`
	Type      string          `json:"type"`
	Timestamp int64           `json:"timestamp"`
	Payload   json.RawMessage `json:"payload"`
}

// MarshalEntry encodes one typed entry for a session transport.
func MarshalEntry(entry Entry) ([]byte, error) {
	typ, payload, err := encodeEntry(entry)
	if err != nil {
		return nil, fmt.Errorf("encode entry %q: %w", entry.ID(), err)
	}
	return json.Marshal(entryEnvelope{
		ID:        entry.ID(),
		ParentID:  entry.ParentID(),
		Type:      typ,
		Timestamp: entry.When().UnixMilli(),
		Payload:   payload,
	})
}

// UnmarshalEntry decodes one entry previously produced by MarshalEntry.
func UnmarshalEntry(data []byte) (Entry, error) {
	var envelope entryEnvelope
	if err := json.Unmarshal(data, &envelope); err != nil {
		return nil, fmt.Errorf("decode entry envelope: %w", err)
	}
	if envelope.ID == "" || envelope.Type == "" {
		return nil, fmt.Errorf("entry envelope requires id and type")
	}
	if len(envelope.Payload) == 0 {
		return nil, fmt.Errorf("entry %q has no payload", envelope.ID)
	}
	return decodeEntry(
		EntryBase{ID: envelope.ID, ParentID: envelope.ParentID, Timestamp: time.UnixMilli(envelope.Timestamp)},
		envelope.Type,
		envelope.Payload,
	)
}
