package prompt

import "github.com/nijaru/ion/internal/tool"

// PrependRequestProcessors inserts preview-safe request processors at the
// front of the request-shaping chain.
func (b *Builder) PrependRequestProcessors(processors ...RequestProcessor) {
	if len(processors) == 0 {
		return
	}
	b.requestProcessors = append(
		append([]RequestProcessor(nil), processors...),
		b.requestProcessors...,
	)
}

// AppendRequestProcessors adds preview-safe request processors after the
// existing prompt shapers and before cache alignment.
func (b *Builder) AppendRequestProcessors(processors ...RequestProcessor) {
	if len(processors) == 0 {
		return
	}
	if idx := b.cacheBoundaryIndex(); idx >= 0 {
		merged := make([]RequestProcessor, 0, len(b.requestProcessors)+len(processors))
		merged = append(merged, b.requestProcessors[:idx]...)
		merged = append(merged, processors...)
		merged = append(merged, b.requestProcessors[idx:]...)
		b.requestProcessors = merged
		return
	}
	b.requestProcessors = append(b.requestProcessors, processors...)
}

// InsertRequestProcessorsBeforeLast inserts preview-safe request processors
// immediately before the last request processor. If the chain is empty, it
// appends them.
func (b *Builder) InsertRequestProcessorsBeforeLast(processors ...RequestProcessor) {
	if len(processors) == 0 {
		return
	}
	if len(b.requestProcessors) == 0 {
		b.AppendRequestProcessors(processors...)
		return
	}
	n := len(b.requestProcessors)
	tail := b.requestProcessors[n-1]
	merged := make([]RequestProcessor, 0, n-1+len(processors)+1)
	merged = append(merged, b.requestProcessors[:n-1]...)
	merged = append(merged, processors...)
	merged = append(merged, tail)
	b.requestProcessors = merged
}

// InsertRequestProcessorsBeforeCache inserts preview-safe request processors
// before cache alignment. This is the usual
// insertion point for host prompt and tool processors because cache markers
// should see the final prompt prefix and tool list.
func (b *Builder) InsertRequestProcessorsBeforeCache(processors ...RequestProcessor) {
	if len(processors) == 0 {
		return
	}
	idx := b.cacheBoundaryIndex()
	if idx < 0 {
		b.InsertRequestProcessorsBeforeLast(processors...)
		return
	}
	merged := make([]RequestProcessor, 0, len(b.requestProcessors)+len(processors))
	merged = append(merged, b.requestProcessors[:idx]...)
	merged = append(merged, processors...)
	merged = append(merged, b.requestProcessors[idx:]...)
	b.requestProcessors = merged
}

func (b *Builder) cacheBoundaryIndex() int {
	for i, proc := range b.requestProcessors {
		switch proc.(type) {
		case cacheAlignerProcessor, *cacheAlignerProcessor:
			return i
		}
	}
	return -1
}

// ReplaceToolRegistryProcessors swaps any tool-registry-bound request
// processors to the provided registry. If the builder has no such processor,
// it inserts a LazyTools processor before cache alignment.
func (b *Builder) ReplaceToolRegistryProcessors(reg *tool.Registry) {
	replaced := false
	for i, proc := range b.requestProcessors {
		toolProc, ok := proc.(ToolRegistryProcessor)
		if !ok {
			continue
		}
		b.requestProcessors[i] = toolProc.WithToolRegistry(reg)
		replaced = true
	}
	if replaced {
		return
	}
	b.InsertRequestProcessorsBeforeCache(NewLazyTools(reg))
}

// PrependMutators inserts commit-time mutators at the front of the mutator chain.
func (b *Builder) PrependMutators(mutators ...ContextMutator) {
	if len(mutators) == 0 {
		return
	}
	b.mutators = append(append([]ContextMutator(nil), mutators...), b.mutators...)
}

// AppendMutators adds commit-time mutators to the end of the mutator chain.
func (b *Builder) AppendMutators(mutators ...ContextMutator) {
	b.mutators = append(b.mutators, mutators...)
}
