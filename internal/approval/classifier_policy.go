package approval

import (
	"context"
	"fmt"
	"strings"

	"github.com/nijaru/ion/internal/llm"
	"github.com/nijaru/ion/internal/storage/session"
)

// ClassifierPolicy uses an LLM classifier to make automated approval decisions.
type ClassifierPolicy struct {
	classifier llm.Classifier
	labels     []string
	builder    func(*session.Session, Request) (string, error)
}

// NewClassifierPolicy creates a new policy backed by an LLM classifier.
func NewClassifierPolicy(classifier llm.Classifier, labels []string) *ClassifierPolicy {
	return &ClassifierPolicy{
		classifier: classifier,
		labels:     labels,
		builder:    DefaultClassifierPrompt,
	}
}

// WithPromptBuilder configures a custom prompt builder for the classifier.
func (p *ClassifierPolicy) WithPromptBuilder(
	builder func(*session.Session, Request) (string, error),
) *ClassifierPolicy {
	if p == nil {
		return nil
	}
	p.builder = builder
	return p
}

// Decide implements Policy.
func (p *ClassifierPolicy) Decide(
	ctx context.Context,
	sess *session.Session,
	req Request,
) (Result, bool, error) {
	if p.classifier == nil {
		return Result{}, false, nil
	}

	input, err := p.builder(sess, req)
	if err != nil {
		return Result{}, false, err
	}

	res, err := p.classifier.Classify(ctx, input, p.labels)
	if err != nil {
		return Result{}, false, err
	}

	decision := DecisionDeny
	if strings.EqualFold(res.Label, string(DecisionAllow)) {
		decision = DecisionAllow
	}

	return Result{
		Decision: decision,
		Reason:   res.Reason,
	}, true, nil
}

// DefaultClassifierPrompt builds a basic prompt for tool call classification.
func DefaultClassifierPrompt(sess *session.Session, req Request) (string, error) {
	// Simple implementation for now.
	// In production, this should include N last messages for context.
	return fmt.Sprintf(
		"Proposed Action:\nTool: %s\nArguments: %s\nCategory: %s\nOperation: %s\nResource: %s",
		req.Tool,
		req.Args,
		req.Category,
		req.Operation,
		req.Resource,
	), nil
}
