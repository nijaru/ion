package main

import (
	"flag"
	"fmt"
	"strings"
	"time"
)

type cliFlags struct {
	continueFlag      *bool
	continueShortFlag *bool
	resumeFlag        *string
	resumeShortFlag   *string
	providerFlag      *string
	modelFlag         *string
	modelShortFlag    *string
	thinkingFlag      *string
	modeFlag          *string
	yoloFlag          *bool
	printFlag         *bool
	promptFlag        *string
	printShortFlag    *bool
	outputFlag        *string
	jsonFlag          *bool
	timeoutFlag       *time.Duration
	agentFlag         *bool
	exportSessionFlag *string
	importSessionFlag *string
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
		resumeFlag:      flag.String("resume", "", "Resume a specific session by ID"),
		resumeShortFlag: flag.String("r", "", "Resume a specific session by ID"),
		providerFlag:    flag.String("provider", "", "Provider to use"),
		modelFlag:       flag.String("model", "", "Model to use"),
		modelShortFlag:  flag.String("m", "", "Model to use"),
		thinkingFlag: flag.String(
			"thinking",
			"",
			"Thinking effort: auto, off, minimal, low, medium, high, xhigh",
		),
		modeFlag: flag.String("mode", "", "Permission mode: read, edit, or auto"),
		yoloFlag: flag.Bool("yolo", false, "Start in AUTO mode (alias for --mode auto)"),
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
		agentFlag:      flag.Bool("agent", false, "Run as an ACP agent over stdio"),
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

func (f cliFlags) resumeID() string {
	return *f.resumeFlag
}

func (f cliFlags) resumeShortID() string {
	return *f.resumeShortFlag
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

func (f cliFlags) modeOverride() string {
	return *f.modeFlag
}

func (f cliFlags) explicitModeRequested() bool {
	return strings.TrimSpace(*f.modeFlag) != "" || *f.yoloFlag
}

func (f cliFlags) yolo() bool {
	return *f.yoloFlag
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

func (f cliFlags) agentRequested() bool {
	return *f.agentFlag
}

func (f cliFlags) exportSessionPath() string {
	return strings.TrimSpace(*f.exportSessionFlag)
}

func (f cliFlags) importSessionPath() string {
	return strings.TrimSpace(*f.importSessionFlag)
}

func (f cliFlags) sessionBundleRequested() bool {
	return f.exportSessionPath() != "" || f.importSessionPath() != ""
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
		"timeout",
		"agent",
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
		"provider",
		"model",
		"m",
		"thinking",
		"mode",
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
