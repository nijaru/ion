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
	// Block, when set, is the structured content for this chunk.
	// Providers may set Block instead of the flat fields.
	Block ContentBlock `json:"block,omitempty"`
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
	if chunk.Block != nil {
		a.addBlock(chunk.Block)
	} else {
		a.resp.Content += chunk.Content
		a.resp.Reasoning += chunk.Reasoning
		for _, block := range chunk.ThinkingBlocks {
			a.addThinkingBlock(block)
		}
		for _, call := range chunk.Calls {
			a.addCall(call)
		}
	}
	if chunk.Usage != nil {
		a.resp.Usage = *chunk.Usage
	}
}

func (a *StreamAccumulator) Response() Response {
	resp := a.resp
	if len(resp.Blocks) > 0 {
		resp.Content = ""
		resp.Reasoning = ""
		resp.ThinkingBlocks = nil
		resp.Calls = nil
		for _, b := range resp.Blocks {
			switch v := b.(type) {
			case TextBlock:
				resp.Content += v.Text
			case ThinkingBlock:
				resp.Reasoning += v.Thinking
				resp.ThinkingBlocks = append(resp.ThinkingBlocks, v)
			case ToolCallBlock:
				resp.Calls = append(resp.Calls, Call{
					ID:   v.ID,
					Type: "function",
					Function: struct {
						Name      string `json:"name"`
						Arguments string `json:"arguments"`
					}{Name: v.Name, Arguments: v.Arguments},
				})
			}
		}
	}
	return resp
}

// addBlock accumulates a typed content block into resp.Blocks.
func (a *StreamAccumulator) addBlock(block ContentBlock) {
	switch b := block.(type) {
	case TextBlock:
		if lastIdx := len(a.resp.Blocks) - 1; lastIdx >= 0 {
			if last, ok := a.resp.Blocks[lastIdx].(TextBlock); ok {
				a.resp.Blocks[lastIdx] = TextBlock{Text: last.Text + b.Text}
				return
			}
		}
		a.resp.Blocks = append(a.resp.Blocks, b)
	case ThinkingBlock:
		if lastIdx := len(a.resp.Blocks) - 1; lastIdx >= 0 {
			if last, ok := a.resp.Blocks[lastIdx].(ThinkingBlock); ok {
				if b.Signature == "" && b.Redacted == last.Redacted {
					a.resp.Blocks[lastIdx] = ThinkingBlock{
						Thinking:  last.Thinking + b.Thinking,
						Signature: last.Signature,
						Redacted:  last.Redacted,
					}
					return
				}
			}
		}
		a.resp.Blocks = append(a.resp.Blocks, b)
	case ToolCallBlock:
		for i, existing := range a.resp.Blocks {
			if cb, ok := existing.(ToolCallBlock); ok && cb.ID == b.ID {
				a.resp.Blocks[i] = b
				return
			}
		}
		a.resp.Blocks = append(a.resp.Blocks, b)
	}
}

func (a *StreamAccumulator) addThinkingBlock(block ThinkingBlock) {
	if len(a.resp.ThinkingBlocks) == 0 || block.Signature != "" ||
		block.Redacted != a.resp.ThinkingBlocks[len(a.resp.ThinkingBlocks)-1].Redacted {
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
