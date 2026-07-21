package tool

import "testing"

func TestNativeMutatingToolsDeclareApprovalRequirements(t *testing.T) {
	bash := NewBash(t.TempDir())
	req, ok, err := bash.ApprovalRequirement(`{"command":"go test ./..."}`)
	if err != nil || !ok || req.Category != "execute" || req.Resource != "go test ./..." {
		t.Fatalf("bash approval = %#v, %v, %v", req, ok, err)
	}
	capabilities, ok := req.Metadata["sandbox_policy"].(map[string]any)
	if !ok {
		t.Fatalf("bash approval sandbox policy = %#v, want capability map", req.Metadata["sandbox_policy"])
	}
	if got := capabilities["network"]; got != "denied" {
		t.Fatalf("bash sandbox network = %#v, want denied", got)
	}
	if readPaths, ok := capabilities["read_paths"].([]string); !ok || len(readPaths) == 0 || readPaths[0] == "*" {
		t.Fatalf("bash sandbox read paths = %#v, want bounded paths", capabilities["read_paths"])
	}
	if _, ok, err := bash.ApprovalRequirement(`{"action":"list"}`); err != nil || ok {
		t.Fatalf("bash list approval = %v, %v, want no requirement", err, ok)
	}

	file := NewFileTool(t.TempDir())
	write := &Write{FileTool: *file}
	req, ok, err = write.ApprovalRequirement(`{"path":"config.toml","content":"x"}`)
	if err != nil || !ok || req.Category != "write" || req.Resource != "config.toml" {
		t.Fatalf("write approval = %#v, %v, %v", req, ok, err)
	}

	edit := &Edit{FileTool: *file}
	req, ok, err = edit.ApprovalRequirement(`{"path":"main.go","edits":[{"old_string":"a","new_string":"b"}]}`)
	if err != nil || !ok || req.Operation != "edit" || req.Resource != "main.go" {
		t.Fatalf("edit approval = %#v, %v, %v", req, ok, err)
	}
}
