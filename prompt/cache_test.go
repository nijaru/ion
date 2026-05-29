package prompt

import (
	"context"
	"regexp"
	"testing"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestFingerprintPromptCacheIgnoresHistorySuffix(t *testing.T) {
	sess := session.New("cache")
	if err := sess.Append(context.Background(), session.NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "hello",
	})); err != nil {
		t.Fatalf("append history: %v", err)
	}

	req1 := &llm.Request{
		Messages: []llm.Message{
			{Role: llm.RoleSystem, Content: "system"},
			{Role: llm.RoleUser, Content: "hello"},
		},
		Tools: []*llm.Spec{{Name: "alpha", Description: "A"}},
	}
	req2 := &llm.Request{
		Messages: []llm.Message{
			{Role: llm.RoleSystem, Content: "system"},
			{Role: llm.RoleUser, Content: "different history"},
		},
		Tools: []*llm.Spec{{Name: "alpha", Description: "A"}},
	}

	fp1, err := FingerprintPromptCache(sess, req1)
	if err != nil {
		t.Fatalf("fingerprint req1: %v", err)
	}
	fp2, err := FingerprintPromptCache(sess, req2)
	if err != nil {
		t.Fatalf("fingerprint req2: %v", err)
	}
	if fp1 != fp2 {
		t.Fatalf("fingerprint changed across history-only mutation: %v vs %v", fp1, fp2)
	}
}

func TestFingerprintPromptCacheChangesOnPrefixOrToolSchema(t *testing.T) {
	sess := session.New("cache-change")
	if err := sess.Append(context.Background(), session.NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "hello",
	})); err != nil {
		t.Fatalf("append history: %v", err)
	}

	base := &llm.Request{
		Messages: []llm.Message{
			{Role: llm.RoleSystem, Content: "system"},
			{Role: llm.RoleUser, Content: "hello"},
		},
		Tools: []*llm.Spec{{Name: "alpha", Description: "A"}},
	}
	changedPrefix := &llm.Request{
		Messages: []llm.Message{
			{Role: llm.RoleSystem, Content: "updated system"},
			{Role: llm.RoleUser, Content: "hello"},
		},
		Tools: []*llm.Spec{{Name: "alpha", Description: "A"}},
	}
	changedTools := &llm.Request{
		Messages: []llm.Message{
			{Role: llm.RoleSystem, Content: "system"},
			{Role: llm.RoleUser, Content: "hello"},
		},
		Tools: []*llm.Spec{{Name: "alpha", Description: "B"}},
	}

	baseFP, err := FingerprintPromptCache(sess, base)
	if err != nil {
		t.Fatalf("fingerprint base: %v", err)
	}
	prefixFP, err := FingerprintPromptCache(sess, changedPrefix)
	if err != nil {
		t.Fatalf("fingerprint changed prefix: %v", err)
	}
	toolsFP, err := FingerprintPromptCache(sess, changedTools)
	if err != nil {
		t.Fatalf("fingerprint changed tools: %v", err)
	}
	if baseFP == prefixFP {
		t.Fatal("expected prefix hash to change when system prompt changes")
	}
	if baseFP == toolsFP {
		t.Fatal("expected tool schema hash to change when tool schema changes")
	}
}

func TestFingerprintPromptCacheUsesExplicitPrefixBoundary(t *testing.T) {
	req1 := &llm.Request{
		Messages: []llm.Message{
			{Role: llm.RoleSystem, Content: "system"},
			{Role: llm.RoleUser, Content: "stable context"},
			{Role: llm.RoleUser, Content: "history one"},
		},
		CachePrefixMessages: 2,
	}
	req2 := &llm.Request{
		Messages: []llm.Message{
			{Role: llm.RoleSystem, Content: "system"},
			{Role: llm.RoleUser, Content: "stable context"},
			{Role: llm.RoleUser, Content: "history two"},
		},
		CachePrefixMessages: 2,
	}
	changedPrefix := &llm.Request{
		Messages: []llm.Message{
			{Role: llm.RoleSystem, Content: "system"},
			{Role: llm.RoleUser, Content: "changed stable context"},
			{Role: llm.RoleUser, Content: "history one"},
		},
		CachePrefixMessages: 2,
	}

	fp1, err := FingerprintPromptCache(nil, req1)
	if err != nil {
		t.Fatalf("fingerprint req1: %v", err)
	}
	fp2, err := FingerprintPromptCache(nil, req2)
	if err != nil {
		t.Fatalf("fingerprint req2: %v", err)
	}
	changedFP, err := FingerprintPromptCache(nil, changedPrefix)
	if err != nil {
		t.Fatalf("fingerprint changed prefix: %v", err)
	}
	if fp1 != fp2 {
		t.Fatalf("expected history suffix to be excluded, got %v vs %v", fp1, fp2)
	}
	if fp1 == changedFP {
		t.Fatal("expected prefix context mutation to change fingerprint")
	}
}

