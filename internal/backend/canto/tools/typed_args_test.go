package tools

import (
	"slices"
	"testing"
)

func TestP1ToolSchemasUseTypedArgumentShapes(t *testing.T) {
	fileTool := NewFileTool(t.TempDir())
	searchTool := NewSearchTool(t.TempDir())
	tests := []struct {
		name       string
		spec       map[string]any
		properties []string
		required   []string
	}{
		{
			name:       "read",
			spec:       (&Read{FileTool: *fileTool}).Spec().Parameters.(map[string]any),
			properties: []string{"file_path", "offset", "limit"},
			required:   []string{"file_path"},
		},
		{
			name:       "write",
			spec:       (&Write{FileTool: *fileTool}).Spec().Parameters.(map[string]any),
			properties: []string{"file_path", "content"},
			required:   []string{"file_path", "content"},
		},
		{
			name:       "edit",
			spec:       (&Edit{FileTool: *fileTool}).Spec().Parameters.(map[string]any),
			properties: []string{"file_path", "edits"},
			required:   []string{"file_path", "edits"},
		},
		{
			name:       "list",
			spec:       (&List{FileTool: *fileTool}).Spec().Parameters.(map[string]any),
			properties: []string{"path"},
		},
		{
			name:       "grep",
			spec:       (&Grep{SearchTool: *searchTool}).Spec().Parameters.(map[string]any),
			properties: []string{"pattern", "path"},
			required:   []string{"pattern"},
		},
		{
			name:       "glob",
			spec:       (&Glob{SearchTool: *searchTool}).Spec().Parameters.(map[string]any),
			properties: []string{"pattern"},
			required:   []string{"pattern"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := tt.spec["type"]; got != "object" {
				t.Fatalf("schema type = %v, want object", got)
			}
			properties, ok := tt.spec["properties"].(map[string]any)
			if !ok {
				t.Fatalf("properties = %T, want map[string]any", tt.spec["properties"])
			}
			for _, property := range tt.properties {
				if _, ok := properties[property]; !ok {
					t.Fatalf("missing property %q in %#v", property, properties)
				}
			}
			required := schemaStringList(t, tt.spec["required"])
			for _, property := range tt.required {
				if !slices.Contains(required, property) {
					t.Fatalf("required = %#v, want %q", required, property)
				}
			}
		})
	}
}

func TestEditToolSchemaRequiresNestedReplacementFields(t *testing.T) {
	fileTool := NewFileTool(t.TempDir())
	params := (&Edit{FileTool: *fileTool}).Spec().Parameters.(map[string]any)
	properties := params["properties"].(map[string]any)
	edits := properties["edits"].(map[string]any)
	items := edits["items"].(map[string]any)
	required := schemaStringList(t, items["required"])
	for _, property := range []string{"old_string", "new_string"} {
		if !slices.Contains(required, property) {
			t.Fatalf("nested required = %#v, want %q", required, property)
		}
	}
}

func schemaStringList(t *testing.T, value any) []string {
	t.Helper()
	if value == nil {
		return nil
	}
	switch value := value.(type) {
	case []string:
		return value
	case []any:
		out := make([]string, 0, len(value))
		for _, item := range value {
			text, ok := item.(string)
			if !ok {
				t.Fatalf("schema list item = %T, want string", item)
			}
			out = append(out, text)
		}
		return out
	default:
		t.Fatalf("schema list = %T, want []string or []any", value)
		return nil
	}
}
