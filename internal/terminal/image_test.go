package terminal

import (
	"strings"
	"testing"
)

func TestRenderInlineImage(t *testing.T) {
	sampleData := []byte{0x89, 'P', 'N', 'G', '\r', '\n', 0x1a, '\n', 0x00, 0x01, 0x02}

	// Test fallback
	fallback := RenderInlineImageWithProtocol("image/png", sampleData, 40, GraphicsNone)
	if !strings.Contains(fallback, "[Image: image/png (11 bytes)]") {
		t.Fatalf("unexpected fallback string: %q", fallback)
	}

	// Test iTerm2
	iterm := RenderInlineImageWithProtocol("image/png", sampleData, 40, GraphicsITerm2)
	if !strings.HasPrefix(iterm, "\x1b]1337;File=inline=1;width=40:") || !strings.HasSuffix(iterm, "\x07") {
		t.Fatalf("unexpected iTerm2 sequence: %q", iterm)
	}

	// Test Kitty
	kitty := RenderInlineImageWithProtocol("image/png", sampleData, 40, GraphicsKitty)
	if !strings.HasPrefix(kitty, "\x1b_Ga=T,f=100,m=0;") || !strings.HasSuffix(kitty, "\x1b\\") {
		t.Fatalf("unexpected Kitty sequence: %q", kitty)
	}
}
