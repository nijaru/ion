package llm

import (
	"encoding/json"
	"net/http"
	"testing"
)

func TestRequestCloneCopiesNestedJSONValues(t *testing.T) {
	raw := json.RawMessage(`{"nested":true}`)
	parameters := map[string]any{
		"labels": map[string]string{"kind": "source"},
		"raw":    raw,
	}
	request := &Request{Tools: []*Spec{{Parameters: parameters}}}
	clone := request.Clone()

	parameters["labels"].(map[string]string)["kind"] = "mutated"
	raw[0] = '['
	cloned := clone.Tools[0].Parameters.(map[string]any)
	if cloned["labels"].(map[string]string)["kind"] != "source" {
		t.Fatalf("clone labels = %#v, want original value", cloned["labels"])
	}
	if string(cloned["raw"].(json.RawMessage)) != `{"nested":true}` {
		t.Fatalf("clone raw = %s, want original value", cloned["raw"])
	}
}

func TestRequestClonePreservesEmptyJSONCollections(t *testing.T) {
	request := &Request{
		Messages: []Message{},
		Tools: []*Spec{{Parameters: map[string]any{
			"properties": map[string]any{},
			"required":   []string{},
		}}},
		ResponseFormat: &ResponseFormat{Schema: map[string]any{}},
	}
	clone := request.Clone()
	if clone.Messages == nil {
		t.Fatal("empty messages became nil")
	}
	parameters := clone.Tools[0].Parameters.(map[string]any)
	if parameters["properties"] == nil || parameters["properties"].(map[string]any) == nil {
		t.Fatalf("empty properties = %#v, want non-nil empty map", parameters["properties"])
	}
	if parameters["required"] == nil || parameters["required"].([]string) == nil {
		t.Fatalf("empty required = %#v, want non-nil empty slice", parameters["required"])
	}
	if clone.ResponseFormat.Schema == nil {
		t.Fatal("empty response schema became nil")
	}
}

func TestRequestTransportIsNotSerialized(t *testing.T) {
	req := Request{Model: "test", Transport: http.DefaultTransport}
	payload, err := json.Marshal(req)
	if err != nil {
		t.Fatal(err)
	}
	if string(payload) == "" || containsJSONKey(payload, "Transport") || containsJSONKey(payload, "transport") {
		t.Fatalf("transport leaked into request payload: %s", payload)
	}
}

func containsJSONKey(payload []byte, key string) bool {
	var object map[string]json.RawMessage
	if err := json.Unmarshal(payload, &object); err != nil {
		return false
	}
	_, ok := object[key]
	return ok
}
