package tools

import (
	"fmt"

	"github.com/go-json-experiment/json"
	cantotool "github.com/nijaru/canto/tool"
)

type readInput struct {
	Path   string `json:"path"`
	Offset int    `json:"offset"`
	Limit  int    `json:"limit"`
}

type writeInput struct {
	Path    string `json:"path"`
	Content string `json:"content"`
}

type editInput struct {
	Path  string            `json:"path"`
	Edits []editReplacement `json:"edits"`
}

type editParametersInput struct {
	Path  string                      `json:"path"`
	Edits []editParametersReplacement `json:"edits"`
}

type editParametersReplacement struct {
	OldText string `json:"oldText"`
	NewText string `json:"newText"`
}

type lsInput struct {
	Path  string `json:"path"`
	Limit int    `json:"limit"`
}

type grepInput struct {
	Pattern    string `json:"pattern"`
	Path       string `json:"path"`
	Glob       string `json:"glob"`
	IgnoreCase bool   `json:"ignoreCase"`
	Literal    bool   `json:"literal"`
	Context    int    `json:"context"`
	Limit      int    `json:"limit"`
}

type findInput struct {
	Pattern string `json:"pattern"`
	Path    string `json:"path"`
	Limit   int    `json:"limit"`
}

func decodeToolArgs[A any](name, args string) (A, error) {
	var input A
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return input, fmt.Errorf("%s args: %w", name, err)
	}
	return input, nil
}

func (i *readInput) UnmarshalJSON(data []byte) error {
	var raw struct {
		Path     string `json:"path"`
		FilePath string `json:"file_path"`
		Offset   int    `json:"offset"`
		Limit    int    `json:"limit"`
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	i.Path = raw.Path
	if i.Path == "" {
		i.Path = raw.FilePath
	}
	i.Offset = raw.Offset
	i.Limit = raw.Limit
	return nil
}

func (i *writeInput) UnmarshalJSON(data []byte) error {
	var raw struct {
		Path     string `json:"path"`
		FilePath string `json:"file_path"`
		Content  string `json:"content"`
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	i.Path = raw.Path
	if i.Path == "" {
		i.Path = raw.FilePath
	}
	i.Content = raw.Content
	return nil
}

func readParameters() map[string]any {
	schema := typedParameters[readInput]([]string{"path"})
	describeProperty(
		schema,
		"path",
		"File to read, relative to the current directory or absolute.",
	)
	describeProperty(schema, "offset", "Line number to start reading from (1-indexed).")
	describeProperty(schema, "limit", "Maximum number of lines to read.")
	return schema
}

func writeParameters() map[string]any {
	schema := typedParameters[writeInput]([]string{"path", "content"})
	describeProperty(
		schema,
		"path",
		"File to write, relative to the current directory or absolute.",
	)
	describeProperty(schema, "content", "The full content to write to the file.")
	return schema
}

func editParameters() map[string]any {
	schema := typedParameters[editParametersInput]([]string{"path", "edits"})
	describeProperty(
		schema,
		"path",
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
		"oldText",
		"The exact text to replace. Must match the original file, not another edit's output.",
	)
	describeArrayItemProperty(schema, "edits", "newText", "The replacement text.")
	requireArrayItemProperties(schema, "edits", []string{"oldText", "newText"})
	return schema
}

func lsParameters() map[string]any {
	schema := typedParameters[lsInput](nil)
	describeProperty(schema, "path", "Directory to list (default: current directory).")
	describeProperty(schema, "limit", "Maximum number of entries to return (default: 500).")
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
	describeProperty(
		schema,
		"glob",
		"Filter files by glob pattern (for example '*.go' or '**/*_test.go').",
	)
	describeProperty(schema, "ignoreCase", "Case-insensitive search (default: false).")
	describeProperty(schema, "literal", "Treat the pattern as a literal string instead of a regex.")
	describeProperty(schema, "context", "Number of context lines before and after each match.")
	describeProperty(schema, "limit", "Maximum number of matches to return (default: 100).")
	return schema
}

func findParameters() map[string]any {
	schema := typedParameters[findInput]([]string{"pattern"})
	describeProperty(schema, "pattern", "The glob pattern to search for (e.g. '**/*.go').")
	describeProperty(schema, "path", "Directory to search in (default: current directory).")
	describeProperty(schema, "limit", "Maximum number of results to return (default: 1000).")
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
