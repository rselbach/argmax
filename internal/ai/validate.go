package ai

import (
	"fmt"
	"strings"
)

// Sanitize normalizes and validates raw model output against the typed
// buffer. It strips code fences and accidental outer quotes, preserves the
// user's exact prefix and casing, and rejects unchanged, empty,
// non-prefix, multiline, or control-data results.
func Sanitize(raw, buffer string) (string, error) {
	s := strings.TrimSpace(raw)
	s = stripFences(s)
	s = stripOuterQuotes(s)
	s = strings.TrimSpace(s)
	if s == "" {
		return "", fmt.Errorf("empty completion")
	}
	if strings.ContainsAny(s, "\n\r") {
		return "", fmt.Errorf("multiline completion")
	}
	// Never treat model output as terminal control data.
	for _, r := range s {
		if r < 0x20 || r == 0x7f {
			return "", fmt.Errorf("completion contains control characters")
		}
	}
	if !strings.HasPrefix(s, buffer) {
		// Tolerate a case-insensitive prefix by restoring the user's exact
		// typed prefix and casing.
		if len(s) >= len(buffer) && strings.EqualFold(s[:len(buffer)], buffer) {
			s = buffer + s[len(buffer):]
		} else {
			return "", fmt.Errorf("completion does not preserve the typed prefix")
		}
	}
	if s == buffer {
		return "", fmt.Errorf("completion returned the unchanged buffer")
	}
	return s, nil
}

func stripFences(s string) string {
	if !strings.HasPrefix(s, "```") {
		return s
	}
	s = strings.TrimPrefix(s, "```")
	if i := strings.Index(s, "\n"); i >= 0 {
		s = s[i+1:] // drop the language tag line
	}
	s = strings.TrimSuffix(strings.TrimSpace(s), "```")
	return strings.TrimSpace(s)
}

func stripOuterQuotes(s string) string {
	if len(s) >= 2 {
		if (s[0] == '"' && s[len(s)-1] == '"') || (s[0] == '`' && s[len(s)-1] == '`') {
			return s[1 : len(s)-1]
		}
	}
	return s
}
