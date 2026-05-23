package tools

import (
	"fmt"

	"github.com/go-json-experiment/json"
	cantotool "github.com/nijaru/canto/tool"
)

type readInput struct {
	FilePath string `json:"file_path"`
	Offset   int    `json:"offset"`
	Limit    int    `json:"limit"`
}

type writeInput struct {
	FilePath string `json:"file_path"`
	Content  string `json:"content"`
}

type editInput struct {
	FilePath string            `json:"file_path"`
	Edits    []editReplacement `json:"edits"`
}

type listInput struct {
	Path string `json:"path"`
}

type grepInput struct {
	Pattern string `json:"pattern"`
	Path    string `json:"path"`
}

type globInput struct {
	Pattern string `json:"pattern"`
}

func decodeToolArgs[A any](name, args string) (A, error) {
	var input A
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return input, fmt.Errorf("%s args: %w", name, err)
	}
	return input, nil
}

func readParameters() map[string]any {
	schema := typedParameters[readInput]([]string{"file_path"})
	describeProperty(
		schema,
		"file_path",
		"File to read, relative to the current directory or absolute.",
	)
	describeProperty(schema, "offset", "Line number to start reading from (1-indexed).")
	describeProperty(schema, "limit", "Maximum number of lines to read.")
	return schema
}

func writeParameters() map[string]any {
	schema := typedParameters[writeInput]([]string{"file_path", "content"})
	describeProperty(
		schema,
		"file_path",
		"File to write, relative to the current directory or absolute.",
	)
	describeProperty(schema, "content", "The full content to write to the file.")
	return schema
}

func editParameters() map[string]any {
	schema := typedParameters[editInput]([]string{"file_path", "edits"})
	describeProperty(
		schema,
		"file_path",
		"File to modify, relative to the current directory or absolute.",
	)
	describeProperty(
		schema,
		"edits",
		"One or more targeted replacements. Each edit is matched against the original file, not another edit's output.",
	)
	describeArrayItemProperty(
		schema,
		"edits",
		"old_string",
		"The exact text to replace. Must match the original file, not another edit's output.",
	)
	describeArrayItemProperty(schema, "edits", "new_string", "The replacement text.")
	describeArrayItemProperty(
		schema,
		"edits",
		"replace_all",
		"Replace all occurrences (default: false, requires unique match).",
	)
	describeArrayItemProperty(
		schema,
		"edits",
		"expected_replacements",
		"Optional exact number of occurrences expected. Use with replace_all for broad replacements.",
	)
	requireArrayItemProperties(schema, "edits", []string{"old_string", "new_string"})
	return schema
}

func listParameters() map[string]any {
	schema := typedParameters[listInput](nil)
	describeProperty(schema, "path", "Directory to list (default: current directory).")
	return schema
}

func grepParameters() map[string]any {
	schema := typedParameters[grepInput]([]string{"pattern"})
	describeProperty(schema, "pattern", "The regex pattern to search for.")
	describeProperty(
		schema,
		"path",
		"Directory or file to search in, relative to the current directory or absolute.",
	)
	return schema
}

func globParameters() map[string]any {
	schema := typedParameters[globInput]([]string{"pattern"})
	describeProperty(schema, "pattern", "The glob pattern to search for (e.g. '**/*.go').")
	return schema
}

func typedParameters[A any](required []string) map[string]any {
	schema, err := cantotool.SchemaFor[A]()
	if err != nil {
		panic(fmt.Sprintf("infer tool schema: %v", err))
	}
	if len(required) > 0 {
		schema["required"] = required
	}
	return schema
}

func describeProperty(schema map[string]any, name, description string) {
	properties, ok := schema["properties"].(map[string]any)
	if !ok {
		return
	}
	property, ok := properties[name].(map[string]any)
	if !ok {
		return
	}
	property["description"] = description
}

func describeArrayItemProperty(schema map[string]any, arrayName, name, description string) {
	property := arrayItemProperty(schema, arrayName, name)
	if property == nil {
		return
	}
	property["description"] = description
}

func requireArrayItemProperties(schema map[string]any, arrayName string, required []string) {
	properties, ok := schema["properties"].(map[string]any)
	if !ok {
		return
	}
	arrayProperty, ok := properties[arrayName].(map[string]any)
	if !ok {
		return
	}
	items, ok := arrayProperty["items"].(map[string]any)
	if !ok {
		return
	}
	items["required"] = required
}

func arrayItemProperty(schema map[string]any, arrayName, name string) map[string]any {
	properties, ok := schema["properties"].(map[string]any)
	if !ok {
		return nil
	}
	arrayProperty, ok := properties[arrayName].(map[string]any)
	if !ok {
		return nil
	}
	items, ok := arrayProperty["items"].(map[string]any)
	if !ok {
		return nil
	}
	itemProperties, ok := items["properties"].(map[string]any)
	if !ok {
		return nil
	}
	property, ok := itemProperties[name].(map[string]any)
	if !ok {
		return nil
	}
	return property
}
