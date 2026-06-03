package typedtool

import (
	"context"
	"errors"
	"fmt"

	"github.com/go-json-experiment/json"
	"github.com/nijaru/ion/internal/approval"
	"github.com/nijaru/ion/llm"
	basetool "github.com/nijaru/ion/tool"
)

// Handler executes a typed tool operation.
type Handler[A, R any] func(context.Context, A) (R, error)

// ApprovalFunc maps typed tool arguments to an optional approval requirement.
type ApprovalFunc[A any] func(A) (approval.Requirement, bool, error)

// Config describes one typed Go-authored tool.
type Config[A, R any] struct {
	Name        string
	Description string
	Schema      any
	Metadata    basetool.Metadata
	Execute     Handler[A, R]
	Approval    ApprovalFunc[A]
}

// Tool adapts a typed Go handler to Ion's raw provider-facing tool contract.
// JSON decoding and encoding stay at this boundary.
type Tool[A, R any] struct {
	spec     llm.Spec
	metadata basetool.Metadata
	execute  Handler[A, R]
	approval ApprovalFunc[A]
}

var (
	_ basetool.Tool                = (*Tool[struct{}, struct{}])(nil)
	_ basetool.MetadataTool        = (*Tool[struct{}, struct{}])(nil)
	_ approval.RequirementProvider = (*Tool[struct{}, struct{}])(nil)
)

// New constructs a tool from a typed Go handler.
func New[A, R any](cfg Config[A, R]) (*Tool[A, R], error) {
	if cfg.Name == "" {
		return nil, errors.New("typed tool: name is required")
	}
	if cfg.Description == "" {
		return nil, errors.New("typed tool: description is required")
	}
	if cfg.Execute == nil {
		return nil, errors.New("typed tool: execute handler is required")
	}

	schema := cfg.Schema
	if schema == nil {
		var err error
		schema, err = basetool.SchemaFor[A]()
		if err != nil {
			return nil, err
		}
	}

	return &Tool[A, R]{
		spec: llm.Spec{
			Name:        cfg.Name,
			Description: cfg.Description,
			Parameters:  schema,
		},
		metadata: cfg.Metadata,
		execute:  cfg.Execute,
		approval: cfg.Approval,
	}, nil
}

// Must constructs a typed tool and panics if the configuration is invalid.
func Must[A, R any](cfg Config[A, R]) *Tool[A, R] {
	t, err := New(cfg)
	if err != nil {
		panic(err)
	}
	return t
}

// Register constructs and registers a typed tool.
func Register[A, R any](r *basetool.Registry, cfg Config[A, R]) (*Tool[A, R], error) {
	if r == nil {
		return nil, errors.New("typed tool: registry is required")
	}
	t, err := New(cfg)
	if err != nil {
		return nil, err
	}
	r.Register(t)
	return t, nil
}

func (t *Tool[A, R]) Spec() llm.Spec { return t.spec }

func (t *Tool[A, R]) Metadata() basetool.Metadata { return t.metadata }

func (t *Tool[A, R]) Execute(ctx context.Context, args string) (string, error) {
	var input A
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return "", fmt.Errorf("typed tool %s: decode args: %w", t.spec.Name, err)
	}

	result, err := t.execute(ctx, input)
	if err != nil {
		return "", fmt.Errorf("typed tool %s: %w", t.spec.Name, err)
	}
	out, err := json.Marshal(result)
	if err != nil {
		return "", fmt.Errorf("typed tool %s: encode result: %w", t.spec.Name, err)
	}
	return string(out), nil
}

func (t *Tool[A, R]) ApprovalRequirement(args string) (approval.Requirement, bool, error) {
	if t.approval == nil {
		return approval.Requirement{}, false, nil
	}
	var input A
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return approval.Requirement{}, false, fmt.Errorf(
			"typed tool %s: decode approval args: %w",
			t.spec.Name,
			err,
		)
	}
	return t.approval(input)
}
