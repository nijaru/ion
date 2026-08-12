package agent

import (
	"bytes"
	"encoding/json"
	"fmt"
	"math"
	"math/big"
	"reflect"
	"regexp"
	"strconv"
	"strings"

	"github.com/google/jsonschema-go/jsonschema"
)

// Tool argument schema normalization and validation lives here so the stateless
// turn engine only coordinates provider and tool lifecycle.
// validateArgs validates the JSON Schema used by a tool before execution.
// Tool specs arrive from both native Go values and JSON-encoded schemas, so the
// boundary normalizes both forms before recursively checking required fields,
// nested objects/arrays, primitive types, enums, and object closure.
func validateArgs(params any, args map[string]any) error {
	_, err := coerceAndValidateArgs(params, args)
	return err
}

func coerceAndValidateArgs(params any, args map[string]any) (map[string]any, error) {
	schema, ok, err := normalizeSchemaMap(params)
	if err != nil || !ok {
		return args, err
	}
	data, err := json.Marshal(args)
	if err != nil {
		return args, fmt.Errorf("invalid tool arguments: %w", err)
	}
	var normalized map[string]any
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	if err := decoder.Decode(&normalized); err != nil {
		return args, fmt.Errorf("invalid tool arguments: %w", err)
	}
	if coerced, ok := coerceSchemaValue(schema, normalized).(map[string]any); ok {
		normalized = coerced
	}
	if err := validateExactNumericSchema(schema, normalized, "root"); err != nil {
		return args, err
	}
	if err := validateJSONSchema(schema, normalized); err != nil {
		return args, err
	}
	return normalized, nil
}

func exactRational(value any) (*big.Rat, bool) {
	switch number := value.(type) {
	case json.Number:
		rational, valid := new(big.Rat).SetString(number.String())
		return rational, valid
	case float64:
		if math.IsNaN(number) || math.IsInf(number, 0) {
			return nil, false
		}
		rational, valid := new(big.Rat).SetString(strconv.FormatFloat(number, 'g', -1, 64))
		return rational, valid
	case float32:
		return exactRational(float64(number))
	case int:
		return new(big.Rat).SetInt64(int64(number)), true
	case int8:
		return new(big.Rat).SetInt64(int64(number)), true
	case int16:
		return new(big.Rat).SetInt64(int64(number)), true
	case int32:
		return new(big.Rat).SetInt64(int64(number)), true
	case int64:
		return new(big.Rat).SetInt64(number), true
	case uint:
		return new(big.Rat).SetUint64(uint64(number)), true
	case uint8:
		return new(big.Rat).SetUint64(uint64(number)), true
	case uint16:
		return new(big.Rat).SetUint64(uint64(number)), true
	case uint32:
		return new(big.Rat).SetUint64(uint64(number)), true
	case uint64:
		return new(big.Rat).SetUint64(number), true
	case uintptr:
		return new(big.Rat).SetUint64(uint64(number)), true
	default:
		return nil, false
	}
}

