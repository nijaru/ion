package anthropic

import "github.com/nijaru/ion/llm"

// DefaultModelCaps returns capability entries for Anthropic models that
// support extended thinking. Merge with your own overrides as needed.
func DefaultModelCaps() map[string]llm.Capabilities {
	return map[string]llm.Capabilities{}
}

// Capabilities returns the feature set for the given model.
// It consults the model caps map first, then p.Config.Models, then gets DefaultCapabilities.
func (p *Provider) Capabilities(model string) llm.Capabilities {
	if p.modelCaps != nil {
		if caps, ok := p.modelCaps[model]; ok {
			return caps
		}
	}
	for _, m := range p.config.Models {
		if m.ID == model && m.Capabilities != nil {
			return *m.Capabilities
		}
	}
	return llm.DefaultCapabilities()
}
