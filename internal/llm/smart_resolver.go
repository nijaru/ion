package llm

import (
	"context"
	"fmt"
	"sync"
	"sync/atomic"
	"time"
)

// Strategy defines how a SmartResolver picks providers.
type Strategy string

const (
	StrategyPriority   Strategy = "priority"
	StrategyRoundRobin Strategy = "round-robin"
)

type managedProvider struct {
	provider Provider
	cooling  time.Time
	failures int
}

// SmartResolver tracks provider health and rotates/fails over among providers.
type SmartResolver struct {
	mu        sync.RWMutex
	providers []*managedProvider
	strategy  Strategy
	lastIdx   uint32
}

// NewSmartResolver creates a new smart resolver.
func NewSmartResolver(strategy Strategy, providers ...Provider) *SmartResolver {
	managed := make([]*managedProvider, len(providers))
	for i, p := range providers {
		managed[i] = &managedProvider{provider: p}
	}
	return &SmartResolver{
		providers: managed,
		strategy:  strategy,
	}
}

func (r *SmartResolver) ID() string {
	if len(r.providers) == 0 {
		return "smart"
	}
	return fmt.Sprintf("smart(%s)", r.providers[0].provider.ID())
}

func (r *SmartResolver) Generate(ctx context.Context, req *Request) (*Response, error) {
	healthy := r.getHealthy()
	if len(healthy) == 0 {
		return nil, fmt.Errorf("all providers are cooling down")
	}

	var lastErr error
	for _, p := range healthy {
		resp, err := p.provider.Generate(ctx, req)
		if err == nil {
			r.markSuccess(p)
			return resp, nil
		}
		lastErr = err

		if p.provider.IsTransient(err) {
			r.markCooling(p)
			continue
		}

		return nil, lastErr
	}

	return nil, fmt.Errorf("all healthy providers exhausted or rate limited: %w", lastErr)
}

func (r *SmartResolver) Stream(ctx context.Context, req *Request) (Stream, error) {
	healthy := r.getHealthy()
	if len(healthy) == 0 {
		return nil, fmt.Errorf("all providers are cooling down")
	}

	var lastErr error
	for _, p := range healthy {
		s, err := p.provider.Stream(ctx, req)
		if err == nil {
			return &smartResolverStream{
				Stream:   s,
				resolver: r,
				provider: p,
			}, nil
		}
		lastErr = err

		if p.provider.IsTransient(err) {
			r.markCooling(p)
			continue
		}

		return nil, lastErr
	}

	return nil, fmt.Errorf("all healthy providers exhausted or rate limited: %w", lastErr)
}

type smartResolverStream struct {
	Stream
	resolver *SmartResolver
	provider *managedProvider
	once     sync.Once
}

func (s *smartResolverStream) Err() error {
	err := s.Stream.Err()
	s.finish(err)
	return err
}

func (s *smartResolverStream) Close() error {
	err := s.Stream.Close()
	if err != nil {
		s.finish(err)
		return err
	}
	s.finish(s.Stream.Err())
	return err
}

func (s *smartResolverStream) finish(err error) {
	s.once.Do(func() {
		if err != nil && s.provider.provider.IsTransient(err) {
			s.resolver.markCooling(s.provider)
			return
		}
		s.resolver.markSuccess(s.provider)
	})
}

func (r *SmartResolver) Models(ctx context.Context) ([]Model, error) {
	seen := make(map[string]bool)
	var all []Model
	r.mu.RLock()
	defer r.mu.RUnlock()
	for _, p := range r.providers {
		models, err := p.provider.Models(ctx)
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

func (r *SmartResolver) CountTokens(
	ctx context.Context,
	model string,
	messages []Message,
) (int, error) {
	healthy := r.getHealthy()
	if len(healthy) == 0 {
		r.mu.RLock()
		defer r.mu.RUnlock()
		if len(r.providers) > 0 {
			return r.providers[0].provider.CountTokens(ctx, model, messages)
		}
		return 0, fmt.Errorf("no providers available")
	}
	return healthy[0].provider.CountTokens(ctx, model, messages)
}

func (r *SmartResolver) Cost(ctx context.Context, model string, usage Usage) float64 {
	r.mu.RLock()
	defer r.mu.RUnlock()
	for _, p := range r.providers {
		models, err := p.provider.Models(ctx)
		if err != nil {
			continue
		}
		for _, m := range models {
			if string(m.ID) == model {
				return p.provider.Cost(ctx, model, usage)
			}
		}
	}
	if len(r.providers) > 0 {
		return r.providers[0].provider.Cost(ctx, model, usage)
	}
	return 0
}

// Capabilities returns the capabilities of the first healthy provider's view
// of the given model.
func (r *SmartResolver) Capabilities(model string) Capabilities {
	providers := r.getHealthy()
	if len(providers) == 0 {
		return DefaultCapabilities()
	}
	return providers[0].provider.Capabilities(model)
}

func (r *SmartResolver) getHealthy() []*managedProvider {
	r.mu.RLock()
	defer r.mu.RUnlock()

	now := time.Now()
	var healthy []*managedProvider
	for _, p := range r.providers {
		if p.cooling.Before(now) {
			healthy = append(healthy, p)
		}
	}

	if len(healthy) > 1 && r.strategy == StrategyRoundRobin {
		idx := int(atomic.AddUint32(&r.lastIdx, 1) % uint32(len(healthy)))
		res := make([]*managedProvider, len(healthy))
		for i := 0; i < len(healthy); i++ {
			res[i] = healthy[(idx+i)%len(healthy)]
		}
		return res
	}

	return healthy
}

func (r *SmartResolver) markCooling(p *managedProvider) {
	r.mu.Lock()
	defer r.mu.Unlock()
	p.failures++
	if p.failures > 10 {
		p.failures = 10
	}
	backoff := time.Duration(1<<uint(p.failures)) * time.Second
	if backoff > 5*time.Minute {
		backoff = 5 * time.Minute
	}
	p.cooling = time.Now().Add(backoff)
}

func (r *SmartResolver) markSuccess(p *managedProvider) {
	r.mu.Lock()
	defer r.mu.Unlock()
	p.failures = 0
	p.cooling = time.Time{}
}

// IsTransient returns true if any underlying provider reports a transient error.
func (r *SmartResolver) IsTransient(err error) bool {
	if err == nil {
		return false
	}
	r.mu.RLock()
	defer r.mu.RUnlock()
	for _, p := range r.providers {
		if p.provider.IsTransient(err) {
			return true
		}
	}
	return false
}

// IsContextOverflow returns true if any underlying provider reports a context
// overflow error.
func (r *SmartResolver) IsContextOverflow(err error) bool {
	if err == nil {
		return false
	}
	r.mu.RLock()
	defer r.mu.RUnlock()
	for _, p := range r.providers {
		if p.provider.IsContextOverflow(err) {
			return true
		}
	}
	return false
}
