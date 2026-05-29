package session

import (
	"context"
	"fmt"
)

// ArtifactRef identifies an artifact emitted by a session or child run.
// Storage, merge policy, and UI treatment are left to higher-level apps.
type ArtifactRef struct {
	ID                string         `json:"id"`
	Kind              string         `json:"kind"`
	URI               string         `json:"uri"`
	Label             string         `json:"label,omitzero"`
	MIMEType          string         `json:"mime_type,omitzero"`
	Size              int64          `json:"size,omitzero"`
	Digest            string         `json:"digest,omitzero"`
	ProducerSessionID string         `json:"producer_session_id,omitzero"`
	ProducerEventID   string         `json:"producer_event_id,omitzero"`
	Metadata          map[string]any `json:"metadata,omitzero"`
}

// RecordArtifact appends an artifact_recorded event for an existing durable
// artifact descriptor or external artifact reference.
func RecordArtifact(
	ctx context.Context,
	sess *Session,
	data ArtifactRecordedData,
) error {
	if sess == nil {
		return fmt.Errorf("record artifact: nil session")
	}

	data.Artifact = withDefaultArtifactProvenance(data.Artifact, sess.ID())
	return sess.Append(ctx, NewArtifactRecordedEvent(sess.ID(), data))
}

func withDefaultArtifactProvenance(
	desc ArtifactRef,
	sessionID string,
) ArtifactRef {
	if desc.ProducerSessionID == "" {
		desc.ProducerSessionID = sessionID
	}
	return desc
}
