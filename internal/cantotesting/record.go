package testing

import (
	"context"
	"os"
	"sync"

	"github.com/go-json-experiment/json"
	"github.com/go-json-experiment/json/jsontext"
	"github.com/nijaru/ion/llm"
)

// RecordedStep captures a single execution step (request and its result).
type RecordedStep struct {
	Request  *llm.Request  `json:"request"`
	Response *llm.Response `json:"response,omitempty"`
	Chunks   []llm.Chunk   `json:"chunks,omitempty"`
}

// RecordProvider wraps an llm.Provider and records all interactions to a list
// of RecordedStep. Use Save() to write the recorded steps to disk.
type RecordProvider struct {
	llm.Provider
	path  string
	mu    sync.Mutex
	steps []RecordedStep
}

// NewRecordProvider creates a RecordProvider that wraps p. It will save the
// recorded steps to path when Save() is called.
func NewRecordProvider(p llm.Provider, path string) *RecordProvider {
	return &RecordProvider{
		Provider: p,
		path:     path,
	}
}

// Generate executes the request through the underlying provider and records the response.
func (r *RecordProvider) Generate(
	ctx context.Context,
	req *llm.Request,
) (*llm.Response, error) {
	resp, err := r.Provider.Generate(ctx, req)
	if err != nil {
		return nil, err
	}

	r.mu.Lock()
	r.steps = append(r.steps, RecordedStep{
		Request:  req,
		Response: resp,
	})
	r.mu.Unlock()

	return resp, nil
}

// Stream executes the request through the underlying provider and returns a
// stream that records all chunks as they are consumed.
func (r *RecordProvider) Stream(ctx context.Context, req *llm.Request) (llm.Stream, error) {
	stream, err := r.Provider.Stream(ctx, req)
	if err != nil {
		return nil, err
	}

	return &recordingStream{
		Stream: stream,
		req:    req,
		parent: r,
	}, nil
}

type recordingStream struct {
	llm.Stream
	req    *llm.Request
	chunks []llm.Chunk
	parent *RecordProvider
}

func (s *recordingStream) Next() (*llm.Chunk, bool) {
	chunk, ok := s.Stream.Next()
	if ok && chunk != nil {
		s.chunks = append(s.chunks, *chunk)
	}
	return chunk, ok
}

func (s *recordingStream) Close() error {
	err := s.Stream.Close()
	if err != nil || s.Stream.Err() != nil {
		return err
	}

	s.parent.mu.Lock()
	s.parent.steps = append(s.parent.steps, RecordedStep{
		Request: s.req,
		Chunks:  s.chunks,
	})
	s.parent.mu.Unlock()
	return err
}

// Save writes the recorded steps to the path provided at construction.
func (r *RecordProvider) Save() error {
	r.mu.Lock()
	steps := make([]RecordedStep, len(r.steps))
	copy(steps, r.steps)
	r.mu.Unlock()

	f, err := os.Create(r.path)
	if err != nil {
		return err
	}
	defer f.Close()

	return json.MarshalWrite(f, steps, jsontext.WithIndent("\t"))
}

// LoadRecordedSteps loads a sequence of RecordedStep from a JSON file.
func LoadRecordedSteps(path string) ([]RecordedStep, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	var steps []RecordedStep
	if err := json.UnmarshalRead(f, &steps); err != nil {
		return nil, err
	}
	return steps, nil
}

// ToSteps converts a slice of RecordedStep to x/testing/Step, suitable for
// programmed responses in a FauxProvider.
func ToSteps(recorded []RecordedStep) []Step {
	steps := make([]Step, len(recorded))
	for i, r := range recorded {
		if r.Response != nil {
			steps[i] = Step{
				Content:        r.Response.Content,
				Reasoning:      r.Response.Reasoning,
				ThinkingBlocks: r.Response.ThinkingBlocks,
				Calls:          r.Response.Calls,
			}
		} else {
			steps[i] = Step{
				Chunks: r.Chunks,
			}
		}
	}
	return steps
}
