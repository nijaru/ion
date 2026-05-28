package openai

import (
	"github.com/nijaru/ion/internal/llm"
)

// Capabilities returns the feature set for the given model.
// It consults ModelCaps first, then b.Config.Models, then falls back to Canto's default model capability registry.
func (b *Base) Capabilities(model string) llm.Capabilities {
	if b.ModelCaps != nil {
		if caps, ok := b.ModelCaps[model]; ok {
			return caps
		}
	}
	for _, m := range b.Config.Models {
		if m.ID == model && m.Capabilities != nil {
			return *m.Capabilities
		}
	}
	return llm.ResolveCapabilities(model)
}

// DefaultModelCaps returns capability entries for well-known OpenAI reasoning
// models. Pass to Base.ModelCaps (or merge with your own overrides) when
// constructing a provider that will use these models.
func DefaultModelCaps() map[string]llm.Capabilities {
	return map[string]llm.Capabilities{}
}
