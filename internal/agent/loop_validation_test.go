package agent

import (
	"encoding/json"
	"math"
	"strings"
	"testing"
)

func TestDecodeToolArgumentsPreservesJSONNumbers(t *testing.T) {
	args := decodeToolArguments(`{"count":9007199254740993}`)
	if _, ok := args["count"].(json.Number); !ok {
		t.Fatalf("count = %T, want json.Number", args["count"])
	}
}

func TestValidateArgsRejectsLargeFractionalInteger(t *testing.T) {
	schema := map[string]any{
		"type":       "object",
		"properties": map[string]any{"count": map[string]any{"type": "integer"}},
	}
	if _, err := coerceAndValidateArgs(schema, map[string]any{"count": json.Number("9007199254740993.1")}); err == nil {
		t.Fatal("large fractional integer accepted")
	}
}

func TestValidateArgsUsesExactNumbersInAdditionalProperties(t *testing.T) {
	schema := map[string]any{"type": "object", "additionalProperties": map[string]any{
		"type": "number", "enum": []any{json.Number("9007199254740993")},
	}}
	if err := validateArgs(schema, map[string]any{"value": json.Number("9007199254740992")}); err == nil {
		t.Fatal("distinct large additional property accepted")
	}
}

func TestValidateArgsUsesExactNumericConstraints(t *testing.T) {
	integerSchema := map[string]any{"type": "object", "properties": map[string]any{
		"value": map[string]any{"type": "integer"},
	}}
	if err := validateArgs(integerSchema, map[string]any{"value": json.Number("9223372036854775808")}); err != nil {
		t.Fatalf("large integer rejected: %v", err)
	}
	bounded := map[string]any{"type": "object", "properties": map[string]any{
		"value": map[string]any{"type": "number", "minimum": json.Number("9007199254740993")},
	}}
	if err := validateArgs(bounded, map[string]any{"value": json.Number("9007199254740992.5")}); err == nil {
		t.Fatal("value below exact minimum accepted")
	}
	enumerated := map[string]any{"type": "object", "properties": map[string]any{
		"value": map[string]any{"type": "number", "enum": []any{json.Number("9007199254740993")}},
	}}
	if err := validateArgs(enumerated, map[string]any{"value": json.Number("9007199254740992")}); err == nil {
		t.Fatal("distinct large enum value accepted")
	}
}

func TestValidateArgsPreservesLargeJSONNumbers(t *testing.T) {
	schema := map[string]any{
		"type":       "object",
		"properties": map[string]any{"count": map[string]any{"type": "integer"}},
	}
	want := json.Number("9007199254740993")
	got, err := coerceAndValidateArgs(schema, map[string]any{"count": want})
	if err != nil {
		t.Fatalf("large integer rejected: %v", err)
	}
	if got["count"] != want {
		t.Fatalf("large integer = %#v, want %s", got["count"], want)
	}
}

func TestValidateArgsSupportsBooleanUnionSchemas(t *testing.T) {
	schema := map[string]any{"type": "object", "properties": map[string]any{
		"value": map[string]any{"anyOf": []any{false, map[string]any{"type": "integer"}}},
	}}
	got, err := coerceAndValidateArgs(schema, map[string]any{"value": "3"})
	if err != nil || got["value"] != float64(3) {
		t.Fatalf("boolean union coercion = %#v, err=%v", got, err)
	}
}

func TestValidateArgsChecksNumericConstraintsDuringUnionCoercion(t *testing.T) {
	schema := map[string]any{"type": "object", "properties": map[string]any{
		"value": map[string]any{"anyOf": []any{
			map[string]any{"type": "number", "minimum": json.Number("10")},
			map[string]any{"type": "string"},
		}},
	}}
	if err := validateArgs(schema, map[string]any{"value": "5"}); err != nil {
		t.Fatalf("string union branch rejected: %v", err)
	}
}

func TestValidateArgsSupportsPatternPropertiesWithClosedObjects(t *testing.T) {
	schema := map[string]any{
		"type":                 "object",
		"patternProperties":    map[string]any{"^x-": map[string]any{"type": "integer"}},
		"additionalProperties": false,
	}
	if err := validateArgs(schema, map[string]any{"x-count": json.Number("2")}); err != nil {
		t.Fatalf("pattern property rejected: %v", err)
	}
	if err := validateArgs(schema, map[string]any{"other": true}); err == nil {
		t.Fatal("unmatched property accepted")
	}
}

func TestValidateArgsAppliesPatternsToNamedProperties(t *testing.T) {
	schema := map[string]any{"type": "object", "properties": map[string]any{
		"x-count": map[string]any{"type": "number"},
	}, "patternProperties": map[string]any{
		"^x-": map[string]any{"minimum": json.Number("10")},
	}}
	if err := validateArgs(schema, map[string]any{"x-count": json.Number("5")}); err == nil {
		t.Fatal("pattern constraint skipped for named property")
	}
}

