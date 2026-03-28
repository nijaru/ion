package backend

import (
	"strings"
)

var safePrefixes = []string{
	// Filesystem (read-only)
	"ls",
	"find",
	"tree",
	"file",
	"stat",
	"du",
	"df",
	"wc",
	// Reading
	"cat",
	"head",
	"tail",
	"less",
	"bat",
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
	"printenv",
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
	for _, p1 := range strings.Split(command, "&&") {
		for _, p2 := range strings.Split(p1, "||") {
			for _, p3 := range strings.Split(p2, ";") {
				for _, p4 := range strings.Split(p3, "|") {
					trimmed := strings.TrimSpace(p4)
					if trimmed != "" {
						segments = append(segments, trimmed)
					}
				}
			}
		}
	}
	return segments
}
