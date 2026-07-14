package llm

import (
	"encoding/json"
	"net/http"
	"testing"
)

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
