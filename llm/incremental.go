package llm

import (
	"bytes"
	"encoding/json"
	"strings"
)

type parseState int

const (
	stateValue parseState = iota
	stateObjectKey
	stateObjectColon
	stateObjectValue
)

// isLiteralBoundary returns true if the byte before position i (or i itself
// if at the start) is a valid boundary for a JSON literal (true/false/null).
// Literals must follow ':', ',', '[', or the start of the input.
func isLiteralBoundary(res []byte, i int) bool {
	if i < 0 {
		return true
	}
	switch res[i] {
	case ':', ',', '[', ' ', '\t', '\n', '\r':
		return true
	}
	return false
}

// ParsePartialJSON attempts to repair a truncated JSON string by closing strings,
// brackets, and braces in the correct order. It also appends missing colons and null
// values for incomplete key-value pairs.
func ParsePartialJSON(input string) []byte {
	if input == "" {
		return []byte("{}")
	}

	var inString bool
	var escaped bool
	var stack []byte // stores '{' or '['

	var stateStack []parseState
	currentState := stateValue

	var result bytes.Buffer

	push := func(b byte, s parseState) {
		stack = append(stack, b)
		stateStack = append(stateStack, currentState)
		currentState = s
	}

	pop := func() {
		if len(stack) > 0 {
			stack = stack[:len(stack)-1]
			currentState = stateStack[len(stateStack)-1]
			stateStack = stateStack[:len(stateStack)-1]
		}
	}

	for i := 0; i < len(input); i++ {
		c := input[i]
		result.WriteByte(c)

		if escaped {
			escaped = false
			continue
		}
		if c == '\\' {
			escaped = true
			continue
		}
		if c == '"' {
			inString = !inString
			if !inString && currentState == stateObjectKey {
				currentState = stateObjectColon
			} else if !inString && currentState == stateObjectValue {
				currentState = stateObjectKey // expecting comma next, then key
			}
			continue
		}
		if inString {
			continue
		}

		switch c {
		case '{':
			if currentState == stateObjectColon {
				currentState = stateObjectValue
			}
			push('}', stateObjectKey)
		case '[':
			if currentState == stateObjectColon {
				currentState = stateObjectValue
			}
			push(']', stateValue)
		case '}':
			if len(stack) > 0 && stack[len(stack)-1] == '}' {
				pop()
				if currentState == stateObjectColon {
					currentState = stateObjectValue
				} else if currentState == stateObjectValue {
					currentState = stateObjectKey
				}
			}
		case ']':
			if len(stack) > 0 && stack[len(stack)-1] == ']' {
				pop()
				if currentState == stateObjectColon {
					currentState = stateObjectValue
				} else if currentState == stateObjectValue {
					currentState = stateObjectKey
				}
			}
		case ':':
			if currentState == stateObjectColon {
				currentState = stateObjectValue
			}
		case ',':
			if len(stack) > 0 && stack[len(stack)-1] == '}' {
				currentState = stateObjectKey
			}
		}
	}

	res := result.Bytes()

	if inString {
		res = append(res, '"')
		if currentState == stateObjectKey {
			currentState = stateObjectColon
		}
	}

	res = bytes.TrimRight(res, " \t\n\r")

	if len(res) > 0 && !inString {
		last := res[len(res)-1]

		if last == ',' {
			res = res[:len(res)-1]
			res = bytes.TrimRight(res, " \t\n\r")
			last = res[len(res)-1]
			if len(stack) > 0 && stack[len(stack)-1] == '}' {
				currentState = stateObjectValue // We removed the comma, so we are back to just having a value
			}
		}

		if last == ':' {
			res = append(res, []byte("null")...)
			currentState = stateObjectKey
		} else {
			var i int
			for i = len(res) - 1; i >= 0; i-- {
				c := res[i]
				if (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '-' || c == '.' {
					continue
				}
				break
			}
			word := res[i+1:]

			if len(word) > 0 && isLiteralBoundary(res, i) {
				str := string(word)
				if strings.HasPrefix("true", str) && str != "true" {
					res = append(res, []byte("true"[len(str):])...)
				} else if strings.HasPrefix("false", str) && str != "false" {
					res = append(res, []byte("false"[len(str):])...)
				} else if strings.HasPrefix("null", str) && str != "null" {
					res = append(res, []byte("null"[len(str):])...)
				}
			}
		}
	}

	// Close all open brackets/braces
	for i := len(stack) - 1; i >= 0; i-- {
		// If we are closing an object and we are expecting a colon, we have a key without value
		if stack[i] == '}' && currentState == stateObjectColon {
			res = append(res, []byte(":null")...)
		}
		res = append(res, stack[i])
		if len(stateStack) > i {
			currentState = stateStack[i]
		}
	}

	return res
}

// PartialArguments parses the incrementally streaming arguments string
// into a map, repairing any truncated JSON structure. This is useful for
// rendering tool calls in a UI before they are fully received.
func (c *Call) PartialArguments() (map[string]any, error) {
	repaired := ParsePartialJSON(c.Function.Arguments)
	var result map[string]any
	if err := json.Unmarshal(repaired, &result); err != nil {
		return nil, err
	}
	return result, nil
}