func validateExactNumericSchema(schema map[string]any, instance any, path string) error {
	if boolean, marked := schema[booleanSchemaMarker].(bool); marked {
		if !boolean {
			return fmt.Errorf("%s is disallowed by schema", path)
		}
		return nil
	}
	if alternatives := schemaSchemas(schema["allOf"]); alternatives != nil {
		for _, alternative := range alternatives {
			if err := validateExactNumericSchema(alternative, instance, path); err != nil {
				return err
			}
		}
	}
	for _, keyword := range []string{"anyOf", "oneOf"} {
		if alternatives := schemaSchemas(schema[keyword]); alternatives != nil {
			var firstErr error
			for _, alternative := range alternatives {
				if err := validateExactNumericSchema(alternative, instance, path); err == nil {
					firstErr = nil
					break
				} else if firstErr == nil {
					firstErr = err
				}
			}
			if firstErr != nil {
				return firstErr
			}
		}
	}
	types := schemaTypes(schema["type"])
	hasNumberType := false
	for _, typ := range types {
		if typ == "number" {
			hasNumberType = true
			break
		}
	}
	if number, ok := instance.(json.Number); ok && len(types) > 0 {
		matched := false
		for _, typ := range types {
			if schemaTypeMatches(typ, number) {
				matched = true
				break
			}
		}
		if !matched {
			return fmt.Errorf("%s has an incompatible type", path)
		}
	}
	for _, typ := range types {
		if number, ok := instance.(json.Number); ok {
			if typ == "integer" {
				rational, valid := new(big.Rat).SetString(number.String())
				if (!valid || !rational.IsInt()) && !hasNumberType {
					return fmt.Errorf("%s must be an integer", path)
				}
			}
			if typ == "number" {
				parsed, err := number.Float64()
				if err != nil || math.IsNaN(parsed) || math.IsInf(parsed, 0) {
					return fmt.Errorf("%s must be a finite number", path)
				}
			}
		}
	}
	if number, ok := exactRational(instance); ok {
		if expected, exists := schema["const"]; exists {
			if expectedNumber, numeric := exactRational(expected); numeric {
				if number.Cmp(expectedNumber) != 0 {
					return fmt.Errorf("%s does not equal const", path)
				}
			}
		}
		if values, ok := schema["enum"].([]any); ok {
			matched := false
			for _, value := range values {
				if expectedNumber, numeric := exactRational(value); numeric && number.Cmp(expectedNumber) == 0 {
					matched = true
					break
				}
			}
			if !matched {
				return fmt.Errorf("%s is not in enum", path)
			}
		}
		if minimum, valid := exactRational(schema["minimum"]); valid && number.Cmp(minimum) < 0 {
			return fmt.Errorf("%s is below minimum", path)
		}
		exclusiveMinimum := schema["exclusiveMinimum"]
		if enabled, ok := exclusiveMinimum.(bool); ok && enabled {
			exclusiveMinimum = schema["minimum"]
		}
		if minimum, valid := exactRational(exclusiveMinimum); valid && number.Cmp(minimum) <= 0 {
			return fmt.Errorf("%s is not above exclusive minimum", path)
		}
		if maximum, valid := exactRational(schema["maximum"]); valid && number.Cmp(maximum) > 0 {
			return fmt.Errorf("%s exceeds maximum", path)
		}
		exclusiveMaximum := schema["exclusiveMaximum"]
		if enabled, ok := exclusiveMaximum.(bool); ok && enabled {
			exclusiveMaximum = schema["maximum"]
		}
		if maximum, valid := exactRational(exclusiveMaximum); valid && number.Cmp(maximum) >= 0 {
			return fmt.Errorf("%s is not below exclusive maximum", path)
		}
		if multiple, valid := exactRational(schema["multipleOf"]); valid && multiple.Sign() > 0 {
			quotient := new(big.Rat).Quo(number, multiple)
			if !quotient.IsInt() {
				return fmt.Errorf("%s is not a multiple of %v", path, multiple)
			}
		}
	}
	if object, ok := schemaObject(instance); ok {
		properties, err := schemaPropertyValues(schema["properties"])
		if err != nil {
			return err
		}
		for name, rawProperty := range properties {
			value, exists := object[name]
			if !exists {
				continue
			}
			if boolean, isBooleanSchema := rawProperty.(bool); isBooleanSchema {
				if !boolean {
					return fmt.Errorf("%s is disallowed by schema", joinValidationPath(path, name))
				}
				continue
			}
			property, valid, err := normalizeSchemaMap(rawProperty)
			if err != nil {
				return err
			}
			if !valid {
				return fmt.Errorf("property %q is not a schema", name)
			}
			if err := validateExactNumericSchema(property, value, joinValidationPath(path, name)); err != nil {
				return err
			}
		}
		additional := schema["additionalProperties"]
		additionalSchema, additionalValid, err := normalizeSchemaMapValue(additional)
		if err != nil && additional != nil {
			return err
		}
		patterns, err := schemaPropertyValues(schema["patternProperties"])
		if err != nil {
			return err
		}
		for name, value := range object {
			matchedPattern := false
			for pattern, rawPattern := range patterns {
				matched, matchErr := regexp.MatchString(pattern, name)
				if matchErr != nil {
					return matchErr
				}
				if !matched {
					continue
				}
				matchedPattern = true
				if boolean, isBooleanSchema := rawPattern.(bool); isBooleanSchema {
					if !boolean {
						return fmt.Errorf("%s is disallowed by schema", joinValidationPath(path, name))
					}
					continue
				}
				patternSchema, valid, normalizeErr := normalizeSchemaMap(rawPattern)
				if normalizeErr != nil {
					return normalizeErr
				}
				if valid {
					if err := validateExactNumericSchema(
						patternSchema,
						value,
						joinValidationPath(path, name),
					); err != nil {
						return err
					}
				}
			}
			if matchedPattern {
				continue
			}
			if _, defined := properties[name]; defined {
				continue
			}
			if boolean, isBooleanSchema := additional.(bool); isBooleanSchema {
				if !boolean {
					return fmt.Errorf("%s is disallowed by schema", joinValidationPath(path, name))
				}
			} else if additionalValid {
				if err := validateExactNumericSchema(
					additionalSchema,
					value,
					joinValidationPath(path, name),
				); err != nil {
					return err
				}
			}
		}
	}
	if array, ok := schemaArray(instance); ok {
		if tuple, valid := schema["items"].([]any); valid {
			for i, value := range array {
				if i >= len(tuple) {
					break
				}
				if boolean, isBooleanSchema := tuple[i].(bool); isBooleanSchema {
					if !boolean {
						return fmt.Errorf("%s[%d] is disallowed by schema", path, i)
					}
					continue
				}
				items, valid, err := normalizeSchemaMap(tuple[i])
				if err != nil {
					return err
				}
				if valid {
					if err := validateExactNumericSchema(items, value, fmt.Sprintf("%s[%d]", path, i)); err != nil {
						return err
					}
				}
			}
		} else if items, valid, err := normalizeSchemaMapValue(schema["items"]); err != nil {
			return err
		} else if valid {
			for i, value := range array {
				if err := validateExactNumericSchema(items, value, fmt.Sprintf("%s[%d]", path, i)); err != nil {
					return err
				}
			}
		}
		if prefix, valid := schema["prefixItems"].([]any); valid {
			for i, value := range array {
				if i >= len(prefix) {
					break
				}
				if boolean, isBooleanSchema := prefix[i].(bool); isBooleanSchema {
					if !boolean {
						return fmt.Errorf("%s[%d] is disallowed by schema", path, i)
					}
					continue
				}
				items, valid, err := normalizeSchemaMap(prefix[i])
				if err != nil {
					return err
				}
				if valid {
					if err := validateExactNumericSchema(items, value, fmt.Sprintf("%s[%d]", path, i)); err != nil {
						return err
					}
				}
			}
		}
	}

	return nil
}