func TestInjectContextBlockRespectsCachePrefixBoundary(t *testing.T) {
	req := &llm.Request{
		Messages: []llm.Message{
			{Role: llm.RoleSystem, Content: "system"},
			{Role: llm.RoleUser, Content: "stable context"},
			{Role: llm.RoleUser, Content: "history"},
		},
		CachePrefixMessages: 2,
	}

	blockRegex := regexp.MustCompile(`(?s)<memory_context>.*?</memory_context>\n*`)
	InjectContextBlock(req, blockRegex, "<memory_context>\ncurrent\n</memory_context>")

	if req.CachePrefixMessages != 2 {
		t.Fatalf("expected cache prefix boundary to remain stable, got %d", req.CachePrefixMessages)
	}
	if req.Messages[1].Content != "stable context" {
		t.Fatalf("expected stable context to remain in prefix, got %#v", req.Messages)
	}
	if req.Messages[2].Content != "<memory_context>\ncurrent\n</memory_context>" {
		t.Fatalf("expected dynamic context after prefix, got %#v", req.Messages)
	}
}

func TestCacheAligner(t *testing.T) {
	req := &llm.Request{
		Messages: []llm.Message{
			{Role: llm.RoleSystem, Content: "1"},
			{Role: llm.RoleUser, Content: "2"},
			{Role: llm.RoleAssistant, Content: "3"},
			{Role: llm.RoleUser, Content: "4"},
		},
		Tools: []*llm.Spec{
			{Name: "tool1"},
			{Name: "tool2"},
			{Name: "tool3"},
		},
	}

	aligner := CacheAligner(2)
	err := aligner.ApplyRequest(context.Background(), nil, "", nil, req)
	if err != nil {
		t.Fatalf("CacheAligner error: %v", err)
	}

	for i, m := range req.Messages {
		if i == 0 || i == 2 || i == 3 {
			if m.CacheControl == nil || m.CacheControl.Type != "ephemeral" {
				t.Errorf("expected message %d to have ephemeral cache control", i)
			}
		} else {
			if m.CacheControl != nil {
				t.Errorf("expected message %d to NOT have cache control, got %v", i, m.CacheControl)
			}
		}
	}

	for i, tool := range req.Tools {
		if i == 2 {
			if tool.CacheControl == nil || tool.CacheControl.Type != "ephemeral" {
				t.Errorf("expected last tool to have ephemeral cache control")
			}
		} else {
			if tool.CacheControl != nil {
				t.Errorf("expected tool %d to NOT have cache control", i)
			}
		}
	}
}

func TestCacheAlignerMarksExplicitPrefixBoundary(t *testing.T) {
	req := &llm.Request{
		Messages: []llm.Message{
			{Role: llm.RoleSystem, Content: "system"},
			{Role: llm.RoleUser, Content: "stable context"},
			{Role: llm.RoleUser, Content: "history"},
		},
		CachePrefixMessages: 2,
	}

	if err := CacheAligner(1).ApplyRequest(context.Background(), nil, "", nil, req); err != nil {
		t.Fatalf("CacheAligner: %v", err)
	}

	if req.Messages[0].CacheControl != nil {
		t.Fatalf("expected only prefix boundary marked, got %#v", req.Messages[0].CacheControl)
	}
	if req.Messages[1].CacheControl == nil ||
		req.Messages[1].CacheControl.Type != "ephemeral" {
		t.Fatalf("expected stable context boundary marker, got %#v", req.Messages[1])
	}
	if req.Messages[2].CacheControl == nil ||
		req.Messages[2].CacheControl.Type != "ephemeral" {
		t.Fatalf("expected recent history marker, got %#v", req.Messages[2])
	}
}
