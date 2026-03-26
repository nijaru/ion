package native

import (
	"context"
	"fmt"
	"os"

	"github.com/google/generative-ai-go/genai"
	"google.golang.org/api/iterator"
	"google.golang.org/api/option"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/registry"
	"github.com/nijaru/ion/internal/config"
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
	cfg     *config.Config
}

func (b *Backend) SetConfig(cfg *config.Config) {
	b.cfg = cfg
}

func (b *Backend) Provider() string {
	if b.cfg != nil && b.cfg.Provider != "" {
		return b.cfg.Provider
	}
	return "gemini"
}

func (b *Backend) Model() string {
	if b.cfg != nil && b.cfg.Model != "" {
		return b.cfg.Model
	}
	return os.Getenv("ION_MODEL")
}

func (b *Backend) ContextLimit() int {
	if b.cfg != nil && b.cfg.ContextLimit > 0 {
		return b.cfg.ContextLimit
	}
	provider := b.Provider()
	model := b.Model()
	if meta, ok := registry.GetMetadata(context.Background(), provider, model); ok {
		return meta.ContextLimit
	}
	return 0
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

func (b *Backend) Session() session.AgentSession {
	return b
}

func (b *Backend) Bootstrap() backend.Bootstrap {
	status := "Ready"
	if b.sess != nil {
		if s, err := b.sess.LastStatus(context.Background()); err == nil && s != "" {
			status = s
		} else {
			// New session
			status = fmt.Sprintf("Connected to %s", b.Model())
		}
	}
	return backend.Bootstrap{
		Entries: []session.Entry{},
		Status:  status,
	}
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

	modelName := b.Model()

	b.model = client.GenerativeModel(modelName)
	b.cs = b.model.StartChat()

	return nil
}

func (b *Backend) Resume(ctx context.Context, sessionID string) error {
	return nil
}

func (b *Backend) SubmitTurn(ctx context.Context, input string) error {
	if b.cs == nil {
		return fmt.Errorf("session not opened")
	}

	b.events <- session.TurnStarted{}
	b.events <- session.StatusChanged{Status: "Gemini is thinking..."}

	go func() {
		iter := b.cs.SendMessageStream(ctx, genai.Text(input))
		for {
			resp, err := iter.Next()
			if err == iterator.Done {
				break
			}
			if err != nil {
				b.events <- session.Error{Err: err, Fatal: false}
				break
			}

			for _, cand := range resp.Candidates {
				if cand.Content != nil {
					for _, part := range cand.Content.Parts {
						if text, ok := part.(genai.Text); ok {
							b.events <- session.AssistantDelta{Delta: string(text)}
						}
					}
				}
			}
		}

		b.events <- session.AssistantMessage{Message: ""} // Commit
		b.events <- session.StatusChanged{Status: "Ready"}
		b.events <- session.TurnFinished{}
	}()

	return nil
}

func (b *Backend) CancelTurn(ctx context.Context) error {
	return nil
}

func (b *Backend) Approve(ctx context.Context, requestID string, approved bool) error {
	return fmt.Errorf("approvals not supported in native backend")
}

func (b *Backend) RegisterMCPServer(ctx context.Context, command string, args ...string) error {
	return fmt.Errorf("MCP not supported in native backend")
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
