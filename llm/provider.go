package llm

import "context"

// Provider defines the interface for an LLM backend.
type Provider interface {
	// ID returns the unique identifier for this provider.
	ID() string

	// Generate executes a non-streaming completion request. Providers receive a
	// neutral request draft and should prepare a provider-specific copy with
	// PrepareRequestForCapabilities before converting it to wire format.
	Generate(ctx context.Context, req *Request) (*Response, error)

	// Stream executes a streaming completion request. Providers receive a neutral
	// request draft and should prepare a provider-specific copy with
	// PrepareRequestForCapabilities before converting it to wire format.
	Stream(ctx context.Context, req *Request) (Stream, error)

	// Models returns the list of models supported by this provider.
	Models(ctx context.Context) ([]Model, error)

	// CountTokens returns the number of tokens in the given messages for a specific model.
	CountTokens(ctx context.Context, model string, messages []Message) (int, error)

	// Cost calculates the cost in USD for the given usage on a specific model.
	Cost(ctx context.Context, model string, usage Usage) float64

	// Capabilities returns the feature set supported by the given model.
	Capabilities(model string) Capabilities

	// IsTransient returns true if the given error is retryable (e.g. 429, 503).
	IsTransient(err error) bool

	// IsContextOverflow returns true if the error indicates the model's context
	// window was exceeded (e.g. context_length_exceeded, 400 bad request with
	// overflow message).
	IsContextOverflow(err error) bool
}

// ServerCompactor is an optional capability for providers that support native
// server-side context pruning or compaction.
type ServerCompactor interface {
	// ServerCompact delegates context compaction to the provider backend.
	ServerCompact(ctx context.Context, req *ServerCompactRequest) (*ServerCompactResult, error)
}

// ServerCompactRequest carries context and hints for provider-delegated compaction.
type ServerCompactRequest struct {
	Model              string
	Messages           []Message
	System             string
	Hint               string
	CustomInstructions string
	ReserveTokens      int
}

// ServerCompactResult carries the server-compacted summary and observed usage.
type ServerCompactResult struct {
	Summary string
	Usage   Usage
}
