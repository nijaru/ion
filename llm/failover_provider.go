package llm

import (
	"context"
	"fmt"
)

// FailoverProvider tries a list of providers in sequence until one succeeds.
type FailoverProvider struct {
	providers []Provider
}

// NewFailoverProvider creates a new failover provider.
func NewFailoverProvider(providers ...Provider) *FailoverProvider {
	return &FailoverProvider{providers: providers}
}

func (p *FailoverProvider) ID() string {
	if len(p.providers) == 0 {
		return "failover"
	}
	return fmt.Sprintf("failover(%s)", p.providers[0].ID())
}

func (p *FailoverProvider) Generate(ctx context.Context, req *Request) (*Response, error) {
	if len(p.providers) == 0 {
		return nil, fmt.Errorf("no providers configured")
	}
	var lastErr error
	for _, sub := range p.providers {
		resp, err := sub.Generate(ctx, req)
		if err == nil {
			return resp, nil
		}
		lastErr = err
	}
	return nil, fmt.Errorf("failover failed: %w", lastErr)
}

func (p *FailoverProvider) Stream(ctx context.Context, req *Request) (Stream, error) {
	if len(p.providers) == 0 {
		return nil, fmt.Errorf("no providers configured")
	}
	var lastErr error
	for _, sub := range p.providers {
		s, err := sub.Stream(ctx, req)
		if err == nil {
			return s, nil
		}
		lastErr = err
	}
	return nil, fmt.Errorf("failover failed to start stream: %w", lastErr)
}

func (p *FailoverProvider) Models(ctx context.Context) ([]Model, error) {
	seen := make(map[string]bool)
	var all []Model
	for _, sub := range p.providers {
		models, err := sub.Models(ctx)
		if err != nil {
			continue
		}
		for _, m := range models {
			if !seen[m.ID] {
				seen[m.ID] = true
				all = append(all, m)
			}
		}
	}
	return all, nil
}

func (p *FailoverProvider) CountTokens(
	ctx context.Context,
	model string,
	messages []Message,
) (int, error) {
	if len(p.providers) == 0 {
		return 0, fmt.Errorf("no providers configured")
	}
	return p.providers[0].CountTokens(ctx, model, messages)
}

func (p *FailoverProvider) Cost(ctx context.Context, model string, usage Usage) float64 {
	for _, sub := range p.providers {
		models, err := sub.Models(ctx)
		if err != nil {
			continue
		}
		for _, m := range models {
			if string(m.ID) == model {
				return sub.Cost(ctx, model, usage)
			}
		}
	}
	if len(p.providers) > 0 {
		return p.providers[0].Cost(ctx, model, usage)
	}
	return 0
}

func (p *FailoverProvider) Capabilities(model string) Capabilities {
	if len(p.providers) == 0 {
		return DefaultCapabilities()
	}
	return p.providers[0].Capabilities(model)
}

// IsTransient returns true if any underlying provider reports a transient error.
func (p *FailoverProvider) IsTransient(err error) bool {
	if err == nil {
		return false
	}
	for _, sub := range p.providers {
		if sub.IsTransient(err) {
			return true
		}
	}
	return false
}

// IsContextOverflow returns true if any underlying provider reports a context
// overflow error.
func (p *FailoverProvider) IsContextOverflow(err error) bool {
	if err == nil {
		return false
	}
	for _, sub := range p.providers {
		if sub.IsContextOverflow(err) {
			return true
		}
	}
	return false
}
