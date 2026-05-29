package session

import (
	"testing"

	"github.com/nijaru/ion/llm"
)

func TestChildRequestedEventRoundTrip(t *testing.T) {
	event := NewChildRequestedEvent("parent", ChildRequestedData{
		ChildID:        "child-1",
		ChildSessionID: "sess-child-1",
		ParentEventID:  "evt-parent-1",
		AgentID:        "reviewer",
		Mode:           ChildModeHandoff,
		Task:           "Review changed files",
		Context:        "Focus on correctness and regressions",
	})

	data, ok, err := event.ChildRequestedData()
	if err != nil {
		t.Fatalf("decode child requested: %v", err)
	}
	if !ok {
		t.Fatal("expected child requested payload")
	}
	if data.Mode != ChildModeHandoff || data.AgentID != "reviewer" {
		t.Fatalf("unexpected payload: %#v", data)
	}
}

func TestChildCompletedEventRoundTrip(t *testing.T) {
	event := NewChildCompletedEvent("parent", ChildCompletedData{
		ChildID:        "child-1",
		ChildSessionID: "sess-child-1",
		Summary:        "Reviewed 3 files",
		ArtifactIDs:    []string{"artifact-1"},
		EpisodeID:      "episode-1",
		Usage: llm.Usage{
			InputTokens:  10,
			OutputTokens: 20,
			TotalTokens:  30,
			Cost:         0.12,
		},
	})

	data, ok, err := event.ChildCompletedData()
	if err != nil {
		t.Fatalf("decode child completed: %v", err)
	}
	if !ok {
		t.Fatal("expected child completed payload")
	}
	if data.EpisodeID != "episode-1" || len(data.ArtifactIDs) != 1 {
		t.Fatalf("unexpected payload: %#v", data)
	}
	if data.Usage.TotalTokens != 30 {
		t.Fatalf("unexpected usage: %#v", data.Usage)
	}
}

func TestArtifactRecordedEventDefaultsSessionID(t *testing.T) {
	event := NewArtifactRecordedEvent("sess-parent", ArtifactRecordedData{
		ChildID: "child-1",
		Artifact: ArtifactRef{
			ID:    "artifact-1",
			Kind:  "patch",
			URI:   "/tmp/patch.diff",
			Label: "Worker patch",
		},
	})

	data, ok, err := event.ArtifactRecordedData()
	if err != nil {
		t.Fatalf("decode artifact recorded: %v", err)
	}
	if !ok {
		t.Fatal("expected artifact recorded payload")
	}
	if data.SessionID != "sess-parent" {
		t.Fatalf("artifact session_id = %q, want sess-parent", data.SessionID)
	}
	if data.Artifact.Kind != "patch" {
		t.Fatalf("unexpected artifact payload: %#v", data.Artifact)
	}
}

func TestRecordArtifactDefaultsProducerSessionID(t *testing.T) {
	sess := New("sess-parent")

	if err := RecordArtifact(t.Context(), sess, ArtifactRecordedData{
		ChildID: "child-1",
		Artifact: ArtifactRef{
			ID:   "artifact-1",
			Kind: "patch",
			URI:  "memory://patch.diff",
		},
	}); err != nil {
		t.Fatalf("record artifact: %v", err)
	}

	last := sess.Events()[len(sess.Events())-1]
	data, ok, err := last.ArtifactRecordedData()
	if err != nil {
		t.Fatalf("decode artifact recorded: %v", err)
	}
	if !ok {
		t.Fatal("expected artifact recorded payload")
	}
	if data.SessionID != "sess-parent" {
		t.Fatalf("session id = %q, want sess-parent", data.SessionID)
	}
	if data.Artifact.ProducerSessionID != "sess-parent" {
		t.Fatalf("producer session id = %q, want sess-parent", data.Artifact.ProducerSessionID)
	}
}

func TestChildDataDecodeReturnsFalseForOtherEventTypes(t *testing.T) {
	event := NewMessage("sess", llm.Message{Role: llm.RoleUser, Content: "hi"})

	_, ok, err := event.ChildStartedData()
	if err != nil {
		t.Fatalf("unexpected decode error: %v", err)
	}
	if ok {
		t.Fatal("expected non-child event to return ok=false")
	}
}
