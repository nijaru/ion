package canto

import (
	"context"
	"errors"

	"github.com/nijaru/canto/llm"
	ctesting "github.com/nijaru/canto/x/testing"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

type compactProvider struct {
	id          string
	lastRequest *llm.Request
}

func (p *compactProvider) ID() string { return p.id }

func (p *compactProvider) Generate(ctx context.Context, req *llm.Request) (*llm.Response, error) {
	p.lastRequest = req
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
}

func (p *reasoningCapProvider) Capabilities(model string) llm.Capabilities {
	caps := llm.DefaultCapabilities()
	caps.ReasoningEffort = p.reasoningEffort
	if p.reasoningEffort {
		caps.Reasoning = llm.ReasoningCapabilities{
			Kind:       llm.ReasoningKindEffort,
			Efforts:    []string{"minimal", "low", "medium", "high"},
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

type proactiveUsageSession struct {
	id       string
	meta     storage.Metadata
	usageIn  int
	usageOut int
}

func (p *retryProvider) IsTransient(err error) bool {
	return errors.Is(err, transientStreamErr)
}

func (p *retryProvider) IsContextOverflow(err error) bool { return false }

type overflowRecoveryProvider struct {
	*ctesting.FauxProvider
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

func (p *overflowRecoveryProvider) IsContextOverflow(err error) bool {
	return errors.Is(err, overflowErr)
}

func (s *proactiveUsageSession) ID() string                                  { return s.id }
func (s *proactiveUsageSession) Meta() storage.Metadata                      { return s.meta }
func (s *proactiveUsageSession) Append(ctx context.Context, event any) error { return nil }
func (s *proactiveUsageSession) Entries(ctx context.Context) ([]ionsession.Entry, error) {
	return nil, nil
}
func (s *proactiveUsageSession) LastStatus(ctx context.Context) (string, error) { return "", nil }
func (s *proactiveUsageSession) Usage(ctx context.Context) (int, int, float64, error) {
	return s.usageIn, s.usageOut, 0, nil
}
func (s *proactiveUsageSession) Close() error { return nil }