func joinValidationPath(path, name string) string {
	if path == "root" {
		return name
	}
	return path + "." + name
}

func validateSchemaValue(schema map[string]any, instance any) error {
	if err := validateExactNumericSchema(schema, instance, "root"); err != nil {
		return err
	}
	return validateJSONSchema(schema, instance)
}

func validateJSONSchema(value any, instance any) error {
	data, err := json.Marshal(normalizeSchemaForValidator(value))
	if err != nil {
		return fmt.Errorf("invalid tool schema: %w", err)
	}
	var schema jsonschema.Schema
	if err := json.Unmarshal(data, &schema); err != nil {
		return fmt.Errorf("invalid tool schema: %w", err)
	}
	resolved, err := schema.Resolve(nil)
	if err != nil {
		return fmt.Errorf("invalid tool schema: %w", err)
	}
	return resolved.Validate(normalizeNumbersForValidation(cloneSchemaValue(instance)))
}

func normalizeSchemaForValidator(value any) any {
	switch value := value.(type) {
	case map[string]any:
		if boolean, marked := value[booleanSchemaMarker]; marked && len(value) == 1 {
			return normalizeSchemaForValidator(boolean)
		}
		result := make(map[string]any, len(value))
		for key, nested := range value {
			if key == "minimum" || key == "maximum" || key == "exclusiveMinimum" || key == "exclusiveMaximum" ||
				key == "multipleOf" {
				continue
			}
			result[key] = normalizeSchemaForValidator(nested)
		}
		if tuple, ok := value["items"].([]any); ok {
			delete(result, "items")
			result["prefixItems"] = normalizeSchemaForValidator(tuple)
			if additional, exists := value["additionalItems"]; exists {
				result["items"] = normalizeSchemaForValidator(additional)
			}
		}
		return result
	case []any:
		result := make([]any, len(value))
		for i, nested := range value {
			result[i] = normalizeSchemaForValidator(nested)
		}
		return result
	case bool:
		if value {
			return map[string]any{}
		}
		return map[string]any{"not": map[string]any{}}
	default:
		return value
	}
}

func normalizeNumbersForValidation(value any) any {
	switch value := value.(type) {
	case json.Number:
		if number, err := value.Float64(); err == nil {
			return number
		}
	case map[string]any:
		for key, nested := range value {
			value[key] = normalizeNumbersForValidation(nested)
		}
	case []any:
		for i, nested := range value {
			value[i] = normalizeNumbersForValidation(nested)
		}
	}
	return value
}

