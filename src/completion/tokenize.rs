use std::ops::Range;

/// Quoting used to form a shell token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuoteKind {
    /// The token contains no quoted fragment.
    Unquoted,
    /// The token consists of a single-quoted fragment.
    Single,
    /// The token consists of a double-quoted fragment.
    Double,
    /// The token combines quoted and unquoted or differently quoted fragments.
    Mixed,
}

/// One token parsed from the authoritative buffer prefix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellToken {
    /// Byte range occupied by the raw token in the original line.
    pub raw: Range<usize>,
    /// Text after removing shell quotes and interpreting shell escapes.
    pub cooked: String,
    /// Quoting used by the token as a whole.
    pub quote: QuoteKind,
    /// Quote still open at the cursor, if any.
    pub open_quote: Option<QuoteKind>,
    /// Whether the token has neither an open quote nor a dangling escape.
    pub complete: bool,
}

impl ShellToken {
    /// Returns this token's bytes from the supplied original line.
    #[must_use]
    pub fn raw_text<'a>(&self, line: &'a str) -> Option<&'a str> {
        line.get(self.raw.clone())
    }

    /// Returns whether this is the synthetic token after trailing whitespace.
    #[must_use]
    pub fn is_empty_at(&self, cursor: usize) -> bool {
        self.raw == (cursor..cursor) && self.cooked.is_empty()
    }
}

/// Shell tokens and separators before a validated cursor position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenizedLine {
    /// Original complete line, including any suffix after the cursor.
    pub line: String,
    /// Cursor byte offset in `line`.
    pub cursor: usize,
    /// Tokens in the prefix. The final item is always the active token.
    pub tokens: Vec<ShellToken>,
    /// Exact byte ranges of unquoted whitespace separators in the prefix.
    pub whitespace: Vec<Range<usize>>,
}

impl TokenizedLine {
    /// Returns the final token being completed.
    ///
    /// # Panics
    ///
    /// Panics only if a caller manually constructs an invalid `TokenizedLine`
    /// with no tokens. Values returned by [`tokenize`] always contain one.
    #[must_use]
    pub fn active_token(&self) -> &ShellToken {
        // `tokenize` always creates an active token, including for an empty line.
        self.tokens
            .last()
            .expect("a tokenized line always has an active token")
    }

    /// Returns tokens committed by whitespace before the active token.
    #[must_use]
    pub fn committed_tokens(&self) -> &[ShellToken] {
        &self.tokens[..self.tokens.len() - 1]
    }

    /// Returns the authoritative prefix before the cursor.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.line[..self.cursor]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParseQuote {
    Single,
    Double,
}

/// Tokenizes a shell buffer through `cursor`, leaving the suffix unparsed.
///
/// This is intentionally a completion tokenizer rather than a shell parser. It
/// recognizes the quoting and escaping rules that affect word boundaries but
/// does not expand variables, substitutions, or globs.
///
/// # Errors
///
/// Returns an error if `cursor` is outside `line` or splits a UTF-8 code point.
pub fn tokenize(line: &str, cursor: usize) -> Result<TokenizedLine, String> {
    if cursor > line.len() || !line.is_char_boundary(cursor) {
        return Err(format!(
            "cursor {cursor} is not a valid UTF-8 boundary for a {}-byte line",
            line.len()
        ));
    }

    let prefix = &line[..cursor];
    let mut tokens = Vec::new();
    let mut whitespace = Vec::new();
    let mut offset = 0;

    while offset < prefix.len() {
        let Some((ch, next)) = next_char(prefix, offset) else {
            break;
        };
        if is_shell_whitespace(ch) {
            let start = offset;
            offset = next;
            while let Some((next_ch, next_offset)) = next_char(prefix, offset) {
                if !is_shell_whitespace(next_ch) {
                    break;
                }
                offset = next_offset;
            }
            whitespace.push(start..offset);
            continue;
        }

        let (token, next_offset) = parse_token(prefix, offset);
        tokens.push(token);
        offset = next_offset;
    }

    if tokens.is_empty()
        || whitespace
            .last()
            .is_some_and(|separator| separator.end == cursor)
    {
        tokens.push(ShellToken {
            raw: cursor..cursor,
            cooked: String::new(),
            quote: QuoteKind::Unquoted,
            open_quote: None,
            complete: true,
        });
    }

    Ok(TokenizedLine {
        line: line.to_string(),
        cursor,
        tokens,
        whitespace,
    })
}

