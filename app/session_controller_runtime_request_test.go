package app

import "testing"

func TestRuntimeRequestControllerOwnsLifecycle(t *testing.T) {
	model := readyModel(t)

	requestID := model.runtimeRequest().begin("Switching runtime...")
	if requestID == 0 {
		t.Fatal("request id = 0, want non-zero")
	}
	if model.Model.RuntimeSwitchRequest != requestID {
		t.Fatalf(
			"runtime switch request = %d, want %d",
			model.Model.RuntimeSwitchRequest,
			requestID,
		)
	}
	if model.Progress.LocalStatus != "Switching runtime..." {
		t.Fatalf("local status = %q, want switching status", model.Progress.LocalStatus)
	}
	if !model.runtimeRequest().matches(requestID) {
		t.Fatal("current request did not match")
	}
	if model.runtimeRequest().matches(requestID + 1) {
		t.Fatal("stale request matched")
	}
	if model.runtimeRequest().finish(requestID + 1) {
		t.Fatal("stale request finished active lifecycle")
	}
	if model.Model.RuntimeSwitchRequest != requestID {
		t.Fatal("stale finish cleared active request")
	}
	if !model.runtimeRequest().finish(requestID) {
		t.Fatal("current request did not finish")
	}
	if model.Model.RuntimeSwitchRequest != 0 {
		t.Fatalf("runtime switch request = %d, want cleared", model.Model.RuntimeSwitchRequest)
	}
	if model.Progress.LocalStatus != "" {
		t.Fatalf("local status = %q, want cleared", model.Progress.LocalStatus)
	}
}
