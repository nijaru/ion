package prompt

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"fmt"

	"github.com/go-json-experiment/json"

	"github.com/nijaru/ion/internal/llm"
	"github.com/nijaru/ion/internal/storage/session"
)

// PromptCacheFingerprint captures the parts of a request that should stay
// stable for prefix-cache reuse.
type PromptCacheFingerprint struct {
	PrefixHash     string `json:"prefix_hash,omitzero"`
	ToolSchemaHash string `json:"tool_schema_hash,omitzero"`
}

// FingerprintPromptCache hashes the static request prefix and current tool
// schema list.
//
// The prefix hash intentionally excludes the session history suffix so the
// result stays stable across ordinary turn-to-turn conversation growth. The
// tool hash is taken from the built request so lazy tool unlocking is visible.
func FingerprintPromptCache(
	sess *session.Session,
	req *llm.Request,
) (PromptCacheFingerprint, error) {
	if req == nil {
		return PromptCacheFingerprint{}, nil
	}

	prefix := req.Messages
	if req.CachePrefixMessages > 0 {
		if req.CachePrefixMessages < len(req.Messages) {
			prefix = req.Messages[:req.CachePrefixMessages]
		}
	} else if sess != nil {
		history, err := sess.EffectiveMessages()
		if err != nil {
			return PromptCacheFingerprint{}, err
		}
		if n := len(req.Messages) - len(history); n > 0 && n <= len(req.Messages) {
			prefix = req.Messages[:n]
		} else {
			prefix = req.Messages[:0]
		}
	}

	return PromptCacheFingerprint{
		PrefixHash:     hashValue(prefix),
		ToolSchemaHash: hashValue(req.Tools),
	}, nil
}

func hashValue(v any) string {
	data, err := json.Marshal(v)
	if err != nil {
		return ""
	}

	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}

func (f PromptCacheFingerprint) String() string {
	switch {
	case f.PrefixHash == "" && f.ToolSchemaHash == "":
		return ""
	case f.ToolSchemaHash == "":
		return f.PrefixHash
	default:
		return fmt.Sprintf("%s/%s", f.PrefixHash, f.ToolSchemaHash)
	}
}

// CacheAligner returns a RequestProcessor that adds provider-agnostic
// cache-control markers to the request to maximize prefix-cache hit rates.
//
// It places "ephemeral" markers at predictable stable boundaries:
//  1. The explicit stable prefix boundary when req.CachePrefixMessages is set,
//     otherwise the last leading system message.
//  2. The last tool (caches the entire tools array).
//  3. The final messages in the request, up to historyLimit (caches the recent turn history).
func CacheAligner(historyLimit int) RequestProcessor {
	return cacheAlignerProcessor{historyLimit: historyLimit}
}

type cacheAlignerProcessor struct {
	historyLimit int
}

func (c cacheAlignerProcessor) ApplyRequest(
	ctx context.Context,
	p llm.Provider,
	model string,
	sess *session.Session,
	req *llm.Request,
) error {
	if req == nil || len(req.Messages) == 0 {
		return nil
	}

	prefixEnd := req.CachePrefixMessages
	if prefixEnd <= 0 || prefixEnd > len(req.Messages) {
		prefixEnd = 0
		for i, m := range req.Messages {
			if m.Role == llm.RoleSystem {
				prefixEnd = i + 1
			} else if prefixEnd != 0 {
				break
			}
		}
	}
	if prefixEnd > 0 {
		req.Messages[prefixEnd-1].CacheControl = &llm.CacheControl{Type: "ephemeral"}
	}

	if len(req.Tools) > 0 {
		req.Tools[len(req.Tools)-1].CacheControl = &llm.CacheControl{Type: "ephemeral"}
	}

	count := 0
	for i := len(req.Messages) - 1; i >= prefixEnd && count < c.historyLimit; i-- {
		if req.Messages[i].CacheControl == nil {
			req.Messages[i].CacheControl = &llm.CacheControl{Type: "ephemeral"}
			count++
		}
	}

	return nil
}
