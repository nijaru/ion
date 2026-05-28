package llm

import (
	"context"
	"encoding/json"
	"fmt"
)

// StandardClassifier implements Classifier by wrapping a standard LLM Provider.
// It uses structured outputs (JSON schema) to ensure deterministic labels.
type StandardClassifier struct {
	Provider Provider
	Model    string
	Prompt   string // System prompt describing the classification task
}

// NewStandardClassifier creates a classifier backed by a chat model.
func NewStandardClassifier(p Provider, model string, systemPrompt string) *StandardClassifier {
	return &StandardClassifier{
		Provider: p,
		Model:    model,
		Prompt:   systemPrompt,
	}
}

func (c *StandardClassifier) Classify(
	ctx context.Context,
	input string,
	labels []string,
) (*Classification, error) {
	if c.Provider == nil {
		return nil, fmt.Errorf("classifier: no provider configured")
	}

	schema := map[string]any{
		"type": "object",
		"properties": map[string]any{
			"label": map[string]any{
				"type": "string",
				"enum": labels,
			},
			"reason": map[string]any{
				"type": "string",
			},
		},
		"required":             []string{"label", "reason"},
		"additionalProperties": false,
	}

	req := &Request{
		Model: c.Model,
		Messages: []Message{
			{Role: RoleSystem, Content: c.Prompt},
			{Role: RoleUser, Content: input},
		},
		ResponseFormat: &ResponseFormat{
			Type:   ResponseFormatJSONSchema,
			Name:   "classification",
			Schema: schema,
			Strict: true,
		},
		Temperature: 0.0,
	}

	resp, err := c.Provider.Generate(ctx, req)
	if err != nil {
		return nil, err
	}

	var result struct {
		Label  string `json:"label"`
		Reason string `json:"reason"`
	}
	if err := json.Unmarshal([]byte(resp.Content), &result); err != nil {
		// Fallback for providers that ignore the schema or return malformed JSON.
		return nil, fmt.Errorf(
			"classifier: failed to parse response: %w (content: %q)",
			err,
			resp.Content,
		)
	}

	return &Classification{
		Label:  result.Label,
		Reason: result.Reason,
		Usage:  resp.Usage,
	}, nil
}
