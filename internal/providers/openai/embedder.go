package openai

import (
	"context"
	"fmt"

	"github.com/nijaru/ion/internal/llm"
	"github.com/sashabaranov/go-openai"
)

// Embedder implements llm.Embedder using the OpenAI embeddings API.
type Embedder struct {
	client *openai.Client
	model  openai.EmbeddingModel
}

// NewEmbedder returns an llm.Embedder backed by the OpenAI embeddings API.
// If model is empty, text-embedding-3-small is used.
func NewEmbedder(apiKey, model string) llm.Embedder {
	m := openai.EmbeddingModel(model)
	if m == "" {
		m = openai.SmallEmbedding3
	}
	return &Embedder{
		client: openai.NewClient(apiKey),
		model:  m,
	}
}

// EmbedContent returns the embedding vector for the given text.
func (e *Embedder) EmbedContent(ctx context.Context, text string) ([]float32, error) {
	resp, err := e.client.CreateEmbeddings(ctx, openai.EmbeddingRequest{
		Input: []string{text},
		Model: e.model,
	})
	if err != nil {
		return nil, fmt.Errorf("openai embed: %w", err)
	}
	if len(resp.Data) == 0 {
		return nil, fmt.Errorf("openai embed: empty response")
	}
	return resp.Data[0].Embedding, nil
}
