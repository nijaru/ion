package canto

import (
	"context"
	"slices"
	"sync"

	"github.com/nijaru/canto/llm"
)

type providerRequestObserver func(provider string, req *llm.Request)

var providerRequestObserverState struct {
	mu       sync.RWMutex
	observer providerRequestObserver
}

// SetProviderRequestObserverForTest installs a provider request observer and
// returns a restore function. It is intended for live smoke tests that need to
// prove the provider-visible history without depending on model wording.
func SetProviderRequestObserverForTest(observer func(provider string, req *llm.Request)) func() {
	providerRequestObserverState.mu.Lock()
	previous := providerRequestObserverState.observer
	providerRequestObserverState.observer = observer
	providerRequestObserverState.mu.Unlock()

	return func() {
		providerRequestObserverState.mu.Lock()
		providerRequestObserverState.observer = previous
		providerRequestObserverState.mu.Unlock()
	}
}

func observeProviderRequests(p llm.Provider) llm.Provider {
	if p == nil || !hasProviderRequestObserver() {
		return p
	}
	return requestObservingProvider{Provider: p}
}

func hasProviderRequestObserver() bool {
	providerRequestObserverState.mu.RLock()
	defer providerRequestObserverState.mu.RUnlock()
	return providerRequestObserverState.observer != nil
}

func notifyProviderRequest(provider string, req *llm.Request) {
	providerRequestObserverState.mu.RLock()
	observer := providerRequestObserverState.observer
	providerRequestObserverState.mu.RUnlock()
	if observer == nil {
		return
	}
	observer(provider, cloneProviderRequest(req))
}

type requestObservingProvider struct {
	llm.Provider
}

func (p requestObservingProvider) Generate(ctx context.Context, req *llm.Request) (*llm.Response, error) {
	notifyProviderRequest(p.ID(), req)
	return p.Provider.Generate(ctx, req)
}

func (p requestObservingProvider) Stream(ctx context.Context, req *llm.Request) (llm.Stream, error) {
	notifyProviderRequest(p.ID(), req)
	return p.Provider.Stream(ctx, req)
}

func cloneProviderRequest(req *llm.Request) *llm.Request {
	if req == nil {
		return nil
	}
	cloned := *req
	cloned.Messages = slices.Clone(req.Messages)
	for i := range cloned.Messages {
		cloned.Messages[i].ThinkingBlocks = slices.Clone(req.Messages[i].ThinkingBlocks)
		cloned.Messages[i].Calls = slices.Clone(req.Messages[i].Calls)
	}
	cloned.Tools = slices.Clone(req.Tools)
	if req.ResponseFormat != nil {
		format := *req.ResponseFormat
		format.Schema = cloneStringAnyMap(req.ResponseFormat.Schema)
		cloned.ResponseFormat = &format
	}
	return &cloned
}

func cloneStringAnyMap(src map[string]any) map[string]any {
	if len(src) == 0 {
		return nil
	}
	dst := make(map[string]any, len(src))
	for key, value := range src {
		dst[key] = value
	}
	return dst
}