fn parse_token(prefix: &str, start: usize) -> (ShellToken, usize) {
    let mut offset = start;
    let mut cooked = String::new();
    let mut state = None;
    let mut dangling_escape = false;
    let mut saw_unquoted = false;
    let mut saw_single = false;
    let mut saw_double = false;

    while let Some((ch, next)) = next_char(prefix, offset) {
        match state {
            None => match ch {
                ch if is_shell_whitespace(ch) => break,
                '\'' => {
                    saw_single = true;
                    state = Some(ParseQuote::Single);
                    offset = next;
                }
                '"' => {
                    saw_double = true;
                    state = Some(ParseQuote::Double);
                    offset = next;
                }
                '\\' => {
                    saw_unquoted = true;
                    let Some((escaped, after_escaped)) = next_char(prefix, next) else {
                        dangling_escape = true;
                        offset = next;
                        continue;
                    };
                    if escaped != '\n' {
                        cooked.push(escaped);
                    }
                    offset = after_escaped;
                }
                _ => {
                    saw_unquoted = true;
                    cooked.push(ch);
                    offset = next;
                }
            },
            Some(ParseQuote::Single) => {
                if ch == '\'' {
                    state = None;
                } else {
                    cooked.push(ch);
                }
                offset = next;
            }
            Some(ParseQuote::Double) => match ch {
                '"' => {
                    state = None;
                    offset = next;
                }
                '\\' => {
                    let Some((escaped, after_escaped)) = next_char(prefix, next) else {
                        dangling_escape = true;
                        offset = next;
                        continue;
                    };
                    if matches!(escaped, '$' | '`' | '"' | '\\') {
                        cooked.push(escaped);
                    } else if escaped != '\n' {
                        cooked.push('\\');
                        cooked.push(escaped);
                    }
                    offset = after_escaped;
                }
                _ => {
                    cooked.push(ch);
                    offset = next;
                }
            },
        }
    }

    let quote = match (saw_unquoted, saw_single, saw_double) {
        (false, true, false) => QuoteKind::Single,
        (false, false, true) => QuoteKind::Double,
        (false | true, false, false) => QuoteKind::Unquoted,
        _ => QuoteKind::Mixed,
    };
    let open_quote = state.map(|quote| match quote {
        ParseQuote::Single => QuoteKind::Single,
        ParseQuote::Double => QuoteKind::Double,
    });

    (
        ShellToken {
            raw: start..offset,
            cooked,
            quote,
            open_quote,
            complete: open_quote.is_none() && !dangling_escape,
        },
        offset,
    )
}

fn next_char(value: &str, offset: usize) -> Option<(char, usize)> {
    let ch = value.get(offset..)?.chars().next()?;
    Some((ch, offset + ch.len_utf8()))
}

const fn is_shell_whitespace(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_repeated_whitespace_and_a_trailing_empty_token() {
        let line = "git  \t remote   ";
        let parsed = tokenize(line, line.len()).unwrap();

        assert_eq!(
            parsed
                .tokens
                .iter()
                .map(|token| token.cooked.as_str())
                .collect::<Vec<_>>(),
            ["git", "remote", ""]
        );
        assert_eq!(
            parsed
                .whitespace
                .iter()
                .map(|range| &line[range.clone()])
                .collect::<Vec<_>>(),
            ["  \t ", "   "]
        );
        assert!(parsed.active_token().is_empty_at(line.len()));
    }

    #[test]
    fn quoted_commit_message_is_one_cooked_argument() {
        let line = "git commit -m 'Troy joins Greendale'";
        let parsed = tokenize(line, line.len()).unwrap();

        assert_eq!(parsed.tokens.len(), 4);
        assert_eq!(parsed.tokens[3].cooked, "Troy joins Greendale");
        assert_eq!(parsed.tokens[3].quote, QuoteKind::Single);
        assert_eq!(
            parsed.tokens[3].raw_text(line),
            Some("'Troy joins Greendale'")
        );
        assert!(parsed.tokens[3].complete);
    }

    #[test]
    fn decodes_unquoted_and_double_quoted_escapes() {
        let line = r#"cmd Greendale\ College "a\"b\$c\q""#;
        let parsed = tokenize(line, line.len()).unwrap();

        assert_eq!(parsed.tokens[1].cooked, "Greendale College");
        assert_eq!(parsed.tokens[2].cooked, "a\"b$c\\q");
        assert_eq!(parsed.tokens[2].quote, QuoteKind::Double);
    }

    #[test]
    fn reports_incomplete_quotes_and_escapes_without_failing() {
        let single = tokenize("cmd 'Greendale", 14).unwrap();
        assert_eq!(single.active_token().cooked, "Greendale");
        assert_eq!(single.active_token().open_quote, Some(QuoteKind::Single));
        assert!(!single.active_token().complete);

        let escaped = tokenize("cmd Troy\\", 9).unwrap();
        assert_eq!(escaped.active_token().cooked, "Troy");
        assert!(!escaped.active_token().complete);
    }

    #[test]
    fn tokenizes_only_through_a_cursor_in_the_middle() {
        let line = "git com --help";
        let parsed = tokenize(line, 7).unwrap();

        assert_eq!(parsed.prefix(), "git com");
        assert_eq!(parsed.active_token().raw, 4..7);
        assert_eq!(parsed.active_token().cooked, "com");
        assert_eq!(parsed.line, line);
    }

    #[test]
    fn ranges_are_utf8_byte_ranges() {
        let line = "café λ";
        let parsed = tokenize(line, line.len()).unwrap();

        assert_eq!(parsed.tokens[0].raw, 0..5);
        assert_eq!(parsed.tokens[0].cooked, "café");
        assert_eq!(parsed.tokens[1].raw, 6..8);
        assert_eq!(parsed.tokens[1].cooked, "λ");
        assert!(tokenize(line, 4).is_err());
    }

    #[test]
    fn adjacent_quote_styles_form_a_mixed_token() {
        let line = r#"cmd 'Greendale'" Community""#;
        let parsed = tokenize(line, line.len()).unwrap();

        assert_eq!(parsed.tokens[1].cooked, "Greendale Community");
        assert_eq!(parsed.tokens[1].quote, QuoteKind::Mixed);
        assert!(parsed.tokens[1].complete);
    }
}
