package privacy

import "regexp"

var redactors = []struct {
	pattern     *regexp.Regexp
	replacement string
}{
	{
		pattern:     regexp.MustCompile(`(?i)(authorization\s*:\s*bearer\s+)[A-Za-z0-9._~+/=-]+`),
		replacement: `${1}[redacted-secret]`,
	},
	{
		pattern:     regexp.MustCompile(`(?i)\b((?:api[_-]?key|token|secret|password|passwd|pwd)\s*[:=]\s*["']?)[^"',}\s]+`),
		replacement: `${1}[redacted-secret]`,
	},
	{
		pattern:     regexp.MustCompile(`\b(?:sk|pk)-[A-Za-z0-9_-]{12,}\b`),
		replacement: `[redacted-secret]`,
	},
	{
		pattern:     regexp.MustCompile(`(?i)\b[A-Z0-9._%+\-]+@[A-Z0-9.\-]+\.[A-Z]{2,}\b`),
		replacement: `[redacted-email]`,
	},
	{
		pattern:     regexp.MustCompile(`(?:\+?1[\s.-]?)?(?:\([2-9][0-9]{2}\)|[2-9][0-9]{2})[\s.-]?[0-9]{3}[\s.-]?[0-9]{4}`),
		replacement: `[redacted-phone]`,
	},
}

func Redact(text string) string {
	for _, redactor := range redactors {
		text = redactor.pattern.ReplaceAllString(text, redactor.replacement)
	}
	return text
}
