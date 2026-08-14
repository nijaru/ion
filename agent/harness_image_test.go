package agent

import (
	"context"
	"encoding/base64"
	"path/filepath"
	"testing"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestHarnessPromptPersistsAndConvertsImageAttachments(t *testing.T) {
	path := filepath.Join(t.TempDir(), "images.db")
	store, err := session.NewSQLiteStore(path, "image-session")
	if err != nil {
		t.Fatal(err)
	}
	sess := session.NewSession(store, 64)

	var requestImage llm.ContentPart
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		for _, message := range req.Messages {
			if message.Role != llm.RoleUser {
				continue
			}
			for _, part := range message.Parts {
				if part.Type == llm.ContentPartImage {
					requestImage = part
				}
			}
		}
		return &mockStream{chunks: []*llm.Chunk{{Content: "seen", StopReason: "stop"}}}, nil
	}
	h := NewController(ControllerConfig{
		Session:  sess,
		Store:    store,
		Model:    llm.Model{ID: "test"},
		StreamFn: streamFn,
	})

	image := session.ImageContent{Data: []byte{1, 2, 3, 4}, MimeType: "image/png"}
	if _, err := h.Prompt(context.Background(), "describe", image); err != nil {
		t.Fatalf("Prompt: %v", err)
	}
	image.Data[0] = 99

	if requestImage.Type != llm.ContentPartImage || requestImage.MIMEType != "image/png" {
		t.Fatalf("provider image = %#v, want png image part", requestImage)
	}
	if requestImage.Data != base64.StdEncoding.EncodeToString([]byte{1, 2, 3, 4}) {
		t.Fatalf("provider image data = %q, want original bytes", requestImage.Data)
	}

	snapshot, err := sess.BuildContext(context.Background())
	if err != nil {
		t.Fatalf("BuildContext: %v", err)
	}
	var found bool
	for _, message := range snapshot.Messages {
		user, ok := message.(*session.UserMessage)
		if !ok {
			continue
		}
		for _, content := range user.Content {
			stored, ok := content.(session.ImageContent)
			if ok && stored.MimeType == "image/png" {
				found = true
				if string(stored.Data) != string([]byte{1, 2, 3, 4}) {
					t.Fatalf("stored image data = %v, want original bytes", stored.Data)
				}
			}
		}
	}
	if !found {
		t.Fatal("persisted user message did not contain image attachment")
	}

	if err := h.Close(); err != nil {
		t.Fatalf("Controller.Close: %v", err)
	}
	if err := store.Close(); err != nil {
		t.Fatalf("store.Close: %v", err)
	}
}
