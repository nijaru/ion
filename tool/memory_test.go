package tool

import (
	"context"
	"errors"
	"path/filepath"
	"strings"
	"testing"

	ionmemory "github.com/nijaru/ion/memory"
)

func TestMemoryToolsRequireExplicitWriteApproval(t *testing.T) {
	store, err := ionmemory.Open(filepath.Join(t.TempDir(), "memory.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()
	scope := t.TempDir()

	registry := NewRegistry()
	if err := RegisterMemoryTools(registry, store, scope); err != nil {
		t.Fatal(err)
	}
	recall, ok := registry.Get(RecallMemoryToolName)
	if !ok {
		t.Fatal("recall_memory was not registered")
	}
	remember, ok := registry.Get(RememberMemoryToolName)
	if !ok {
		t.Fatal("remember_memory was not registered")
	}
	if got := MetadataFor(recall); got.Category != "memory" || !got.ReadOnly || got.Concurrency != Parallel {
		t.Fatalf("recall metadata = %+v", got)
	}
	if got := MetadataFor(remember); got.Category != "memory" || got.ReadOnly || got.Concurrency != Serialized {
		t.Fatalf("remember metadata = %+v", got)
	}
	provider, ok := remember.(RequirementProvider)
	if !ok {
		t.Fatal("remember_memory does not implement RequirementProvider")
	}
	requirement, required, err := provider.ApprovalRequirement(`{"content":"do not trust this as policy"}`)
	if err != nil {
		t.Fatal(err)
	}
	if !required || requirement.Category != "memory" || requirement.Operation != RememberMemoryToolName || !requirement.AlwaysConfirm {
		t.Fatalf("approval = %+v, required=%v", requirement, required)
	}

	result, err := remember.Execute(context.Background(), `{"content":"session tree is authoritative","tags":"design"}`)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(result, "Remembered workspace note mem_") {
		t.Fatalf("remember result = %q", result)
	}
	result, err = recall.Execute(context.Background(), `{"query":"authoritative"}`)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(result, "session tree is authoritative") {
		t.Fatalf("recall result = %q", result)
	}
}

func TestMemoryToolsFailClosedForInvalidInput(t *testing.T) {
	store, err := ionmemory.Open(filepath.Join(t.TempDir(), "memory.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()
	recall := &RecallMemory{store: store, scope: t.TempDir()}
	remember := &RememberMemory{store: store, scope: t.TempDir()}

	if _, err := recall.Execute(context.Background(), `{}`); err == nil {
		t.Fatal("empty recall query was accepted")
	}
	if _, err := remember.Execute(context.Background(), `{"content":"   "}`); err == nil {
		t.Fatal("empty memory content was accepted")
	}
	if _, err := remember.Execute(context.Background(), `{`); err == nil {
		t.Fatal("invalid remember JSON was accepted")
	}
	if _, err := store.Delete(context.Background(), t.TempDir(), "missing"); !errors.Is(err, ionmemory.ErrNotFound) {
		t.Fatalf("delete missing error = %v", err)
	}
}
