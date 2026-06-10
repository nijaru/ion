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
	resp Response
}

func (a *StreamAccumulator) Add(chunk *Chunk) {
	if chunk == nil {
		return
	}
	if chunk.Block != nil {
		a.addBlock(chunk.Block)
	} else {
		// Write to Blocks instead of flat fields.
		if chunk.Content != "" {
			a.addBlock(TextBlock{Text: chunk.Content})
		}
		if chunk.Reasoning != "" {
			a.addBlock(ThinkingBlock{Thinking: chunk.Reasoning})
		}
		for _, block := range chunk.ThinkingBlocks {
			a.addBlock(block)
		}
		for _, call := range chunk.Calls {
			a.addBlock(ToolCallBlock{
				ID:        call.ID,
				Name:      call.Function.Name,
				Arguments: call.Function.Arguments,
			})
		}
	}
	if chunk.Usage != nil {
		a.resp.Usage = *chunk.Usage
	}
}

func (a *StreamAccumulator) Response() Response {
	resp := a.resp
	// Populate flat fields from Blocks for backward compatibility.
	// This ensures consumers that haven't migrated to accessors still work.
	if len(resp.Blocks) > 0 {
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



