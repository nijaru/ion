package session

import (
	"bytes"
	"fmt"
	"io"
	"time"

	"github.com/go-json-experiment/json"
	"github.com/go-json-experiment/json/jsontext"
	"github.com/oklog/ulid/v2"
)

type eventEnvelope struct {
	ID        ulid.ULID      `json:"id"`
	SessionID string         `json:"session_id"`
	TurnID    string         `json:"turn_id,omitzero"`
	Seq       int64          `json:"seq,omitzero"`
	ParentID  string         `json:"parent_id,omitzero"`
	Type      EventType      `json:"type"`
	Timestamp time.Time      `json:"timestamp"`
	Data      jsontext.Value `json:"data"`
	Metadata  jsontext.Value `json:"metadata,omitzero"`
	Cost      float64        `json:"cost,omitzero"`
}

func envelopeFromEvent(e Event) (eventEnvelope, error) {
	metadata, err := e.encodedMetadata()
	if err != nil {
		return eventEnvelope{}, fmt.Errorf("encode event metadata %s: %w", e.ID, err)
	}
	return eventEnvelope{
		ID:        e.ID,
		SessionID: e.SessionID,
		TurnID:    e.TurnID,
		Seq:       e.Seq,
		ParentID:  e.ParentID,
		Type:      e.Type,
		Timestamp: e.Timestamp,
		Data:      e.Data,
		Metadata:  metadata,
		Cost:      e.Cost,
	}, nil
}

func eventFromEnvelope(env eventEnvelope) Event {
	return Event{
		ID:          env.ID,
		SessionID:   env.SessionID,
		TurnID:      env.TurnID,
		Seq:         env.Seq,
		ParentID:    env.ParentID,
		Type:        env.Type,
		Timestamp:   env.Timestamp,
		Data:        env.Data,
		Cost:        env.Cost,
		metadataRaw: env.Metadata,
	}
}

func writeEventJSON(w io.Writer, e Event) error {
	env, err := envelopeFromEvent(e)
	if err != nil {
		return err
	}
	return json.MarshalWrite(w, env)
}

// MarshalEventJSON encodes an event using the durable session-log envelope.
func MarshalEventJSON(e Event) ([]byte, error) {
	var buf bytes.Buffer
	if err := writeEventJSON(&buf, e); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func decodeEventJSON(data []byte) (Event, error) {
	var env eventEnvelope
	if err := json.Unmarshal(data, &env); err != nil {
		return Event{}, err
	}
	return eventFromEnvelope(env), nil
}

// UnmarshalEventJSON decodes an event from the durable session-log envelope.
func UnmarshalEventJSON(data []byte) (Event, error) {
	return decodeEventJSON(data)
}

func decodeEventRow(
	idStr, sessionID, turnID, parentID, typeStr, timeStr string,
	seq int64,
	data, metadata []byte,
	cost float64,
) (Event, error) {
	id, err := ulid.Parse(idStr)
	if err != nil {
		return Event{}, err
	}
	ts, err := time.Parse(time.RFC3339, timeStr)
	if err != nil {
		ts, err = time.Parse(time.RFC3339Nano, timeStr)
		if err != nil {
			return Event{}, err
		}
	}
	return Event{
		ID:          id,
		SessionID:   sessionID,
		TurnID:      turnID,
		Seq:         seq,
		ParentID:    parentID,
		Type:        EventType(typeStr),
		Timestamp:   ts,
		Data:        jsontext.Value(data),
		Cost:        cost,
		metadataRaw: jsontext.Value(metadata),
	}, nil
}
