package llm

import (
	"context"
	"fmt"
	"sync"
)

// FauxStep is one scripted response returned by FauxProvider.
type FauxStep struct {
	Content        string
	Reasoning      string
	ThinkingBlocks []ThinkingBlock
	Calls          []Call
	Usage          Usage
	Err            error
	// Chunks, if set, causes Stream to return these chunks instead of a single
	// synthesized chunk from Content and Calls.
	Chunks []Chunk
}

// FauxProvider is a deterministic in-memory Provider for examples and tests.
// It consumes scripted steps in order and never performs network I/O.
type FauxProvider struct {
	mu    sync.Mutex
	id    string
	steps []FauxStep
	pos   int
	calls []*Request

	IsContextOverflowFn func(error) bool
	IsTransientFn       func(error) bool
}

// NewFauxProvider creates a deterministic provider with scripted responses.
func NewFauxProvider(id string, steps ...FauxStep) *FauxProvider {
	if id == "" {
		id = "faux"
	}
	return &FauxProvider{id: id, steps: append([]FauxStep(nil), steps...)}
}

func (p *FauxProvider) ID() string { return p.id }

func (p *FauxProvider) Generate(_ context.Context, req *Request) (*Response, error) {
	step, err := p.next(req)
	if err != nil {
		return nil, err
	}
	if step.Err != nil {
		return nil, step.Err
	}
	return &Response{
		Content:        step.Content,
		Reasoning:      step.Reasoning,
		ThinkingBlocks: step.ThinkingBlocks,
		Calls:          append([]Call(nil), step.Calls...),
		Usage:          step.Usage,
	}, nil
}

func (p *FauxProvider) Stream(_ context.Context, req *Request) (Stream, error) {
	step, err := p.next(req)
	if err != nil {
		return nil, err
	}
	if step.Err != nil {
		return nil, step.Err
	}
	chunks := append([]Chunk(nil), step.Chunks...)
	if chunks == nil {
		chunks = []Chunk{{
			Content:        step.Content,
			Reasoning:      step.Reasoning,
			ThinkingBlocks: append([]ThinkingBlock(nil), step.ThinkingBlocks...),
			Calls:          append([]Call(nil), step.Calls...),
			Usage:          &step.Usage,
		}}
	}
	return &FauxStream{chunks: chunks}, nil
}

func (p *FauxProvider) next(req *Request) (FauxStep, error) {
	p.mu.Lock()
	defer p.mu.Unlock()

	p.calls = append(p.calls, req)
	if p.pos >= len(p.steps) {
		return FauxStep{}, fmt.Errorf(
			"faux provider %q: no more steps (called %d times, have %d)",
			p.id,
			p.pos+1,
			len(p.steps),
		)
	}
	step := p.steps[p.pos]
	p.pos++
	return step, nil
}

func (p *FauxProvider) Models(_ context.Context) ([]Model, error) {
	return nil, nil
}

func (p *FauxProvider) CountTokens(_ context.Context, _ string, messages []Message) (int, error) {
	total := 0
	for _, msg := range messages {
		total += len(msg.TextContent())
	}
	return total, nil
}

func (p *FauxProvider) Cost(_ context.Context, _ string, usage Usage) float64 {
	return usage.Cost
}

func (p *FauxProvider) Capabilities(_ string) Capabilities {
	return DefaultCapabilities()
}

func (p *FauxProvider) IsTransient(err error) bool {
	return p.IsTransientFn != nil && p.IsTransientFn(err)
}

func (p *FauxProvider) IsContextOverflow(err error) bool {
	return p.IsContextOverflowFn != nil && p.IsContextOverflowFn(err)
}

// Calls returns requests processed by the provider.
func (p *FauxProvider) Calls() []*Request {
	p.mu.Lock()
	defer p.mu.Unlock()
	out := make([]*Request, len(p.calls))
	copy(out, p.calls)
	return out
}

// Remaining returns the number of unconsumed scripted steps.
func (p *FauxProvider) Remaining() int {
	p.mu.Lock()
	defer p.mu.Unlock()
	return len(p.steps) - p.pos
}

// FauxStream is a deterministic Stream over scripted chunks.
type FauxStream struct {
	chunks []Chunk
	pos    int
	err    error
}

func NewFauxStream(chunks ...Chunk) *FauxStream {
	return &FauxStream{chunks: append([]Chunk(nil), chunks...)}
}

func (s *FauxStream) Next() (*Chunk, bool) {
	if s.pos >= len(s.chunks) {
		return nil, false
	}
	chunk := s.chunks[s.pos]
	s.pos++
	return &chunk, true
}

func (s *FauxStream) Err() error   { return s.err }
func (s *FauxStream) Close() error { return nil }
