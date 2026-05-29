package llm

import "context"

// Embedder defines the interface for creating vector embeddings from text content.
type Embedder interface {
	// EmbedContent converts a text string into a high-dimensional vector representation.
	EmbedContent(ctx context.Context, text string) ([]float32, error)
}
