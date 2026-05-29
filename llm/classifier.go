package llm

import "context"

// Classification describes the result of a discrete model judgment.
type Classification struct {
	// Label is the predicted class (e.g. "allow", "deny", "escalate").
	Label string
	// Reason is the model's justification for the label.
	Reason string
	// Usage is the token consumption for this judgment.
	Usage Usage
	// Metadata contains provider-specific or prompt-defined extra fields.
	Metadata map[string]any
}

// Classifier describes the interface for an LLM judgment task.
// It is intended for small, fast models acting as evaluators or routers.
type Classifier interface {
	// Classify executes a judgment against the given input.
	// labels is the set of valid output categories.
	Classify(ctx context.Context, input string, labels []string) (*Classification, error)
}