func coerceSchemaValue(schema map[string]any, value any) any {
	if alternatives := schemaSchemas(schema["allOf"]); alternatives != nil {
		for _, alternative := range alternatives {
			value = coerceSchemaValue(alternative, value)
		}
	}
	if alternatives := schemaAlternativeValues(schema["anyOf"]); alternatives != nil {
		value = coerceSchemaUnion(value, alternatives)
	}
	if alternatives := schemaAlternativeValues(schema["oneOf"]); alternatives != nil {
		value = coerceSchemaUnion(value, alternatives)
	}

	types := schemaTypes(schema["type"])
	matches := false
	for _, typ := range types {
		if schemaTypeMatches(typ, value) {
			matches = true
			break
		}
	}
	if len(types) > 0 && !matches {
		for _, typ := range types {
			if converted, ok := coercePrimitiveByType(value, typ); ok {
				value = converted
				break
			}
		}
	}
	if object, ok := schemaObject(value); ok {
		properties, _ := schemaPropertyValues(schema["properties"])
		for name, rawProperty := range properties {
			if current, exists := object[name]; exists {
				if property, valid, _ := normalizeSchemaMap(rawProperty); valid {
					object[name] = coerceSchemaValue(property, current)
				}
			}
		}
		if additional, valid, _ := normalizeSchemaMapValue(schema["additionalProperties"]); valid {
			for name, current := range object {
				if _, defined := properties[name]; !defined {
					object[name] = coerceSchemaValue(additional, current)
				}
			}
		}
	}
	if array, ok := schemaArray(value); ok {
		if tuple, valid := schema["items"].([]any); valid {
			for i, item := range array {
				if i < len(tuple) {
					if itemSchema, valid, _ := normalizeSchemaMap(tuple[i]); valid {
						array[i] = coerceSchemaValue(itemSchema, item)
					}
				}
			}
		} else if items, valid, _ := normalizeSchemaMapValue(schema["items"]); valid {
			for i, item := range array {
				array[i] = coerceSchemaValue(items, item)
			}
		}
	}
	return value
}

func coerceSchemaUnion(value any, alternatives []any) any {
	for _, rawAlternative := range alternatives {
		alternative, valid, err := normalizeSchemaMap(rawAlternative)
		if err != nil || !valid {
			if boolean, ok := rawAlternative.(bool); ok && boolean {
				return value
			}
			continue
		}
		candidate := cloneSchemaValue(value)
		candidate = coerceSchemaValue(alternative, candidate)
		if validateSchemaValue(alternative, candidate) == nil {
			return candidate
		}
	}
	return value
}

func cloneSchemaValue(value any) any {
	data, err := json.Marshal(value)
	if err != nil {
		return value
	}
	var clone any
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	if err := decoder.Decode(&clone); err != nil {
		return value
	}
	return clone
}

func coercePrimitiveByType(value any, typ string) (any, bool) {
	switch typ {
	case "number":
		if value == nil {
			return float64(0), true
		}
		if text, ok := value.(string); ok && strings.TrimSpace(text) != "" {
			if number, err := strconv.ParseFloat(
				text,
				64,
			); err == nil && !math.IsNaN(number) &&
				!math.IsInf(number, 0) {
				return number, true
			}
		}
		if boolean, ok := value.(bool); ok {
			if boolean {
				return float64(1), true
			}
			return float64(0), true
		}
	case "integer":
		if value == nil {
			return float64(0), true
		}
		if text, ok := value.(string); ok && strings.TrimSpace(text) != "" {
			if number, err := strconv.ParseFloat(
				text,
				64,
			); err == nil && math.Trunc(number) == number &&
				!math.IsInf(number, 0) {
				return number, true
			}
		}
		if boolean, ok := value.(bool); ok {
			if boolean {
				return float64(1), true
			}
			return float64(0), true
		}
	case "boolean":
		if value == nil {
			return false, true
		}
		if text, ok := value.(string); ok {
			if text == "true" {
				return true, true
			}
			if text == "false" {
				return false, true
			}
		}
		if number, ok := schemaNumber(value); ok {
			if number == 1 {
				return true, true
			}
			if number == 0 {
				return false, true
			}
		}
	case "string":
		if value == nil {
			return "", true
		}
		switch converted := value.(type) {
		case bool:
			return strconv.FormatBool(converted), true
		case float64:
			return strconv.FormatFloat(converted, 'f', -1, 64), true
		case json.Number:
			return converted.String(), true
		}
	case "null":
		if value == "" || value == false {
			return nil, true
		}
		if number, ok := schemaNumber(value); ok && number == 0 {
			return nil, true
		}
	}
	return value, false
}

