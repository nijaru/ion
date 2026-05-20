package canto

import (
	"context"
	"errors"
	"sync"

	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/prompt"
	ctesting "github.com/nijaru/canto/x/testing"
)

type compactProvider struct {
	mu          sync.Mutex
	id          string
	lastRequest *llm.Request
}

func (p *compactProvider) ID() string { return p.id }

func (p *compactProvider) Generate(ctx context.Context, req *llm.Request) (*llm.Response, error) {
	p.mu.Lock()
	p.lastRequest = req
	p.mu.Unlock()
	return &llm.Response{Content: "condensed summary"}, nil
}

func (p *compactProvider) Stream(ctx context.Context, req *llm.Request) (llm.Stream, error) {
	return nil, nil
}

func (p *compactProvider) Models(ctx context.Context) ([]llm.Model, error) {
	return nil, nil
}

func (p *compactProvider) CountTokens(
	ctx context.Context,
	model string,
	messages []llm.Message,
) (int, error) {
	return 10_000, nil
}

func (p *compactProvider) Cost(
	ctx context.Context,
	model string,
	usage llm.Usage,
) float64 {
	return 0
}

func (p *compactProvider) Capabilities(model string) llm.Capabilities {
	return llm.DefaultCapabilities()
}

func (p *compactProvider) IsTransient(err error) bool { return false }

func (p *compactProvider) IsContextOverflow(err error) bool { return false }

type reasoningCapProvider struct {
	compactProvider
	reasoningEffort bool
	reasoningToggle bool
}

func (p *reasoningCapProvider) Capabilities(model string) llm.Capabilities {
	caps := llm.DefaultCapabilities()
	if p.reasoningEffort {
		caps.Reasoning = llm.ReasoningCapabilities{
			Kind:       llm.ReasoningKindEffort,
			Efforts:    []string{"minimal", "low", "medium", "high"},
			CanDisable: true,
		}
	}
	if p.reasoningToggle {
		caps.Reasoning = llm.ReasoningCapabilities{
			Kind:       llm.ReasoningKindBoolean,
			CanDisable: true,
		}
	}
	return caps
}

var (
	transientStreamErr = errors.New("transient provider failure")
	overflowErr        = errors.New("context_length_exceeded")
)

type retryProvider struct {
	*ctesting.FauxProvider
}

func (p *retryProvider) IsTransient(err error) bool {
	return errors.Is(err, transientStreamErr)
}

func (p *retryProvider) IsContextOverflow(err error) bool { return false }

type overflowRecoveryProvider struct {
	*ctesting.FauxProvider
}

type heuristicCountProvider struct {
	*ctesting.FauxProvider
}

type fixedCountProvider struct {
	*ctesting.FauxProvider
	tokens int
}

type blockingCountProvider struct {
	compactProvider
	entered chan struct{}
	once    sync.Once
}

type blockingFirstCountProvider struct {
	*ctesting.FauxProvider
	mu             sync.Mutex
	id             string
	tokens         int
	entered        chan struct{}
	release        chan struct{}
	once           sync.Once
	generateModels []string
}

type blockingStreamProvider struct {
	compactProvider
	streamCtx chan context.Context
}

type contextBlockingStream struct {
	ctx context.Context
}

func (p *blockingStreamProvider) Stream(ctx context.Context, req *llm.Request) (llm.Stream, error) {
	p.streamCtx <- ctx
	return &contextBlockingStream{ctx: ctx}, nil
}

func (s *contextBlockingStream) Next() (*llm.Chunk, bool) {
	<-s.ctx.Done()
	return nil, false
}

func (s *contextBlockingStream) Err() error {
	return s.ctx.Err()
}

func (s *contextBlockingStream) Close() error { return nil }

type lateSuccessStreamProvider struct {
	compactProvider
	streamCtx chan context.Context
}

type lateSuccessStream struct {
	ctx  context.Context
	sent bool
}

func (p *lateSuccessStreamProvider) Stream(
	ctx context.Context,
	req *llm.Request,
) (llm.Stream, error) {
	p.streamCtx <- ctx
	return &lateSuccessStream{ctx: ctx}, nil
}

func (s *lateSuccessStream) Next() (*llm.Chunk, bool) {
	if s.sent {
		return nil, false
	}
	<-s.ctx.Done()
	s.sent = true
	return &llm.Chunk{Content: "late answer"}, true
}

func (s *lateSuccessStream) Err() error   { return nil }
func (s *lateSuccessStream) Close() error { return nil }

type testTool struct {
	name string
}

func (t *testTool) Spec() llm.Spec {
	return llm.Spec{Name: t.name}
}

func (t *testTool) Execute(ctx context.Context, args string) (string, error) {
	return "", nil
}

func (p *overflowRecoveryProvider) CountTokens(
	ctx context.Context,
	model string,
	messages []llm.Message,
) (int, error) {
	return 10_000, nil
}

func (p *heuristicCountProvider) CountTokens(
	ctx context.Context,
	model string,
	messages []llm.Message,
) (int, error) {
	return prompt.EstimateMessagesTokens(ctx, nil, model, messages), nil
}

func (p *fixedCountProvider) CountTokens(
	ctx context.Context,
	model string,
	messages []llm.Message,
) (int, error) {
	return p.tokens, nil
}

func (p *blockingCountProvider) CountTokens(
	ctx context.Context,
	model string,
	messages []llm.Message,
) (int, error) {
	p.once.Do(func() {
		close(p.entered)
	})
	<-ctx.Done()
	return 0, ctx.Err()
}

func (p *blockingFirstCountProvider) ID() string {
	return p.id
}

func (p *blockingFirstCountProvider) Generate(
	ctx context.Context,
	req *llm.Request,
) (*llm.Response, error) {
	p.mu.Lock()
	p.generateModels = append(p.generateModels, req.Model)
	p.mu.Unlock()
	return &llm.Response{Content: "condensed summary"}, nil
}

func (p *blockingFirstCountProvider) CountTokens(
	ctx context.Context,
	model string,
	messages []llm.Message,
) (int, error) {
	block := false
	p.once.Do(func() {
		close(p.entered)
		block = true
	})
	if block {
		select {
		case <-p.release:
		case <-ctx.Done():
			return 0, ctx.Err()
		}
	}
	return p.tokens, nil
}

func (p *blockingFirstCountProvider) GenerateModels() []string {
	p.mu.Lock()
	defer p.mu.Unlock()
	return append([]string(nil), p.generateModels...)
}

func (p *overflowRecoveryProvider) IsContextOverflow(err error) bool {
	return errors.Is(err, overflowErr)
}
