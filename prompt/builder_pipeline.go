package prompt

import (
	"context"
	"fmt"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// Build executes the commit-time pipeline to transform the session and request.
func (b *Builder) Build(
	ctx context.Context,
	p llm.Provider,
	model string,
	sess *session.Session,
	req *llm.Request,
) error {
	return b.BuildCommit(ctx, p, model, sess, req)
}

// BuildPreview builds a request using only preview-safe request processors.
func (b *Builder) BuildPreview(
	ctx context.Context,
	p llm.Provider,
	model string,
	sess *session.Session,
	req *llm.Request,
) error {
	return b.previewPipeline().BuildPreview(ctx, p, model, sess, req)
}

// BuildCommit runs commit-time mutation first and then rebuilds the request
// from the updated session state.
func (b *Builder) BuildCommit(
	ctx context.Context,
	p llm.Provider,
	model string,
	sess *session.Session,
	req *llm.Request,
) error {
	pipeline, err := b.commitPipeline()
	if err != nil {
		return err
	}
	return pipeline.BuildCommit(ctx, p, model, sess, req)
}

// Effects returns the aggregate side effects of the current mutator chain.
func (b *Builder) Effects() SideEffects {
	var effects SideEffects
	for _, proc := range b.requestProcessors {
		effects = effects.merge(requestProcessorEffects(proc))
	}
	for _, m := range b.mutators {
		effects = effects.merge(mutatorEffects(m))
	}
	return effects
}

func (b *Builder) previewPipeline() *Pipeline {
	return NewPipeline(b.requestProcessors...)
}

func (b *Builder) commitPipeline() (*Pipeline, error) {
	if err := validateCompactionOrder(b.mutators); err != nil {
		return nil, err
	}

	pipeline := NewPipeline(b.requestProcessors...)
	for _, m := range b.mutators {
		pipeline.AddMutator(m)
	}
	return pipeline, nil
}

func validateCompactionOrder(mutators []ContextMutator) error {
	var hasOffloader bool
	var hasSummarizer bool
	var seenSummarizer bool
	offloaderBeforeSummarizer := true

	for _, mutator := range mutators {
		if c, ok := mutator.(Compactor); ok {
			strategy := c.CompactionStrategy()
			if strategy == "offload" {
				hasOffloader = true
				if seenSummarizer {
					offloaderBeforeSummarizer = false
				}
			} else if strategy == "summarize" {
				hasSummarizer = true
				seenSummarizer = true
			}
		}
	}

	if hasSummarizer && !hasOffloader {
		return fmt.Errorf(
			"commit pipeline: compaction requires offloader before summarizer (never skip to summarize)",
		)
	}
	if hasSummarizer && hasOffloader && !offloaderBeforeSummarizer {
		return fmt.Errorf(
			"commit pipeline: compaction requires offloader to run before summarizer",
		)
	}
	return nil
}
