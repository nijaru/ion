package app

import (
	"context"
	"os/exec"
	"regexp"
	"strconv"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
)

const gitDiffStatsTimeout = 1500 * time.Millisecond

var (
	gitDiffInsertionsPattern = regexp.MustCompile(`(\d+) insertion`)
	gitDiffDeletionsPattern  = regexp.MustCompile(`(\d+) deletion`)
)

func loadGitDiffStats(workdir string) tea.Cmd {
	workdir = strings.TrimSpace(workdir)
	if workdir == "" {
		return nil
	}
	return func() tea.Msg {
		return gitDiffStatsMsg{
			workdir: workdir,
			stats:   currentGitDiffStats(workdir),
		}
	}
}

func currentGitDiffStats(workdir string) string {
	ctx, cancel := context.WithTimeout(context.Background(), gitDiffStatsTimeout)
	defer cancel()

	out, err := exec.CommandContext(ctx, "git", "-C", workdir, "diff", "--shortstat", "HEAD", "--").
		Output()
	if err != nil {
		return ""
	}
	return parseGitDiffShortstat(string(out))
}

func parseGitDiffShortstat(output string) string {
	insertions := parseGitDiffCount(gitDiffInsertionsPattern, output)
	deletions := parseGitDiffCount(gitDiffDeletionsPattern, output)

	var parts []string
	if insertions > 0 {
		parts = append(parts, "+"+strconv.Itoa(insertions))
	}
	if deletions > 0 {
		parts = append(parts, "-"+strconv.Itoa(deletions))
	}
	return strings.Join(parts, "/")
}

func parseGitDiffCount(pattern *regexp.Regexp, output string) int {
	match := pattern.FindStringSubmatch(output)
	if len(match) != 2 {
		return 0
	}
	value, err := strconv.Atoi(match[1])
	if err != nil {
		return 0
	}
	return value
}
