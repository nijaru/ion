package main

import (
	"fmt"
	"strings"
)

func validatePrintSelection(printRequested, openResumePicker bool) error {
	if printRequested && openResumePicker {
		return fmt.Errorf("--resume requires a session ID in print mode")
	}
	return nil
}

func normalizeFlagArgs(args []string) ([]string, bool) {
	if len(args) > 1 && args[0] == "--" && strings.HasPrefix(args[1], "-") {
		args = args[1:]
	}
	flagArgs := make([]string, 0, len(args))
	positionals := make([]string, 0, len(args))
	openResumePicker := false
	allowFlagLikePositionals := false
	for i := 0; i < len(args); i++ {
		arg := args[i]
		if arg == "--" {
			positionals = append(positionals, args[i+1:]...)
			break
		}
		name, hasInlineValue, isKnown := ionFlagName(arg)
		if !isKnown {
			if strings.HasPrefix(arg, "-") && arg != "-" && !allowFlagLikePositionals {
				flagArgs = append(flagArgs, arg)
				continue
			}
			positionals = append(positionals, arg)
			continue
		}
		if name == "print" || name == "p" || name == "json" {
			allowFlagLikePositionals = true
		}
		switch {
		case (name == "resume" || name == "r") && !hasInlineValue:
			if i+1 < len(args) && !strings.HasPrefix(args[i+1], "-") {
				flagArgs = append(flagArgs, arg, args[i+1])
				i++
				continue
			}
			openResumePicker = true
		case ionFlagNeedsValue(name) && !hasInlineValue:
			flagArgs = append(flagArgs, arg)
			if i+1 < len(args) {
				flagArgs = append(flagArgs, args[i+1])
				i++
			}
		default:
			flagArgs = append(flagArgs, arg)
		}
	}
	if len(positionals) == 0 {
		return flagArgs, openResumePicker
	}
	normalized := make([]string, 0, len(flagArgs)+1+len(positionals))
	normalized = append(normalized, flagArgs...)
	normalized = append(normalized, "--")
	normalized = append(normalized, positionals...)
	return normalized, openResumePicker
}

func ionFlagName(arg string) (string, bool, bool) {
	if !strings.HasPrefix(arg, "-") || arg == "-" {
		return "", false, false
	}
	name := strings.TrimLeft(arg, "-")
	if name == "" {
		return "", false, false
	}
	if before, _, found := strings.Cut(name, "="); found {
		name = before
		return name, true, ionKnownFlag(name)
	}
	return name, false, ionKnownFlag(name)
}

func ionKnownFlag(name string) bool {
	switch name {
	case "continue",
		"c",
		"resume",
		"r",
		"provider",
		"model",
		"m",
		"thinking",
		"mode",
		"yolo",
		"print",
		"prompt",
		"p",
		"output",
		"json",
		"timeout":
		return true
	default:
		return false
	}
}

func ionFlagNeedsValue(name string) bool {
	switch name {
	case "resume", "r", "provider", "model", "m", "thinking", "mode", "prompt", "output", "timeout":
		return true
	default:
		return false
	}
}
