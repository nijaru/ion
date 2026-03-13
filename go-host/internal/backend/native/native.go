package native

import (
	"context"
	"fmt"
	"os"

	"github.com/google/generative-ai-go/genai"
	"google.golang.org/api/iterator"
	"google.golang.org/api/option"

	"github.com/nijaru/ion/go-host/internal/backend"
	"github.com/nijaru/ion/go-host/internal/session"
)

type Backend struct {
	events chan session.Event
	client *genai.Client
	model  *genai.GenerativeModel
	cs     *genai.ChatSession
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
	
	b.events <- session.EventStatusChanged{Status: fmt.Sprintf("Connected to %s", modelName)}
	return nil
}

func (b *Backend) Resume(ctx context.Context, sessionID string) error {
	return nil
}

func (b *Backend) SubmitTurn(ctx context.Context, input string) error {
	if b.cs == nil {
		return fmt.Errorf("session not opened")
	}

	b.events <- session.EventTurnStarted{}
	b.events <- session.EventStatusChanged{Status: "Gemini is thinking..."}

	go func() {
		iter := b.cs.SendMessageStream(ctx, genai.Text(input))
		for {
			resp, err := iter.Next()
			if err == iterator.Done {
				break
			}
			if err != nil {
				b.events <- session.EventError{Error: err, Fatal: false}
				break
			}

			for _, cand := range resp.Candidates {
				if cand.Content != nil {
					for _, part := range cand.Content.Parts {
						if text, ok := part.(genai.Text); ok {
							b.events <- session.EventAssistantDelta{Delta: string(text)}
						}
					}
				}
			}
		}
		
		b.events <- session.EventAssistantMessage{Message: ""} // Commit
		b.events <- session.EventStatusChanged{Status: "Ready"}
		b.events <- session.EventTurnFinished{}
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
