package session

import "testing"

func TestRuntimeRequestLifecycle(t *testing.T) {
	begin := BeginRuntimeRequest(RuntimeRequestBeginInput{
		Current: 41,
		Status:  "Switching runtime...",
	})
	if begin.RequestID != 42 ||
		begin.Status != "Switching runtime..." ||
		!begin.SetLocalStatus {
		t.Fatalf("BeginRuntimeRequest() = %#v", begin)
	}

	if !RuntimeRequestMatches(begin.RequestID, begin.RequestID) {
		t.Fatal("current request did not match")
	}
	if !RuntimeRequestMatches(begin.RequestID, 0) {
		t.Fatal("zero request should match as legacy/no-id request")
	}
	if RuntimeRequestMatches(begin.RequestID, begin.RequestID+1) {
		t.Fatal("stale request matched")
	}

	staleFinish := FinishRuntimeRequest(begin.RequestID, begin.RequestID+1)
	if staleFinish.Matched ||
		staleFinish.Active != begin.RequestID ||
		staleFinish.ClearLocalStatus {
		t.Fatalf("stale FinishRuntimeRequest() = %#v", staleFinish)
	}

	finish := FinishRuntimeRequest(begin.RequestID, begin.RequestID)
	if !finish.Matched || finish.Active != 0 || !finish.ClearLocalStatus {
		t.Fatalf("FinishRuntimeRequest() = %#v", finish)
	}

	clear := ClearRuntimeRequest()
	if clear.Active != 0 || !clear.ClearLocalStatus {
		t.Fatalf("ClearRuntimeRequest() = %#v", clear)
	}
}