func TestValidateArgsSupportsBooleanTupleItems(t *testing.T) {
	schema := map[string]any{"type": "object", "properties": map[string]any{
		"pair": map[string]any{"type": "array", "items": []any{true, map[string]any{"type": "integer"}}},
	}}
	if err := validateArgs(schema, map[string]any{"pair": []any{"anything", json.Number("2")}}); err != nil {
		t.Fatalf("boolean tuple item rejected: %v", err)
	}
}

func TestValidateArgsUsesExactMultipleOf(t *testing.T) {
	schema := map[string]any{"type": "object", "properties": map[string]any{
		"value": map[string]any{"type": "number", "multipleOf": json.Number("0.1")},
	}}
	if err := validateArgs(schema, map[string]any{"value": json.Number("0.3")}); err != nil {
		t.Fatalf("exact multiple rejected: %v", err)
	}
}

func TestValidateArgsPreservesNumericUnionSemantics(t *testing.T) {
	schema := map[string]any{
		"type": "object",
		"properties": map[string]any{
			"value": map[string]any{"anyOf": []any{
				map[string]any{"type": "integer"},
				map[string]any{"type": "number"},
			}},
		},
	}
	if err := validateArgs(schema, map[string]any{"value": json.Number("9007199254740993.1")}); err != nil {
		t.Fatalf("number branch rejected valid fractional value: %v", err)
	}
	if err := validateArgs(schema, map[string]any{"value": json.Number("9007199254740993.2")}); err != nil {
		t.Fatalf("number branch rejected distinct fractional value: %v", err)
	}
	booleanUnion := map[string]any{
		"type": "object",
		"properties": map[string]any{"value": map[string]any{"anyOf": []any{
			map[string]any{"type": "integer"},
			map[string]any{"type": "boolean"},
		}}},
	}
	if err := validateArgs(booleanUnion, map[string]any{"value": json.Number("9007199254740993.1")}); err == nil {
		t.Fatal("fractional number accepted by integer/boolean union")
	}
}

func TestValidateArgsNumericKeywordsOnlyApplyToNumbers(t *testing.T) {
	schema := map[string]any{"type": "object", "properties": map[string]any{
		"value": map[string]any{"type": "string", "minimum": 10},
	}}
	if err := validateArgs(schema, map[string]any{"value": "5"}); err != nil {
		t.Fatalf("numeric keyword applied to string: %v", err)
	}
}

func TestValidateArgsSupportsTupleItems(t *testing.T) {
	schema := map[string]any{"type": "object", "properties": map[string]any{
		"pair": map[string]any{"type": "array", "items": []any{
			map[string]any{"type": "string"},
			map[string]any{"type": "integer"},
		}},
	}}
	if err := validateArgs(schema, map[string]any{"pair": []any{"ion", json.Number("2")}}); err != nil {
		t.Fatalf("valid tuple rejected: %v", err)
	}
	if err := validateArgs(schema, map[string]any{"pair": []any{"ion", "two"}}); err == nil {
		t.Fatal("invalid tuple accepted")
	}
}

func TestValidateArgsCoercesPrimitiveValues(t *testing.T) {
	schema := map[string]any{
		"type": "object",
		"properties": map[string]any{
			"count":   map[string]any{"type": "integer"},
			"enabled": map[string]any{"type": "boolean"},
		},
	}
	got, err := coerceAndValidateArgs(schema, map[string]any{"count": "3", "enabled": float64(1)})
	if err != nil {
		t.Fatalf("coercible values rejected: %v", err)
	}
	if got["count"] != float64(3) || got["enabled"] != true {
		t.Fatalf("coerced args = %#v, want integer 3 and boolean true", got)
	}
}

func TestValidateArgsAcceptsJSONSchemaStringAndRequiredStringSlice(t *testing.T) {
	schema := `{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}`
	if err := validateArgs(schema, map[string]any{}); err == nil || !strings.Contains(err.Error(), "command") {
		t.Fatalf("missing required argument error = %v, want command", err)
	}
	if err := validateArgs(schema, map[string]any{"command": "go test"}); err != nil {
		t.Fatalf("valid JSON schema string rejected: %v", err)
	}
}

func TestValidateArgsChecksNestedObjectsAndArrays(t *testing.T) {
	schema := map[string]any{
		"type": "object",
		"properties": map[string]any{
			"config": map[string]any{
				"type":     "object",
				"required": []string{"enabled"},
				"properties": map[string]any{
					"enabled": map[string]any{"type": "boolean"},
				},
			},
			"paths": map[string]any{
				"type":  "array",
				"items": map[string]any{"type": "string"},
			},
		},
	}
	if err := validateArgs(schema, map[string]any{
		"config": map[string]any{"enabled": true},
		"paths":  []any{"one", "two"},
	}); err != nil {
		t.Fatalf("valid nested schema rejected: %v", err)
	}
	if err := validateArgs(schema, map[string]any{
		"config": map[string]any{"enabled": "yes"},
		"paths":  []any{"one"},
	}); err == nil || !strings.Contains(err.Error(), "enabled") {
		t.Fatalf("nested type error = %v, want enabled", err)
	}
	if err := validateArgs(schema, map[string]any{
		"config": map[string]any{"enabled": true},
		"paths":  []any{"one", map[string]any{}},
	}); err == nil || !strings.Contains(err.Error(), "items") {
		t.Fatalf("array item error = %v, want item path", err)
	}
}

