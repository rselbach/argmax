package spec

import "strings"

// QuoteIfNeeded single-quotes s exactly once when it contains spaces or
// shell metacharacters (FILE-009); already-quoted values are returned
// unchanged.
func QuoteIfNeeded(s string) string {
	if s == "" {
		return "''"
	}
	if len(s) >= 2 {
		first, last := s[0], s[len(s)-1]
		if first == '\'' && last == '\'' || first == '"' && last == '"' {
			return s // already quoted
		}
	}
	if isShellSafe(s) {
		return s
	}
	return "'" + strings.ReplaceAll(s, "'", `'\''`) + "'"
}

// isShellSafe reports whether s consists solely of characters that need no
// quoting in POSIX-like shells.
func isShellSafe(s string) bool {
	for i := 0; i < len(s); i++ {
		c := s[i]
		switch {
		case c >= 'a' && c <= 'z', c >= 'A' && c <= 'Z', c >= '0' && c <= '9':
		case strings.IndexByte("_@%+=:,./^~-", c) >= 0:
		default:
			return false
		}
	}
	return true
}
