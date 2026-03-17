package native

import (
	"context"
	"fmt"
	"os"

	"github.com/google/generative-ai-go/genai"
	"google.golang.org/api/iterator"
	"google.golang.org/api/option"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

type Backend struct {
	events  chan session.Event
	client  *genai.Client
	model   *genai.GenerativeModel
	cs      *genai.ChatSession
	storage storage.Store
	sess    storage.Session
}

func (b *Backend) SetStore(s storage.Store) {
	b.storage = s
}

func (b *Backend) SetSession(s storage.Session) {
	b.sess = s
}

func (b *Backend) ID() string {
	if b.sess != nil {
		return b.sess.ID()
	}
	return ""
}

func (b *Backend) Meta() map[string]string {
	if b.sess != nil {
		m := b.sess.Meta()
		return map[string]string{
			"model":  m.Model,
			"branch": m.Branch,
			"cwd":    m.CWD,
		}
	}
	return nil
}

func New() *Backend {
	return &Backend{
		events: make(chan session.Event, 100),
	}
}

func (b *Backend) Name() string {
	return "native"
}

func (b *Backend) Bootstrap() backend.Bootstrap {
	return backend.Bootstrap{
		Entries: []session.Entry{
			{Role: session.RoleSystem, Content: "Native Ion Session (Gemini)"},
		},
		Status: "Initializing API client...",
	}
}

func (b *Backend) Session() session.AgentSession {
	return b
}

func (b *Backend) Open(ctx context.Context) error {
	apiKey := os.Getenv("GEMINI_API_KEY")
	if apiKey == "" {
		apiKey = os.Getenv("GOOGLE_API_KEY")
	}
	if apiKey == "" {
		return fmt.Errorf("GEMINI_API_KEY or GOOGLE_API_KEY not set")
	}

	client, err := genai.NewClient(ctx, option.WithAPIKey(apiKey))
	if err != nil {
		return err
	}
	b.client = client
	
	modelName := os.Getenv("ION_MODEL")
	if modelName == "" {
		modelName = "gemini-2.0-flash"
	}
	
	b.model = client.GenerativeModel(modelName)
	b.cs = b.model.StartChat()
	
	b.events <- session.EventStatusChanged{BaseEvent: session.BaseEvent{}, Status: fmt.Sprintf("Connected to %s", modelName)}
	return nil
}

func (b *Backend) Resume(ctx context.Context, sessionID string) error {
	return nil
}

func (b *Backend) SubmitTurn(ctx context.Context, input string) error {
	if b.cs == nil {
		return fmt.Errorf("session not opened")
	}

	b.events <- session.EventTurnStarted{BaseEvent: session.BaseEvent{}}
	b.events <- session.EventStatusChanged{BaseEvent: session.BaseEvent{}, Status: "Gemini is thinking..."}

	go func() {
		iter := b.cs.SendMessageStream(ctx, genai.Text(input))
		for {
			resp, err := iter.Next()
			if err == iterator.Done {
				break
			}
			if err != nil {
				b.events <- session.EventError{BaseEvent: session.BaseEvent{}, Error: err, Fatal: false}
				break
			}

			for _, cand := range resp.Candidates {
				if cand.Content != nil {
					for _, part := range cand.Content.Parts {
						if text, ok := part.(genai.Text); ok {
							b.events <- session.EventAssistantDelta{BaseEvent: session.BaseEvent{}, Delta: string(text)}
						}
					}
				}
			}
		}
		
		b.events <- session.EventAssistantMessage{BaseEvent: session.BaseEvent{}, Message: ""} // Commit
		b.events <- session.EventStatusChanged{BaseEvent: session.BaseEvent{}, Status: "Ready"}
		b.events <- session.EventTurnFinished{BaseEvent: session.BaseEvent{}}
	}()

	return nil
}

func (b *Backend) CancelTurn(ctx context.Context) error {
	return nil
}

func (b *Backend) Close() error {
	if b.client != nil {
		b.client.Close()
	}
	close(b.events)
	return nil
}

func (b *Backend) Events() <-chan session.Event {
	return b.events
}
