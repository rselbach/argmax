package spec

// Tokenize splits a command line shell-style (SPEC-002): single/double
// quotes, backslash escapes, embedded spaces, and a trailing empty token
// representing "ready for the next argument" when the line ends with
// whitespace or is empty. Token values are unescaped/unquoted.
//
// Examples: "git che" -> ["git","che"]; "git checkout " ->
// ["git","checkout",""]; `echo "a b" c` -> ["echo","a b","c"];
// `echo a\ b` -> ["echo","a b"]; empty or whitespace-only line -> [""].
func Tokenize(line string) []string {
	tokens, _ := tokenize(line)
	return tokens
}

// tokenize splits line like Tokenize and additionally reports the byte
// offset at which each token starts in the original line, so callers can
// reconstruct the raw line prefix that precedes the final (partial) token.
func tokenize(line string) (tokens []string, starts []int) {
	var cur []byte
	start := -1 // byte offset of the token being accumulated
	lastSep := false
	flush := func() {
		tokens = append(tokens, string(cur))
		starts = append(starts, start)
		cur = cur[:0]
		start = -1
	}
	begin := func(i int) {
		if start < 0 {
			start = i
		}
	}

	n := len(line)
	for i := 0; i < n; {
		c := line[i]
		switch c {
		case ' ', '\t', '\n':
			if start >= 0 {
				flush()
			}
			lastSep = true
			i++
		case '\\':
			begin(i)
			lastSep = false
			if i+1 < n {
				cur = append(cur, line[i+1])
				i += 2
			} else {
				i++ // trailing backslash: dropped
			}
		case '\'':
			begin(i)
			lastSep = false
			i++
			for i < n && line[i] != '\'' {
				cur = append(cur, line[i])
				i++
			}
			if i < n {
				i++ // closing quote
			}
		case '"':
			begin(i)
			lastSep = false
			i++
			for i < n && line[i] != '"' {
				if line[i] == '\\' && i+1 < n &&
					(line[i+1] == '"' || line[i+1] == '\\' || line[i+1] == '$' || line[i+1] == '`') {
					cur = append(cur, line[i+1])
					i += 2
					continue
				}
				cur = append(cur, line[i])
				i++
			}
			if i < n {
				i++ // closing quote
			}
		default:
			begin(i)
			lastSep = false
			cur = append(cur, c)
			i++
		}
	}
	if start >= 0 {
		flush()
	}
	// SPEC-002: trailing empty token for "ready for the next argument" when
	// the line is empty or ends with an unquoted separator.
	if len(tokens) == 0 || lastSep {
		tokens = append(tokens, "")
		starts = append(starts, n)
	}
	return tokens, starts
}