func normalizeSchemaMap(value any) (map[string]any, bool, error) {
	if value == nil {
		return nil, false, nil
	}
	if schema, ok := value.(map[string]any); ok {
		return schema, true, nil
	}
	var data []byte
	switch schema := value.(type) {
	case string:
		data = []byte(schema)
	case json.RawMessage:
		data = []byte(schema)
	default:
		encoded, err := json.Marshal(value)
		if err != nil {
			return nil, false, fmt.Errorf("invalid tool schema: %w", err)
		}
		data = encoded
	}
	var schema map[string]any
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	if err := decoder.Decode(&schema); err != nil {
		return nil, false, fmt.Errorf("invalid tool schema: %w", err)
	}
	if schema == nil {
		return nil, false, nil
	}
	return schema, true, nil
}

func schemaTypes(value any) []string {
	if text, ok := value.(string); ok {
		return []string{text}
	}
	return schemaStrings(value)
}

func schemaStrings(value any) []string {
	var result []string
	switch values := value.(type) {
	case []string:
		return append(result, values...)
	case []any:
		for _, value := range values {
			if text, ok := value.(string); ok {
				result = append(result, text)
			}
		}
	}
	return result
}

func schemaTypeMatches(typ string, value any) bool {
	switch typ {
	case "string":
		_, ok := value.(string)
		return ok
	case "number":
		_, ok := schemaNumber(value)
		return ok
	case "integer":
		if number, ok := value.(json.Number); ok {
			rational, valid := exactRational(number)
			return valid && rational.IsInt()
		}
		v := reflect.ValueOf(value)
		if !v.IsValid() {
			return false
		}
		switch v.Kind() {
		case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64,
			reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
			return true
		case reflect.Float32, reflect.Float64:
			return math.Trunc(v.Float()) == v.Float()
		default:
			return false
		}
	case "boolean":
		_, ok := value.(bool)
		return ok
	case "array":
		_, ok := schemaArray(value)
		return ok
	case "object":
		_, ok := schemaObject(value)
		return ok
	case "null":
		return value == nil
	default:
		return false
	}
}

func schemaNumber(value any) (float64, bool) {
	if number, ok := value.(json.Number); ok {
		parsed, err := number.Float64()
		return parsed, err == nil && !math.IsNaN(parsed) && !math.IsInf(parsed, 0)
	}
	v := reflect.ValueOf(value)
	if !v.IsValid() {
		return 0, false
	}
	switch v.Kind() {
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		return float64(v.Int()), true
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
		return float64(v.Uint()), true
	case reflect.Float32, reflect.Float64:
		parsed := v.Float()
		return parsed, !math.IsNaN(parsed) && !math.IsInf(parsed, 0)
	default:
		return 0, false
	}
}

func schemaObject(value any) (map[string]any, bool) {
	object, ok := value.(map[string]any)
	return object, ok
}

func schemaArray(value any) ([]any, bool) {
	if array, ok := value.([]any); ok {
		return array, true
	}
	v := reflect.ValueOf(value)
	if !v.IsValid() || (v.Kind() != reflect.Array && v.Kind() != reflect.Slice) {
		return nil, false
	}
	array := make([]any, v.Len())
	for i := range array {
		array[i] = v.Index(i).Interface()
	}
	return array, true
}

func schemaPropertyValues(value any) (map[string]any, error) {
	if value == nil {
		return nil, nil
	}
	if properties, ok := value.(map[string]any); ok {
		return properties, nil
	}
	data, err := json.Marshal(value)
	if err != nil {
		return nil, err
	}
	properties := map[string]any{}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	if err := decoder.Decode(&properties); err != nil {
		return nil, err
	}
	return properties, nil
}

func normalizeSchemaMapValue(value any) (map[string]any, bool, error) {
	if _, boolean := value.(bool); boolean {
		return nil, false, nil
	}
	schema, ok, err := normalizeSchemaMap(value)
	return schema, ok, err
}

const booleanSchemaMarker = "__ion_boolean_schema"

func schemaAlternativeValues(value any) []any {
	values, ok := schemaArray(value)
	if !ok {
		return nil
	}
	return values
}

func schemaSchemas(value any) []map[string]any {
	values := schemaAlternativeValues(value)
	if values == nil {
		return nil
	}
	result := make([]map[string]any, 0, len(values))
	for _, value := range values {
		if boolean, ok := value.(bool); ok {
			result = append(result, map[string]any{booleanSchemaMarker: boolean})
			continue
		}
		schema, valid, err := normalizeSchemaMap(value)
		if err != nil || !valid {
			return nil
		}
		result = append(result, schema)
	}
	return result
}

// --- helpers ---
