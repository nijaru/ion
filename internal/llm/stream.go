package llm

// GenerateFromStream collects chunks from a stream and assembles a Response.
// It is intended for use by Provider implementations to avoid duplicating
// the complex logic of assembling streaming chunks.
func GenerateFromStream(s Stream) (*Response, error) {
	defer s.Close()
	var acc StreamAccumulator

	for {
		chunk, ok := s.Next()
		if !ok {
			break
		}
		acc.Add(chunk)
	}
	if err := s.Err(); err != nil {
		return nil, err
	}
	resp := acc.Response()
	return &resp, nil
}

// Stream defines the interface for a streaming LLM response.
type Stream interface {
	// Next returns the next chunk of the response.
	// It returns (nil, false) when the stream is exhausted.
	Next() (*Chunk, bool)
	// Err returns the first error encountered during streaming.
	Err() error
	// Close closes the stream.
	Close() error
}

// Chunk represents a single piece of a streaming response.
type Chunk struct {
	Content        string          `json:"content"`
	Reasoning      string          `json:"reasoning,omitempty"`
	ThinkingBlocks []ThinkingBlock `json:"thinking_blocks,omitempty"`
	Calls          []Call          `json:"tool_calls,omitempty"`
	// Usage is cumulative when present. Providers may emit multiple usage chunks;
	// consumers should keep the latest value rather than summing chunks.
	Usage *Usage `json:"usage,omitempty"`
}

// StreamAccumulator assembles normalized stream chunks into a provider response.
//
// Provider adapters are responsible for turning provider-specific deltas into
// cumulative chunks. Tool calls with the same ID replace the previous call state,
// and Usage keeps the latest cumulative value.
type StreamAccumulator struct {
	resp            Response
	toolCallIndices map[string]int
}

func (a *StreamAccumulator) Add(chunk *Chunk) {
	if chunk == nil {
		return
	}
	a.resp.Content += chunk.Content
	a.resp.Reasoning += chunk.Reasoning
	for _, block := range chunk.ThinkingBlocks {
		a.addThinkingBlock(block)
	}
	for _, call := range chunk.Calls {
		a.addCall(call)
	}
	if chunk.Usage != nil {
		a.resp.Usage = *chunk.Usage
	}
}

func (a *StreamAccumulator) Response() Response {
	return a.resp
}

func (a *StreamAccumulator) addThinkingBlock(block ThinkingBlock) {
	if len(a.resp.ThinkingBlocks) == 0 || block.Signature != "" ||
		block.Type != a.resp.ThinkingBlocks[len(a.resp.ThinkingBlocks)-1].Type {
		a.resp.ThinkingBlocks = append(a.resp.ThinkingBlocks, block)
		return
	}

	last := &a.resp.ThinkingBlocks[len(a.resp.ThinkingBlocks)-1]
	last.Thinking += block.Thinking
	if block.Signature != "" {
		last.Signature = block.Signature
	}
}

func (a *StreamAccumulator) addCall(call Call) {
	if call.ID == "" {
		return
	}
	if a.toolCallIndices == nil {
		a.toolCallIndices = make(map[string]int)
	}
	if idx, ok := a.toolCallIndices[call.ID]; ok {
		a.resp.Calls[idx] = call
		return
	}
	a.toolCallIndices[call.ID] = len(a.resp.Calls)
	a.resp.Calls = append(a.resp.Calls, call)
}
