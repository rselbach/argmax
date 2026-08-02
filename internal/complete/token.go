package complete

import "strings"

// Token is one shell-like token of the prompt buffer.
type Token struct {
	// Text is the unquoted token content.
	Text string
	// Start is the byte offset of the token in the original line.
	Start int
}

// Tokenize splits a prompt buffer into completion-oriented tokens. It
// handles single and double quotes, escaped characters, and embedded
// spaces. A buffer ending in unquoted whitespace (or an empty buffer)
// yields a trailing empty token representing "ready for the next
// argument". The tokenizer is deliberately shell-like, not a full POSIX
// parser.
func Tokenize(line string) []Token {
	var (
		tokens  []Token
		current strings.Builder
		start   = -1
		quote   byte
		escaped bool
	)
	for i := 0; i < len(line); i++ {
		ch := line[i]
		switch {
		case escaped:
			current.WriteByte(ch)
			escaped = false
		case ch == '\\' && quote != '\'':
			if start < 0 {
				start = i
			}
			escaped = true
		case quote != 0:
			if ch == quote {
				quote = 0
				continue
			}
			current.WriteByte(ch)
		case ch == '\'' || ch == '"':
			if start < 0 {
				start = i
			}
			quote = ch
		case ch == ' ' || ch == '\t':
			if start >= 0 {
				tokens = append(tokens, Token{Text: current.String(), Start: start})
				current.Reset()
				start = -1
			}
		default:
			if start < 0 {
				start = i
			}
			current.WriteByte(ch)
		}
	}
	if start >= 0 {
		tokens = append(tokens, Token{Text: current.String(), Start: start})
		return tokens
	}
	tokens = append(tokens, Token{Text: "", Start: len(line)})
	return tokens
}

// QuoteArg quotes an argument containing spaces exactly once.
func QuoteArg(s string) string {
	if s == "" || !strings.ContainsAny(s, " \t") {
		return s
	}
	if strings.HasPrefix(s, `"`) && strings.HasSuffix(s, `"`) {
		return s
	}
	return `"` + strings.ReplaceAll(s, `"`, `\"`) + `"`
}
