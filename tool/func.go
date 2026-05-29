package tool

import (
	"context"
	"fmt"

	"github.com/nijaru/ion/llm"
)

// funcTool adapts a plain function to the Tool interface.
type funcTool struct {
	spec     llm.Spec
	metadata Metadata
	fn       func(ctx context.Context, args string) (string, error)
}

func (f *funcTool) Spec() llm.Spec { return f.spec }

func (f *funcTool) Metadata() Metadata { return f.metadata }

func (f *funcTool) Execute(ctx context.Context, args string) (string, error) {
	res, err := f.fn(ctx, args)
	if err != nil {
		return "", fmt.Errorf("tool %s: %w", f.spec.Name, err)
	}
	return res, nil
}

// Func constructs a Tool from a function, eliminating struct boilerplate
// for stateless single-function tools.
func Func(
	name, desc string,
	schema any,
	fn func(ctx context.Context, args string) (string, error),
) Tool {
	return FuncWithMetadata(name, desc, schema, Metadata{}, fn)
}

// FuncWithMetadata constructs a Tool from a function with metadata.
func FuncWithMetadata(
	name, desc string,
	schema any,
	metadata Metadata,
	fn func(ctx context.Context, args string) (string, error),
) Tool {
	return &funcTool{
		spec:     llm.Spec{Name: name, Description: desc, Parameters: schema},
		metadata: metadata,
		fn:       fn,
	}
}
