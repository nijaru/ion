package app

import "testing"

func TestProgressReducerLocalStatusLifecycle(t *testing.T) {
	model := readyModel(t)

	model.progressReducer().beginLocalStatus("Saving settings...")
	if model.Progress.Status != "Saving settings..." {
		t.Fatalf("status = %q, want Saving settings...", model.Progress.Status)
	}
	if model.Progress.StatusUpdatedAt.IsZero() {
		t.Fatal("status timestamp was not recorded")
	}

	model.progressReducer().clearLocalBusyStatus()
	if model.Progress.Status != "" {
		t.Fatalf("status = %q, want cleared", model.Progress.Status)
	}
	if !model.Progress.StatusUpdatedAt.IsZero() {
		t.Fatalf("status timestamp = %v, want zero", model.Progress.StatusUpdatedAt)
	}
}

func TestProgressReducerCompleteCompactionClearsBusyError(t *testing.T) {
	model := readyModel(t)
	model.Progress.Compacting = true
	model.Progress.Mode = stateError
	model.Progress.LastError = "old error"
	model.Progress.ContextTokens = 123
	model.Progress.Status = "Compacting context..."

	model.progressReducer().completeCompaction()
	if model.Progress.Compacting {
		t.Fatal("compacting = true, want false")
	}
	if model.Progress.ContextTokens != 0 {
		t.Fatalf("context tokens = %d, want zero", model.Progress.ContextTokens)
	}
	if model.Progress.Mode != stateReady || model.Progress.LastError != "" {
		t.Fatalf("progress error state = %#v, want ready without error", model.Progress)
	}
	if model.Progress.Status != "Ready" {
		t.Fatalf("status = %q, want Ready", model.Progress.Status)
	}
}
