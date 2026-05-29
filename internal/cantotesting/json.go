package testing

import "github.com/go-json-experiment/json"

func unmarshalJSON(data []byte, v any) error {
	return json.Unmarshal(data, v)
}
