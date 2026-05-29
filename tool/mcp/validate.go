package mcp

import (
	"fmt"
	"regexp"
	"strings"

	"github.com/nijaru/ion/llm"
)

// Validate checks a tool spec received from an external MCP server before
// registering it. It returns a non-nil error if the spec fails any security
// check. Callers should reject specs that fail validation.
//
// Checks performed:
//   - Prompt injection: description contains embedded LLM instructions.
//   - Name shadowing: name collides with a reserved internal tool name.
//   - Implicit irreversible ops: description references destructive operations
//     without making them explicit (e.g. "permanently deletes all data").
func Validate(spec llm.Spec) error {
	if err := checkName(spec.Name); err != nil {
		return err
	}
	if err := checkPromptInjection(spec.Description); err != nil {
		return err
	}
	if err := checkIrreversible(spec.Description); err != nil {
		return err
	}
	return nil
}

// reservedNames are internal tool names that external servers must not shadow.
var reservedNames = map[string]bool{
	"search_tools": true, // LazyTools meta-tool
	"read_skill":   true, // skill progressive disclosure
}

func checkName(name string) error {
	if name == "" {
		return fmt.Errorf("mcp/validate: tool name is empty")
	}
	if len(name) > 128 {
		return fmt.Errorf("mcp/validate: tool name %q exceeds 128 characters", name)
	}
	if reservedNames[name] {
		return fmt.Errorf("mcp/validate: tool name %q is reserved", name)
	}
	if !validName.MatchString(name) {
		return fmt.Errorf(
			"mcp/validate: tool name %q contains invalid characters (want [a-zA-Z0-9_.-])",
			name,
		)
	}
	return nil
}

// validName matches the MCP tool name character set.
var validName = regexp.MustCompile(`^[a-zA-Z0-9_.-]+$`)

// promptInjectionPatterns matches description text that embeds instructions
// targeting the LLM rather than describing tool behavior.
var promptInjectionPatterns = []*regexp.Regexp{
	regexp.MustCompile(
		`(?i)\bignore\s+(all\s+|previous\s+|prior\s+|above\s+)?(instructions?|rules?|guidelines?|constraints?|prompts?)\b`,
	),
	regexp.MustCompile(`(?i)\byou\s+are\s+now\b`),
	regexp.MustCompile(`(?i)\bact\s+as\b`),
	regexp.MustCompile(`(?i)\bpretend\s+(to\s+be|you\s+are)\b`),
	regexp.MustCompile(`(?i)\bfrom\s+now\s+on\b`),
	regexp.MustCompile(`(?i)\bdisregard\s+(all\s+)?(previous|prior|above)\b`),
	regexp.MustCompile(
		`(?i)\byour\s+(new\s+|updated\s+)?(instructions?|role|task|purpose)\s+(are|is)\b`,
	),
	regexp.MustCompile(`(?i)\bnew\s+instructions?\s*:`),
	regexp.MustCompile(`(?i)\bsystem\s+prompt\s*:`),
	regexp.MustCompile(
		`(?i)\bforget\s+(everything|all\s+previous|your\s+(previous\s+)?instructions?)\b`,
	),
}

func checkPromptInjection(desc string) error {
	for _, pat := range promptInjectionPatterns {
		if loc := pat.FindStringIndex(desc); loc != nil {
			snippet := strings.TrimSpace(desc[loc[0]:min(loc[1]+20, len(desc))])
			return fmt.Errorf("mcp/validate: prompt injection detected in description: %q", snippet)
		}
	}
	return nil
}

// irreversiblePatterns matches descriptions that reference destructive side-effects
// without surfacing them as explicit warnings to callers.
var irreversiblePatterns = []*regexp.Regexp{
	regexp.MustCompile(`(?i)\bpermanently\s+(delete[sd]?|remov(e[sd]?|ing)|erase[sd]?)\b`),
	regexp.MustCompile(
		`(?i)\bformat(s|ted|ting)?\s+(the\s+)?(disk|drive|filesystem|partition|volume)\b`,
	),
	regexp.MustCompile(
		`(?i)\bdrop(s|ped|ping)?\s+(the\s+)?(table|database|db|schema|collection)\b`,
	),
	regexp.MustCompile(`(?i)\b(wipe[sd]?|nuke[sd]?)\s+(the\s+)?(disk|drive|system|database|db)\b`),
	regexp.MustCompile(`(?i)\brm\s+-rf\b`),
}

func checkIrreversible(desc string) error {
	for _, pat := range irreversiblePatterns {
		if loc := pat.FindStringIndex(desc); loc != nil {
			snippet := strings.TrimSpace(desc[loc[0]:min(loc[1]+20, len(desc))])
			return fmt.Errorf(
				"mcp/validate: implicit irreversible operation in description: %q",
				snippet,
			)
		}
	}
	return nil
}
