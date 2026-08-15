package app

import (
	"testing"
)

func TestPreparePickerSearchQueryRegex(t *testing.T) {
	q := preparePickerSearchQuery("re:gpt-[45]")
	if q.regex == nil || q.regErr != nil {
		t.Fatalf("expected compiled regex, got err=%v", q.regErr)
	}

	fields := []pickerSearchField{
		{value: "openrouter/openai/gpt-5-mini", weight: 0},
		{value: "anthropic/claude-3.5-sonnet", weight: 0},
	}

	score, matched := pickerSearchScorePrepared(q, fields)
	if !matched {
		t.Fatal("expected regex to match gpt-5-mini")
	}
	if score < 0 {
		t.Fatalf("unexpected score: %d", score)
	}

	// Non-matching regex
	qNoMatch := preparePickerSearchQuery("re:claude-4")
	_, matchedNo := pickerSearchScorePrepared(qNoMatch, fields[:1])
	if matchedNo {
		t.Fatal("expected regex NOT to match gpt-5-mini")
	}
}

func TestPreparePickerSearchQueryPhrases(t *testing.T) {
	q := preparePickerSearchQuery(`foo "exact phrase" bar`)
	if len(q.tokens) != 3 {
		t.Fatalf("expected 3 tokens, got %d: %+v", len(q.tokens), q.tokens)
	}
	if !q.tokens[1].isPhrase || q.tokens[1].value != "exact phrase" {
		t.Fatalf("expected token 1 to be phrase 'exact phrase', got: %+v", q.tokens[1])
	}

	fields := []pickerSearchField{
		{value: "this is foo with exact phrase and bar inside", weight: 0},
	}

	score, matched := pickerSearchScorePrepared(q, fields)
	if !matched {
		t.Fatal("expected multi-token with phrase to match")
	}
	if score < 0 {
		t.Fatalf("unexpected score: %d", score)
	}
}