func TestValidateArgsRejectsNonFiniteNumbersAndSupportsAlternatives(t *testing.T) {
	numberSchema := map[string]any{
		"type":       "object",
		"properties": map[string]any{"value": map[string]any{"type": "number"}},
	}
	for _, value := range []any{math.NaN(), math.Inf(1), math.Inf(-1)} {
		if err := validateArgs(numberSchema, map[string]any{"value": value}); err == nil {
			t.Fatalf("non-finite number %v accepted", value)
		}
	}
	alternative := map[string]any{
		"type": "object",
		"anyOf": []any{
			map[string]any{"required": []string{"path"}},
			map[string]any{"required": []string{"query"}},
		},
	}
	if err := validateArgs(alternative, map[string]any{}); err == nil {
		t.Fatal("anyOf schema accepted object matching no branch")
	}
	if err := validateArgs(alternative, map[string]any{"query": "ion"}); err != nil {
		t.Fatalf("anyOf schema rejected matching branch: %v", err)
	}
	typedAlternative := map[string]any{
		"type":  "object",
		"anyOf": []map[string]any{{"required": []string{"path"}}},
	}
	if err := validateArgs(typedAlternative, map[string]any{}); err == nil {
		t.Fatal("typed anyOf schema accepted object matching no branch")
	}
}

func TestValidateArgsChecksCommonSchemaConstraints(t *testing.T) {
	schema := map[string]any{
		"type": "object",
		"properties": map[string]any{
			"name":  map[string]any{"type": "string", "pattern": "^[a-z]+$"},
			"score": map[string]any{"type": "number", "minimum": 1},
			"items": map[string]any{"type": "array", "minItems": 2},
		},
	}
	if err := validateArgs(schema, map[string]any{"name": "ion", "score": 2, "items": []any{"a", "b"}}); err != nil {
		t.Fatalf("valid constrained schema rejected: %v", err)
	}
	for name, args := range map[string]map[string]any{
		"pattern": map[string]any{"name": "Ion", "score": 2, "items": []any{"a", "b"}},
		"minimum": map[string]any{"name": "ion", "score": 0, "items": []any{"a", "b"}},
		"items":   map[string]any{"name": "ion", "score": 2, "items": []any{"a"}},
	} {
		if err := validateArgs(schema, args); err == nil {
			t.Errorf("%s constraint accepted", name)
		}
	}
}

func TestValidateArgsSupportsBooleanSchemas(t *testing.T) {
	closed := map[string]any{"type": "object", "properties": map[string]any{
		"allowed": true,
	}, "additionalProperties": false}
	if err := validateArgs(closed, map[string]any{"allowed": "yes"}); err != nil {
		t.Fatalf("boolean true property rejected: %v", err)
	}
	if err := validateArgs(closed, map[string]any{}); err != nil {
		t.Fatalf("valid closed object rejected: %v", err)
	}
	open := map[string]any{"type": "object", "additionalProperties": true}
	if err := validateArgs(open, map[string]any{"anything": 1}); err != nil {
		t.Fatalf("boolean true additionalProperties rejected: %v", err)
	}
	mixed := map[string]any{"type": "object", "properties": map[string]any{
		"flag": true,
	}, "additionalProperties": map[string]any{"type": "integer"}}
	got, err := coerceAndValidateArgs(mixed, map[string]any{"flag": true, "count": "2"})
	if err != nil || got["flag"] != true || got["count"] != float64(2) {
		t.Fatalf("mixed boolean/additional schema = %#v, err=%v", got, err)
	}
}

func TestValidateArgsRejectsUnknownPropertiesWhenClosed(t *testing.T) {
	schema := map[string]any{
		"type":                 "object",
		"additionalProperties": false,
		"properties": map[string]any{
			"name": map[string]any{"type": "string"},
		},
	}
	if err := validateArgs(schema, map[string]any{"name": "ion", "extra": true}); err == nil {
		t.Fatal("unknown property accepted by closed schema")
	}
}

func TestValidateArgsRejectsMalformedNestedSchema(t *testing.T) {
	schema := map[string]any{
		"type": "object",
		"properties": map[string]any{
			"name": false,
		},
	}
	if err := validateArgs(schema, map[string]any{"name": "ion"}); err == nil {
		t.Fatal("malformed nested schema accepted")
	}
}

func TestValidateArgsRejectsMalformedSchema(t *testing.T) {
	if err := validateArgs(`{"type":`, map[string]any{}); err == nil {
		t.Fatal("malformed schema accepted")
	}
}
