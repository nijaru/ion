package main

import (
	"flag"
	"fmt"
	"strings"
	"time"
)

type cliFlags struct {
	continueFlag         *bool
	continueShortFlag    *bool
	sessionFlag          *string
	sessionIDFlag        *string
	noSessionFlag        *bool
	resumeFlag           *string
	resumeShortFlag      *string
	forkFlag             *bool
	providerFlag         *string
	modelFlag            *string
	modelShortFlag       *string
	thinkingFlag         *string
	apiKeyFlag           *string
	sessionDirFlag       *string
	listModelsFlag       *bool
	systemPromptFlag     *string
	appendSystemPromptFlag *string
	printFlag            *bool
	promptFlag           *string
	printShortFlag       *bool
	outputFlag           *string
	jsonFlag             *bool
	timeoutFlag          *time.Duration
	exportSessionFlag    *string
	importSessionFlag    *string
}

func registerCLIFlags() cliFlags {
	return cliFlags{
		continueFlag: flag.Bool(
			"continue",
			false,
			"Continue the most recent session in this directory",
		),
		continueShortFlag: flag.Bool(
			"c",
			false,
			"Continue the most recent session in this directory",
		),
		sessionFlag: flag.String(
			"session",
			"",
			"Use a specific session by ID",
		),
		sessionIDFlag: flag.String(
			"session-id",
			"",
			"Use a specific session by ID (Pi-equivalent alias)",
		),
		noSessionFlag: flag.Bool(
			"no-session",
			false,
			"Run with an ephemeral in-memory session",
		),
		resumeFlag:      flag.String("resume", "", "Resume a specific session by ID"),
		resumeShortFlag: flag.String("r", "", "Resume a specific session by ID"),
		forkFlag: flag.Bool(
			"fork",
			false,
			"Fork the current session into a new branch",
		),
		providerFlag:    flag.String("provider", "", "Provider to use"),
		modelFlag:       flag.String("model", "", "Model to use"),
		modelShortFlag:  flag.String("m", "", "Model to use"),
		thinkingFlag: flag.String(
			"thinking",
			"",
			"Thinking effort: auto, off, minimal, low, medium, high, xhigh",
		),
		apiKeyFlag: flag.String(
			"api-key",
			"",
			"API key for the selected provider (Pi parity)",
		),
		sessionDirFlag: flag.String(
			"session-dir",
			"",
			"Directory path for session storage (Pi parity)",
		),
		listModelsFlag: flag.Bool(
			"list-models",
			false,
			"List available models and exit (Pi parity)",
		),
		systemPromptFlag: flag.String(
			"system-prompt",
			"",
			"Override the system prompt (Pi parity)",
		),
		appendSystemPromptFlag: flag.String(
			"append-system-prompt",
			"",
			"Append to the system prompt (Pi parity)",
		),
		printFlag: flag.Bool(
			"print",
			false,
			"Print response and exit (use with --prompt or stdin)",
		),
		promptFlag:     flag.String("prompt", "", "Prompt to send in print mode"),
		printShortFlag: flag.Bool("p", false, "Print response and exit (alias for --print)"),
		outputFlag:     flag.String("output", "text", "Print mode output: text or json"),
		jsonFlag:       flag.Bool("json", false, "Emit JSON in print mode"),
		timeoutFlag:    flag.Duration("timeout", 5*time.Minute, "Timeout for print mode"),
		exportSessionFlag: flag.String(
			"export-session",
			"",
			"Export the selected session bundle to a JSON file",
		),
		importSessionFlag: flag.String(
			"import-session",
			"",
			"Import a session bundle JSON file",
		),
	}
}

func (f cliFlags) continueRequested() bool {
	return *f.continueFlag || *f.continueShortFlag
}

func (f cliFlags) sessionID() string {
	if sid := strings.TrimSpace(*f.sessionFlag); sid != "" {
		return sid
	}
	return strings.TrimSpace(*f.sessionIDFlag)
}

func (f cliFlags) noSessionRequested() bool {
	return *f.noSessionFlag
}

func (f cliFlags) resumeID() string {
	return *f.resumeFlag
}

func (f cliFlags) resumeShortID() string {
	return *f.resumeShortFlag
}

func (f cliFlags) forkRequested() bool {
	return *f.forkFlag
}

func (f cliFlags) providerOverride() string {
	return strings.TrimSpace(*f.providerFlag)
}

func (f cliFlags) modelOverride() string {
	return firstNonEmpty(*f.modelFlag, *f.modelShortFlag)
}

func (f cliFlags) thinkingOverride() string {
	return *f.thinkingFlag
}

func (f cliFlags) apiKeyOverride() string {
	return strings.TrimSpace(*f.apiKeyFlag)
}

func (f cliFlags) sessionDirOverride() string {
	return strings.TrimSpace(*f.sessionDirFlag)
}

func (f cliFlags) listModelsRequested() bool {
	return *f.listModelsFlag
}

func (f cliFlags) systemPromptOverride() string {
	return strings.TrimSpace(*f.systemPromptFlag)
}

