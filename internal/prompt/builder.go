package prompt

import (
	"github.com/nijaru/ion/internal/tool"
)

// Builder implements the context engineering pipeline.
type Builder struct {
	requestProcessors []RequestProcessor
	mutators          []ContextMutator
}

// ToolRegistryProcessor is a request processor whose registry can be swapped
// for runtime-scoped prompt shaping.
type ToolRegistryProcessor interface {
	RequestProcessor
	WithToolRegistry(*tool.Registry) RequestProcessor
}

// NewBuilder creates a new builder with the default request-shaping chain.
func NewBuilder(processors ...RequestProcessor) *Builder {
	return &Builder{requestProcessors: append([]RequestProcessor(nil), processors...)}
}

// RequestProcessors returns a copy of the current request-shaping chain.
func (b *Builder) RequestProcessors() []RequestProcessor {
	res := make([]RequestProcessor, len(b.requestProcessors))
	copy(res, b.requestProcessors)
	return res
}

// Mutators returns a copy of the current commit-time mutator chain.
func (b *Builder) Mutators() []ContextMutator {
	res := make([]ContextMutator, len(b.mutators))
	copy(res, b.mutators)
	return res
}

// Clone returns a shallow copy of the builder with copied pipeline slices.
func (b *Builder) Clone() *Builder {
	if b == nil {
		return nil
	}
	return &Builder{
		requestProcessors: b.RequestProcessors(),
		mutators:          b.Mutators(),
	}
}
