package terminal

import (
	"encoding/base64"
	"fmt"
	"os"
	"strings"
)

// GraphicsProtocol represents supported terminal graphics capabilities.
type GraphicsProtocol int

const (
	GraphicsNone GraphicsProtocol = iota
	GraphicsKitty
	GraphicsITerm2
)

// DetectGraphicsProtocol checks environment variables for terminal graphics protocol support.
func DetectGraphicsProtocol() GraphicsProtocol {
	term := os.Getenv("TERM")
	termProg := os.Getenv("TERM_PROGRAM")

	// Kitty or Ghostty (both support Kitty graphics protocol)
	if os.Getenv("KITTY_WINDOW_ID") != "" ||
		os.Getenv("GHOSTTY_RESOURCES_DIR") != "" ||
		strings.Contains(term, "kitty") ||
		strings.Contains(term, "ghostty") {
		return GraphicsKitty
	}

	// iTerm2, WezTerm, VS Code terminal (support iTerm2 protocol)
	if termProg == "iTerm.app" ||
		termProg == "WezTerm" ||
		termProg == "vscode" ||
		os.Getenv("WEZTERM_PANE") != "" {
		return GraphicsITerm2
	}

	return GraphicsNone
}

// RenderInlineImage generates the terminal escape sequence to render an image inline.
// If the terminal does not support graphics, returns a fallback badge string.
func RenderInlineImage(mimeType string, data []byte, maxCells int) string {
	return RenderInlineImageWithProtocol(mimeType, data, maxCells, DetectGraphicsProtocol())
}

// RenderInlineImageWithProtocol generates inline image escapes using an explicit protocol target.
func RenderInlineImageWithProtocol(mimeType string, data []byte, maxCells int, proto GraphicsProtocol) string {
	if len(data) == 0 {
		return ""
	}

	b64Data := base64.StdEncoding.EncodeToString(data)

	switch proto {
	case GraphicsKitty:
		// Kitty graphics protocol transmission with 4096-byte chunking
		var b strings.Builder
		chunkSize := 4096
		for i := 0; i < len(b64Data); i += chunkSize {
			end := i + chunkSize
			m := 1
			if end >= len(b64Data) {
				end = len(b64Data)
				m = 0
			}
			chunk := b64Data[i:end]
			if i == 0 {
				b.WriteString(fmt.Sprintf("\x1b_Ga=T,f=100,m=%d;%s\x1b\\", m, chunk))
			} else {
				b.WriteString(fmt.Sprintf("\x1b_Gm=%d;%s\x1b\\", m, chunk))
			}
		}
		return b.String()

	case GraphicsITerm2:
		widthSpec := ""
		if maxCells > 0 {
			widthSpec = fmt.Sprintf(";width=%d", maxCells)
		}
		return fmt.Sprintf("\x1b]1337;File=inline=1%s:%s\x07", widthSpec, b64Data)

	default:
		return fmt.Sprintf("[Image: %s (%d bytes)]", mimeType, len(data))
	}
}
