package main

import (
	"bytes"
	"encoding/json"
	"strings"
	"testing"
)

func TestConfigExplainCommand(t *testing.T) {
	var stdout, stderr bytes.Buffer
	handled, code := runTopLevelCommand([]string{"config", "explain"}, &stdout, &stderr)
	if !handled || code != 0 {
		t.Fatalf("config explain failed: handled=%v, code=%d, stderr=%s", handled, code, stderr.String())
	}
	out := stdout.String()
	if !strings.Contains(out, "Ion Effective Configuration") {
		t.Fatalf("expected output header, got %q", out)
	}
	if !strings.Contains(out, "provider:") || !strings.Contains(out, "model:") {
		t.Fatalf("expected provider/model fields, got %q", out)
	}

	stdout.Reset()
	stderr.Reset()
	handled, code = runTopLevelCommand([]string{"config", "explain", "--json"}, &stdout, &stderr)
	if !handled || code != 0 {
		t.Fatalf("config explain --json failed: handled=%v, code=%d, stderr=%s", handled, code, stderr.String())
	}
	var explanation EffectiveConfigExplanation
	if err := json.Unmarshal(stdout.Bytes(), &explanation); err != nil {
		t.Fatalf("failed to decode JSON explanation: %v", err)
	}
	if explanation.WorkspaceRoot == "" {
		t.Fatal("empty workspace root in JSON explanation")
	}
	if _, ok := explanation.Fields["provider"]; !ok {
		t.Fatal("missing provider in explanation fields")
	}
	if _, ok := explanation.Fields["model"]; !ok {
		t.Fatal("missing model in explanation fields")
	}
}
