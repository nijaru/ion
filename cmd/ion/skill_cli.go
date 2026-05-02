package main

import (
	"errors"
	"fmt"
	"io"
	"strings"

	"github.com/nijaru/ion/internal/config"
	ionskills "github.com/nijaru/ion/internal/skills"
)

func runTopLevelCommand(args []string, stdout, stderr io.Writer) (bool, int) {
	if len(args) == 0 || args[0] != "skill" {
		return false, 0
	}
	if err := runSkillCommand(args[1:], stdout); err != nil {
		fmt.Fprintf(stderr, "%v\n", err)
		return true, 1
	}
	return true, 0
}

func runSkillCommand(args []string, stdout io.Writer) error {
	if len(args) == 0 {
		return errors.New(skillCommandUsage())
	}
	switch args[0] {
	case "list", "ls":
		return runSkillList(args[1:], stdout)
	case "install":
		return runSkillInstall(args[1:], stdout)
	default:
		return errors.New(skillCommandUsage())
	}
}

func runSkillList(args []string, stdout io.Writer) error {
	dir, err := config.DefaultSkillsDir()
	if err != nil {
		return fmt.Errorf("resolve skills dir: %w", err)
	}
	out, err := ionskills.Notice([]string{dir}, strings.Join(args, " "))
	if err != nil {
		return fmt.Errorf("load skills: %w", err)
	}
	_, err = fmt.Fprintln(stdout, out)
	return err
}

func runSkillInstall(args []string, stdout io.Writer) error {
	source, confirm, err := parseSkillInstallArgs(args)
	if err != nil {
		return err
	}
	dir, err := config.DefaultSkillsDir()
	if err != nil {
		return fmt.Errorf("resolve skills dir: %w", err)
	}
	if !confirm {
		preview, err := ionskills.PreviewInstall(source, dir)
		if err != nil {
			return err
		}
		printSkillInstallPreview(stdout, preview, false)
		return nil
	}
	installed, err := ionskills.Install(source, dir)
	if err != nil {
		return err
	}
	printSkillInstallPreview(stdout, installed, true)
	return nil
}

func parseSkillInstallArgs(args []string) (string, bool, error) {
	var source string
	var confirm bool
	for _, arg := range args {
		switch arg {
		case "--confirm", "-y":
			confirm = true
		default:
			if strings.HasPrefix(arg, "-") {
				return "", false, fmt.Errorf("unknown skill install flag: %s", arg)
			}
			if source != "" {
				return "", false, fmt.Errorf("usage: ion skill install [--confirm] <path>")
			}
			source = arg
		}
	}
	if source == "" {
		return "", false, fmt.Errorf("usage: ion skill install [--confirm] <path>")
	}
	return source, confirm, nil
}

func printSkillInstallPreview(out io.Writer, preview ionskills.InstallPreview, installed bool) {
	title := "Skill install preview"
	if installed {
		title = "Skill installed"
	}
	fmt.Fprintln(out, title)
	fmt.Fprintf(out, "name: %s\n", preview.Name)
	if preview.Description != "" {
		fmt.Fprintf(out, "description: %s\n", preview.Description)
	}
	if len(preview.AllowedTools) > 0 {
		fmt.Fprintf(out, "allowed tools: %s\n", strings.Join(preview.AllowedTools, ", "))
	}
	fmt.Fprintf(out, "source: %s\n", preview.Source)
	fmt.Fprintf(out, "target: %s\n", preview.Target)
	fmt.Fprintf(out, "files: %d\n", preview.Files)
	if !installed {
		fmt.Fprintf(out, "run: ion skill install --confirm %s\n", preview.Source)
	}
}

func skillCommandUsage() string {
	return strings.Join([]string{
		"usage:",
		"  ion skill list [query]",
		"  ion skill install [--confirm] <path>",
	}, "\n")
}
