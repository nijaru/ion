package privacy

import (
	"strings"
	"testing"
)

func TestRedactSecretsAndPII(t *testing.T) {
	input := `Authorization: Bearer abc.def-123
api_key="sk-test1234567890"
email jane.doe@example.com
phone (415) 555-1212`

	got := Redact(input)
	for _, leaked := range []string{
		"abc.def-123",
		"sk-test1234567890",
		"jane.doe@example.com",
		"(415) 555-1212",
	} {
		if strings.Contains(got, leaked) {
			t.Fatalf("redacted text leaked %q: %s", leaked, got)
		}
	}
	for _, want := range []string{
		"[redacted-secret]",
		"[redacted-email]",
		"[redacted-phone]",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("redacted text missing %q: %s", want, got)
		}
	}
}
