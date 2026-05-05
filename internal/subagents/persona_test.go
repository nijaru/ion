package subagents

import (
	"os"
	"path/filepath"
	"testing"
)

func TestParseMarkdownPersona(t *testing.T) {
	persona, err := ParseMarkdown(`---
name: scout
description: Quick repository scouting.
model: fast
tools: [read, grep, read]
---
Find the relevant files and summarize them.
`)
	if err != nil {
		t.Fatalf("ParseMarkdown returned error: %v", err)
	}
	if persona.Name != "scout" {
		t.Fatalf("name = %q, want scout", persona.Name)
	}
	if persona.ModelSlot != ModelSlotFast {
		t.Fatalf("model slot = %q, want fast", persona.ModelSlot)
	}
	if len(persona.Tools) != 2 || persona.Tools[0] != "grep" || persona.Tools[1] != "read" {
		t.Fatalf("tools = %#v, want deduped sorted tools", persona.Tools)
	}
	if persona.Prompt != "Find the relevant files and summarize them." {
		t.Fatalf("prompt = %q", persona.Prompt)
	}
}

func TestParseMarkdownPersonaRequiresFrontmatter(t *testing.T) {
	if _, err := ParseMarkdown("plain markdown"); err == nil {
		t.Fatal("ParseMarkdown returned nil error")
	}
}

func TestParseMarkdownPersonaAcceptsCRLFFrontmatter(t *testing.T) {
	persona, err := ParseMarkdown("---\r\nname: scout\r\ndescription: Quick scouting.\r\nmodel: fast\r\ntools: [read]\r\n---\r\nFind files.\r\n")
	if err != nil {
		t.Fatalf("ParseMarkdown returned error: %v", err)
	}
	if persona.Name != "scout" || persona.Prompt != "Find files." {
		t.Fatalf("persona = %#v, want parsed CRLF markdown", persona)
	}
}

func TestLoadDirSortsMarkdownPersonas(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "b.md"), []byte(`---
name: b
description: B persona.
tools: [read]
---
B prompt.
`), 0o600); err != nil {
		t.Fatalf("write b: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dir, "a.md"), []byte(`---
name: a
description: A persona.
model: fast
tools: [grep]
---
A prompt.
`), 0o600); err != nil {
		t.Fatalf("write a: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dir, "skip.txt"), []byte("ignored"), 0o600); err != nil {
		t.Fatalf("write skip: %v", err)
	}

	personas, err := LoadDir(dir)
	if err != nil {
		t.Fatalf("LoadDir returned error: %v", err)
	}
	if len(personas) != 2 || personas[0].Name != "a" || personas[1].Name != "b" {
		t.Fatalf("personas = %#v, want sorted a,b", personas)
	}
}

func TestMergeOverridesBuiltins(t *testing.T) {
	merged := Merge([]Persona{{
		Name:        "explorer",
		Description: "builtin",
		ModelSlot:   ModelSlotFast,
		Tools:       []string{"read"},
		Prompt:      "builtin prompt",
	}}, []Persona{{
		Name:        "explorer",
		Description: "custom",
		ModelSlot:   ModelSlotPrimary,
		Tools:       []string{"bash"},
		Prompt:      "custom prompt",
	}})

	persona, ok := Find(merged, "explorer")
	if !ok {
		t.Fatal("merged persona not found")
	}
	if persona.Description != "custom" || persona.ModelSlot != ModelSlotPrimary {
		t.Fatalf("merged persona = %#v, want custom override", persona)
	}
}