func (f cliFlags) appendSystemPromptOverride() string {
	return strings.TrimSpace(*f.appendSystemPromptFlag)
}

func (f cliFlags) printRequested() bool {
	return *f.printFlag
}

func (f cliFlags) printShortRequested() bool {
	return *f.printShortFlag
}

func (f cliFlags) prompt() string {
	return *f.promptFlag
}

func (f cliFlags) output() string {
	return *f.outputFlag
}

func (f cliFlags) jsonRequested() bool {
	return *f.jsonFlag
}

func (f cliFlags) timeout() time.Duration {
	return *f.timeoutFlag
}

func (f cliFlags) exportSessionPath() string {
	return strings.TrimSpace(*f.exportSessionFlag)
}

func (f cliFlags) importSessionPath() string {
	return strings.TrimSpace(*f.importSessionFlag)
}

func validatePrintSelection(printRequested, openResumePicker bool) error {
	if printRequested && openResumePicker {
		return fmt.Errorf("--resume requires a session ID in print mode")
	}
	return nil
}

func validateSessionBundleSelection(exportPath, importPath string) error {
	if exportPath != "" && importPath != "" {
		return fmt.Errorf("--export-session and --import-session cannot be used together")
	}
	return nil
}

func validateSessionSelection(
	noSession bool,
	sessionID, resumeID, resumeShortID string,
	continueRequested bool,
	openResumePicker bool,
	exportPath, importPath string,
) error {
	if noSession {
		switch {
		case sessionID != "" || resumeID != "" || resumeShortID != "" ||
			continueRequested || openResumePicker:
			return fmt.Errorf("--no-session cannot be combined with session selection")
		case exportPath != "" || importPath != "":
			return fmt.Errorf("--no-session cannot be combined with session import/export")
		}
	}
	if sessionID != "" &&
		(resumeID != "" || resumeShortID != "" || continueRequested || openResumePicker) {
		return fmt.Errorf("--session cannot be combined with other session selection flags")
	}
	return nil
}

func normalizeFlagArgs(args []string) ([]string, bool) {
	hadLeadingSeparator := false
	if len(args) > 1 && args[0] == "--" && strings.HasPrefix(args[1], "-") {
		args = args[1:]
		hadLeadingSeparator = true
	}
	flagArgs := make([]string, 0, len(args))
	positionals := make([]string, 0, len(args))
	openResumePicker := false
	allowFlagLikePositionals := false
	seenPositional := false
	hasFlagLikePositional := false
	hasFlagAfterPositional := false
	hasNonConsumedNonPromptFlag := false
	promptFlagCount := 0
	for i := 0; i < len(args); i++ {
		arg := args[i]
		if arg == "--" {
			positionals = append(positionals, args[i+1:]...)
			break
		}
		name, hasInlineValue, isKnown := ionFlagName(arg)
		if !isKnown {
			if strings.HasPrefix(arg, "-") && arg != "-" && !allowFlagLikePositionals {
				if seenPositional {
					hasFlagAfterPositional = true
				}
				hasNonConsumedNonPromptFlag = true
				flagArgs = append(flagArgs, arg)
				continue
			}
			positionals = append(positionals, arg)
			seenPositional = true
			if strings.HasPrefix(arg, "-") {
				hasFlagLikePositional = true
			}
			continue
		}
		if seenPositional {
			hasFlagAfterPositional = true
		}
		isPromptFlag := name == "print" || name == "p" || name == "json"
		if isPromptFlag {
			allowFlagLikePositionals = true
			promptFlagCount++
		} else if !ionFlagNeedsValue(name) || hasInlineValue {
			hasNonConsumedNonPromptFlag = true
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
	// Add "--" separator before positionals when needed:
	// 1. A flag-like positional (starts with "-") could be confused with a flag
	// 2. A flag appears after a positional in the original args (reordered)
	// 3. Prompt mode ambiguity: multiple prompt flags or non-consumed non-prompt flags
	needsSeparator := hasFlagLikePositional || hasFlagAfterPositional ||
		allowFlagLikePositionals && (promptFlagCount > 1 || hasNonConsumedNonPromptFlag)
	if !hadLeadingSeparator && needsSeparator {
		normalized = append(normalized, "--")
	}
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
		"session",
		"session-id",
		"no-session",
		"resume",
		"r",
		"fork",
		"provider",
		"model",
		"m",
		"thinking",
		"api-key",
		"session-dir",
		"list-models",
		"system-prompt",
		"append-system-prompt",
		"print",
		"prompt",
		"p",
		"output",
		"json",
		"timeout",
		"export-session",
		"import-session":
		return true
	default:
		return false
	}
}

func ionFlagNeedsValue(name string) bool {
	switch name {
	case "resume",
		"r",
		"session",
		"session-id",
		"provider",
		"model",
		"m",
		"thinking",
		"api-key",
		"session-dir",
		"system-prompt",
		"append-system-prompt",
		"prompt",
		"output",
		"timeout",
		"export-session",
		"import-session":
		return true
	default:
		return false
	}
}
