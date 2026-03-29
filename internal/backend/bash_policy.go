package backend

import (
	"strings"
)

var safePrefixes = []string{
	// Filesystem (read-only, structural)
	"ls",
	"find",
	"tree",
	"file",
	"stat",
	"du",
	"df",
	"wc",
	// Search
	"grep",
	"rg",
	"ag",
	"fd",
	"fzf",
	// Git (read-only subcommands)
	"git status",
	"git log",
	"git diff",
	"git show",
	"git branch",
	"git tag",
	"git remote",
	"git rev-parse",
	"git describe",
	"git ls-files",
	"git blame",
	// Version checks
	"cargo --version",
	"rustc --version",
	"node --version",
	"python --version",
	"go version",
	"uv --version",
	// Build/test (read-only side effects only)
	"cargo check",
	"cargo clippy",
	"cargo test",
	"cargo bench",
	"npm test",
	"pytest",
	"go test",
	"go vet",
	"ruff check",
	// Task tracking
	"tk",
	// System info
	"uname",
	"whoami",
	"hostname",
	"date",
	"which",
	"type",
	// Misc read-only
	"echo",
	"pwd",
	"realpath",
	"dirname",
	"basename",
}

// IsSafeBashCommand checks if a bash command consists only of safe (read-only) operations.
func IsSafeBashCommand(command string) bool {
	// Reject subshells and process substitution
	if strings.Contains(command, "$(") ||
		strings.Contains(command, "`") ||
		strings.Contains(command, "<(") ||
		strings.Contains(command, ">(") {
		return false
	}

	segments := splitCommandChain(command)
	if len(segments) == 0 {
		return false
	}

	for _, segment := range segments {
		trimmed := strings.TrimSpace(segment)
		if trimmed == "" {
			continue
		}

		// Reject output redirections within any segment
		if strings.Contains(trimmed, ">") {
			return false
		}

		// Reject privilege escalation — sudo/doas must never be allowed in Read mode
		if strings.HasPrefix(trimmed, "sudo ") || strings.HasPrefix(trimmed, "doas ") {
			return false
		}

		found := false
		for _, prefix := range safePrefixes {
			if trimmed == prefix ||
				strings.HasPrefix(trimmed, prefix+" ") ||
				strings.HasPrefix(trimmed, prefix+"\t") {
				found = true
				break
			}
		}
		if !found {
			return false
		}
	}

	return true
}

func splitCommandChain(command string) []string {
	var segments []string
	var current strings.Builder
	inQuote := false
	var quoteChar rune

	runes := []rune(command)
	for i := 0; i < len(runes); i++ {
		r := runes[i]
		if (r == '"' || r == '\'') && (i == 0 || runes[i-1] != '\\') {
			if inQuote {
				if r == quoteChar {
					inQuote = false
				}
			} else {
				inQuote = true
				quoteChar = r
			}
		}

		if !inQuote {
			// Check for operators: &&, ||, ;, |
			isOp := false
			opLen := 0
			if r == ';' || r == '|' {
				isOp = true
				opLen = 1
				// Check for ||
				if r == '|' && i+1 < len(runes) && runes[i+1] == '|' {
					opLen = 2
				}
			} else if r == '&' && i+1 < len(runes) && runes[i+1] == '&' {
				isOp = true
				opLen = 2
			}

			if isOp {
				segment := strings.TrimSpace(current.String())
				if segment != "" {
					segments = append(segments, segment)
				}
				current.Reset()
				i += opLen - 1 // Skip extra op chars
				continue
			}
		}
		current.WriteRune(r)
	}

	final := strings.TrimSpace(current.String())
	if final != "" {
		segments = append(segments, final)
	}
	return segments
}
