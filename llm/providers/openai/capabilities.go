package openai

import (
	"github.com/nijaru/ion/llm"
)

// Capabilities returns the feature set for the given model.
// It consults ModelCaps first, then b.Config.Models, then falls back to Ion's
// default model capability registry.
func (b *Base) Capabilities(model string) llm.Capabilities {
	var caps llm.Capabilities
	found := false
	if b.ModelCaps != nil {
		if configured, ok := b.ModelCaps[model]; ok {
			caps = configured
			found = true
		}
	}
	for _, m := range b.Config.Models {
		if m.ID != model {
			continue
		}
		if m.Capabilities != nil {
			caps = *m.Capabilities
			found = true
		}
		break
	}
	if !found {
		if b.ModelRegistry != nil {
			caps = b.ModelRegistry.Resolve(model)
		} else {
			caps = llm.DefaultCapabilities()
		}
	}
	for _, m := range b.Config.Models {
		if m.ID == model && m.Compat != nil && m.Compat.SupportsVision != nil && !*m.Compat.SupportsVision {
			caps.InputModalities = []string{"text"}
			break
		}
	}
	return caps
}

// DefaultModelCaps returns capability entries for well-known OpenAI reasoning
// models. Pass to Base.ModelCaps (or merge with your own overrides) when
// constructing a provider that will use these models.
func DefaultModelCaps() map[string]llm.Capabilities {
	return map[string]llm.Capabilities{}
}
