package prompt

import (
	"context"

	"github.com/nijaru/ion/internal/llm"
	"github.com/nijaru/ion/internal/storage/session"
)

// RequestProcessor shapes the in-flight request without mutating durable state.
type RequestProcessor interface {
	ApplyRequest(
		ctx context.Context,
		p llm.Provider,
		model string,
		sess *session.Session,
		req *llm.Request,
	) error
}

// RequestProcessorFunc adapts a function to RequestProcessor.
type RequestProcessorFunc func(
	ctx context.Context,
	p llm.Provider,
	model string,
	sess *session.Session,
	req *llm.Request,
) error

func (f RequestProcessorFunc) ApplyRequest(
	ctx context.Context,
	p llm.Provider,
	model string,
	sess *session.Session,
	req *llm.Request,
) error {
	return f(ctx, p, model, sess, req)
}

// ContextMutator changes durable session or external state before request
// construction is rebuilt from the updated session state.
type ContextMutator interface {
	Mutate(ctx context.Context, p llm.Provider, model string, sess *session.Session) error
}

// Compactor indicates that the mutator performs compaction.
// It returns "offload" for reversible compaction and "summarize" for lossy compaction.
type Compactor interface {
	CompactionStrategy() string
}

// ContextMutatorFunc adapts a function to ContextMutator.
type ContextMutatorFunc func(
	ctx context.Context,
	p llm.Provider,
	model string,
	sess *session.Session,
) error

func (f ContextMutatorFunc) Mutate(
	ctx context.Context,
	p llm.Provider,
	model string,
	sess *session.Session,
) error {
	return f(ctx, p, model, sess)
}

// Pipeline separates preview-safe request shaping from commit-time mutation.
type Pipeline struct {
	mutators   []ContextMutator
	processors []RequestProcessor
}

// NewPipeline creates a phased context pipeline.
func NewPipeline(processors ...RequestProcessor) *Pipeline {
	return &Pipeline{processors: processors}
}

// AddMutator appends a commit-time mutator.
func (p *Pipeline) AddMutator(m ContextMutator) {
	p.mutators = append(p.mutators, m)
}

// AddRequestProcessor appends a preview-safe request processor.
func (p *Pipeline) AddRequestProcessor(r RequestProcessor) {
	p.processors = append(p.processors, r)
}

// BuildPreview shapes the request without running mutators.
func (p *Pipeline) BuildPreview(
	ctx context.Context,
	provider llm.Provider,
	model string,
	sess *session.Session,
	req *llm.Request,
) error {
	for _, rp := range p.processors {
		if err := rp.ApplyRequest(ctx, provider, model, sess, req); err != nil {
			return err
		}
	}
	return nil
}

// BuildCommit runs mutators first, then rebuilds the request from the updated
// session state using the request processors.
func (p *Pipeline) BuildCommit(
	ctx context.Context,
	provider llm.Provider,
	model string,
	sess *session.Session,
	req *llm.Request,
) error {
	for _, m := range p.mutators {
		if err := m.Mutate(ctx, provider, model, sess); err != nil {
			return err
		}
	}
	return p.BuildPreview(ctx, provider, model, sess, req)
}
