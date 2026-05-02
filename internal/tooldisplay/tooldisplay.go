package tooldisplay

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/charmbracelet/x/ansi"
)

type Options struct {
	Workdir string
	Width   int
}

func Title(name, args string, opts Options) string {
	displayName := Name(name)
	args = strings.TrimSpace(args)
	if args == "" || args == "{}" {
		return displayName
	}

	if value, kind, ok := primaryArg(name, args); ok {
		value = formatArg(value, kind, argOptions(displayName, opts))
		return fmt.Sprintf("%s(%s)", displayName, value)
	}

	if !strings.HasPrefix(args, "{") && !strings.HasPrefix(args, "[") {
		return fmt.Sprintf("%s(%s)", displayName, fitArg(args, argWidth(displayName, opts.Width)))
	}
	if strings.Contains(args, "[redacted-secret]") {
		return fmt.Sprintf("%s(%s)", displayName, fitArg(args, argWidth(displayName, opts.Width)))
	}

	return displayName
}

func NormalizeTitle(title string, opts Options) string {
	title = strings.TrimSpace(title)
	if title == "" {
		return ""
	}
	verb := titleVerb(title)
	displayName := Name(verb)
	if displayName == "Tool" || displayName == verb || verb == "" {
		return fitArg(title, opts.Width)
	}
	rest := strings.TrimSpace(title[len(verb):])
	if rest == "" {
		return displayName
	}
	arg := rest
	if strings.HasPrefix(rest, "(") && strings.HasSuffix(rest, ")") {
		arg = strings.TrimSuffix(strings.TrimPrefix(rest, "("), ")")
	}
	arg = formatArg(arg, titleArgKind(verb), argOptions(displayName, opts))
	return fmt.Sprintf("%s(%s)", displayName, arg)
}

func Name(name string) string {
	switch strings.TrimSpace(strings.ToLower(name)) {
	case "read":
		return "Read"
	case "write":
		return "Write"
	case "edit":
		return "Edit"
	case "multi_edit":
		return "Edit"
	case "list":
		return "List"
	case "grep":
		return "Search"
	case "glob":
		return "Find"
	case "bash":
		return "Bash"
	default:
		if strings.TrimSpace(name) == "" {
			return "Tool"
		}
		return name
	}
}

func primaryArg(name, args string) (string, argKind, bool) {
	tool := strings.ToLower(strings.TrimSpace(name))
	switch tool {
	case "bash":
		return jsonStringArg(args, "command", kindText)
	case "read", "write", "edit", "multi_edit":
		return jsonStringArg(args, "file_path", kindPath)
	case "list":
		return jsonStringArg(args, "path", kindPath)
	case "grep":
		if value, kind, ok := jsonStringArg(args, "pattern", kindText); ok {
			return value, kind, true
		}
		return jsonStringArg(args, "path", kindPath)
	case "glob":
		return jsonStringArg(args, "pattern", kindText)
	default:
		for _, candidate := range []struct {
			key  string
			kind argKind
		}{
			{key: "command", kind: kindText},
			{key: "file_path", kind: kindPath},
			{key: "path", kind: kindPath},
			{key: "pattern", kind: kindText},
			{key: "query", kind: kindText},
		} {
			if value, kind, ok := jsonStringArg(args, candidate.key, candidate.kind); ok {
				return value, kind, true
			}
		}
		return "", kindText, false
	}
}

func jsonStringArg(args, key string, kind argKind) (string, argKind, bool) {
	var raw map[string]json.RawMessage
	if err := json.Unmarshal([]byte(args), &raw); err != nil {
		return "", kindText, false
	}
	value, ok := raw[key]
	if !ok {
		return "", kindText, false
	}
	var str string
	if err := json.Unmarshal(value, &str); err == nil && strings.TrimSpace(str) != "" {
		return str, kind, true
	}
	return "", kindText, false
}

type argKind int

const (
	kindText argKind = iota
	kindPath
)

func titleArgKind(verb string) argKind {
	switch strings.ToLower(strings.TrimSpace(verb)) {
	case "read", "write", "edit", "multi_edit", "list":
		return kindPath
	default:
		return kindText
	}
}

func formatArg(value string, kind argKind, opts Options) string {
	value = strings.TrimSpace(value)
	if kind == kindPath {
		return fitPath(displayPath(value, opts.Workdir), opts.Width)
	}
	return fitArg(value, opts.Width)
}

func argOptions(displayName string, opts Options) Options {
	opts.Width = argWidth(displayName, opts.Width)
	return opts
}

func argWidth(displayName string, width int) int {
	if width <= 0 {
		return 0
	}
	return max(1, width-ansi.StringWidth(displayName)-2)
}

func displayPath(value, workdir string) string {
	value = strings.TrimSpace(value)
	if value == "" {
		return value
	}

	cleaned := filepath.Clean(value)
	if !filepath.IsAbs(cleaned) {
		return filepath.ToSlash(cleaned)
	}

	if workdir != "" {
		if rel, ok := relativeTo(workdir, cleaned); ok {
			return filepath.ToSlash(rel)
		}
	}

	if home, err := os.UserHomeDir(); err == nil && home != "" {
		if rel, ok := relativeTo(home, cleaned); ok {
			if rel == "." {
				return "~"
			}
			return "~/" + filepath.ToSlash(rel)
		}
	}

	return filepath.ToSlash(cleaned)
}

func relativeTo(root, target string) (string, bool) {
	absRoot, err := filepath.Abs(root)
	if err != nil {
		return "", false
	}
	absTarget, err := filepath.Abs(target)
	if err != nil {
		return "", false
	}
	rel, err := filepath.Rel(filepath.Clean(absRoot), filepath.Clean(absTarget))
	if err != nil {
		return "", false
	}
	if rel == "." || filepath.IsLocal(rel) {
		return filepath.Clean(rel), true
	}
	return "", false
}

func fitPath(value string, width int) string {
	if width <= 0 || ansi.StringWidth(value) <= width {
		return value
	}
	if width <= 2 {
		return ansi.Truncate(value, width, "…")
	}

	parts := strings.Split(value, "/")
	var suffix string
	for i := len(parts) - 1; i >= 0; i-- {
		candidate := parts[i]
		if suffix != "" {
			candidate += "/" + suffix
		}
		if ansi.StringWidth("…/"+candidate) > width {
			if suffix != "" {
				break
			}
			return ansi.TruncateLeft(value, width, "…")
		}
		suffix = candidate
	}
	if suffix == "" {
		return ansi.TruncateLeft(value, width, "…")
	}
	return "…/" + suffix
}

func fitArg(value string, width int) string {
	if width <= 0 || ansi.StringWidth(value) <= width {
		return value
	}
	return ansi.Truncate(value, width, "…")
}

func titleVerb(title string) string {
	title = strings.TrimSpace(title)
	if title == "" {
		return ""
	}
	if idx := strings.IndexAny(title, " ("); idx >= 0 {
		return strings.TrimSpace(title[:idx])
	}
	return title
}
