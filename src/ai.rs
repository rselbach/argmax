//! Inert validation for AI-proposed shell completions.

use std::error::Error;
use std::fmt;

use crate::completion::{
    CompletionQuery, InsertionBehavior, Suggestion, SuggestionSource, TextEdit,
};

/// Maximum decoded assistant text accepted by the validation boundary.
pub const MAX_AI_CANDIDATE_BYTES: usize = 32 * 1024;

/// Why an AI response was rejected before it became an inert suggestion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AiRejection {
    /// Requests require at least three non-whitespace input characters.
    InputTooShort,
    /// AI completion is only valid at the end of the editable buffer.
    CursorNotAtEnd,
    /// The decoded provider response exceeded the fixed memory bound.
    ResponseTooLarge,
    /// The response was empty after removing one recognized wrapper.
    Empty,
    /// The response contained more than one shell line.
    MultipleLines,
    /// The response contained a terminal or other control character.
    ControlCharacter,
    /// The response contained an invisible or directional formatting character.
    InvisibleCharacter,
    /// A code fence or surrounding quote was malformed or unsupported.
    InvalidWrapper,
    /// The response did not preserve the input buffer byte for byte.
    NonPrefix,
    /// The response added nothing to the current input.
    Identical,
}

impl fmt::Display for AiRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InputTooShort => "AI completion requires at least three input characters",
            Self::CursorNotAtEnd => "AI completion requires the cursor at the buffer end",
            Self::ResponseTooLarge => "AI response exceeds the candidate size limit",
            Self::Empty => "AI response is empty",
            Self::MultipleLines => "AI response contains multiple lines",
            Self::ControlCharacter => "AI response contains a control character",
            Self::InvisibleCharacter => "AI response contains an invisible character",
            Self::InvalidWrapper => "AI response has an invalid surrounding wrapper",
            Self::NonPrefix => "AI response does not preserve the exact input prefix",
            Self::Identical => "AI response is identical to the input buffer",
        })
    }
}

impl Error for AiRejection {}

/// Validates one decoded provider response as an exact-prefix shell completion.
///
/// At most one recognized Markdown code fence or matching surrounding quote is
/// removed. Shell syntax inside that wrapper is never parsed or reinterpreted.
///
/// # Errors
///
/// Returns a precise rejection reason for unsafe, empty, identical, multi-line,
/// non-prefix, or oversized content.
pub fn validate_candidate(input: &str, response: &str) -> Result<String, AiRejection> {
    if input.trim().chars().count() < 3 {
        return Err(AiRejection::InputTooShort);
    }
    if response.len() > MAX_AI_CANDIDATE_BYTES {
        return Err(AiRejection::ResponseTooLarge);
    }

    let candidate = strip_recognized_wrapper(response)?;
    if candidate.is_empty() {
        return Err(AiRejection::Empty);
    }
    if candidate.contains(['\n', '\r']) {
        return Err(AiRejection::MultipleLines);
    }
    if candidate.chars().any(char::is_control) {
        return Err(AiRejection::ControlCharacter);
    }
    if candidate.chars().any(is_invisible) {
        return Err(AiRejection::InvisibleCharacter);
    }
    if !candidate.starts_with(input) {
        return Err(AiRejection::NonPrefix);
    }
    if candidate == input {
        return Err(AiRejection::Identical);
    }
    Ok(candidate.to_owned())
}

/// Converts a validated response into an inert suffix insertion.
///
/// # Errors
///
/// Rejects a cursor away from the buffer end or any response rejected by
/// [`validate_candidate`].
pub fn ai_suggestion(query: &CompletionQuery, response: &str) -> Result<Suggestion, AiRejection> {
    if query.cursor != query.line.len() {
        return Err(AiRejection::CursorNotAtEnd);
    }
    let candidate = validate_candidate(&query.line, response)?;
    let suffix = candidate[query.line.len()..].to_owned();
    let identity = candidate_identity(&candidate);
    Ok(Suggestion::new(
        TextEdit {
            range: query.cursor..query.cursor,
            replacement: suffix,
        },
        candidate,
        "AI completion",
        "ai",
        SuggestionSource::Ai,
        InsertionBehavior::Exact,
        identity,
    ))
}

/// Returns whether a character occupies no width or reorders what surrounds it.
///
/// An accepted candidate is inserted into the shell buffer verbatim, so a
/// character the user cannot see is a character the user cannot review. Bidi
/// overrides, zero-width marks, and tag characters all let a response display
/// as one command and execute as another. These are rejected rather than
/// stripped, because silently altering a command is its own hazard.
///
/// The set is Unicode's format category, which `char::is_control` excludes.
fn is_invisible(character: char) -> bool {
    matches!(
        character,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

fn strip_recognized_wrapper(response: &str) -> Result<&str, AiRejection> {
    if response.starts_with("```") {
        return strip_code_fence(response);
    }
    if response.ends_with("```") {
        return Err(AiRejection::InvalidWrapper);
    }

    let Some(first) = response.chars().next() else {
        return Ok(response);
    };
    if matches!(first, '\'' | '"') {
        if response.len() < first.len_utf8() * 2 || !response.ends_with(first) {
            return Err(AiRejection::InvalidWrapper);
        }
        let start = first.len_utf8();
        return Ok(&response[start..response.len() - start]);
    }
    Ok(response)
}

fn strip_code_fence(response: &str) -> Result<&str, AiRejection> {
    let Some((opening, remainder)) = response.split_once('\n') else {
        return Err(AiRejection::InvalidWrapper);
    };
    let language = opening
        .strip_prefix("```")
        .ok_or(AiRejection::InvalidWrapper)?
        .trim();
    if !matches!(language, "" | "sh" | "bash" | "zsh" | "fish" | "shell") {
        return Err(AiRejection::InvalidWrapper);
    }
    let Some((candidate, closing)) = remainder.rsplit_once('\n') else {
        return Err(AiRejection::InvalidWrapper);
    };
    if closing != "```" {
        return Err(AiRejection::InvalidWrapper);
    }
    Ok(candidate)
}

fn candidate_identity(candidate: &str) -> String {
    let hash = candidate
        .as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    format!("ai:{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(line: &str, cursor: usize) -> CompletionQuery {
        CompletionQuery::new(line, cursor, "/tmp/Greendale", 7).unwrap()
    }

    #[test]
    fn accepts_exact_prefix_and_only_inserts_the_missing_suffix() {
        let query = query("git log --author=", 17);
        let suggestion = ai_suggestion(&query, "git log --author='Troy Barnes'").unwrap();

        assert_eq!(suggestion.display(), "git log --author='Troy Barnes'");
        assert_eq!(suggestion.edit().range, 17..17);
        assert_eq!(suggestion.edit().replacement, "'Troy Barnes'");
        assert_eq!(
            suggestion.resulting_line(&query).unwrap(),
            suggestion.display()
        );
        assert_eq!(suggestion.source(), SuggestionSource::Ai);
    }

    #[test]
    fn removes_only_recognized_fences_and_quotes() {
        let cases = [
            "```sh\ngit status --short\n```",
            "```\ngit status --short\n```",
            "'git status --short'",
            "\"git status --short\"",
        ];
        for response in cases {
            assert_eq!(
                validate_candidate("git status", response).unwrap(),
                "git status --short"
            );
        }
    }

    #[test]
    fn rejects_candidates_carrying_invisible_characters() {
        let cases = [
            "git status --short\u{200B}",
            "git status --short\u{00AD}",
            "git status --short\u{202E}",
            "git status --short\u{2066}",
            "git status --short\u{FEFF}",
            "git status --short\u{E0041}",
            "git status --sh\u{200D}ort",
        ];
        for response in cases {
            assert_eq!(
                validate_candidate("git status", response),
                Err(AiRejection::InvisibleCharacter),
                "accepted {response:?}"
            );
        }

        assert_eq!(
            validate_candidate("git commit", "git commit -m 'café Grüße 日本語'").unwrap(),
            "git commit -m 'café Grüße 日本語'"
        );
    }

    #[test]
    fn rejects_multi_line_control_and_unsupported_wrappers() {
        let cases = [
            ("git status\ngit push", AiRejection::MultipleLines),
            ("git status\n", AiRejection::MultipleLines),
            ("git status\t", AiRejection::ControlCharacter),
            ("git status\u{1b}[31m", AiRejection::ControlCharacter),
            (
                "```python\ngit status --short\n```",
                AiRejection::InvalidWrapper,
            ),
            ("```sh git status --short```", AiRejection::InvalidWrapper),
            ("'git status --short", AiRejection::InvalidWrapper),
        ];
        for (response, want) in cases {
            assert_eq!(validate_candidate("git status", response), Err(want));
        }
    }

    #[test]
    fn rejects_empty_identical_nonprefix_short_and_oversized_values() {
        assert_eq!(validate_candidate("git", ""), Err(AiRejection::Empty));
        assert_eq!(
            validate_candidate("git status", "git status"),
            Err(AiRejection::Identical)
        );
        assert_eq!(
            validate_candidate("Git", "git status"),
            Err(AiRejection::NonPrefix)
        );
        assert_eq!(
            validate_candidate(" g ", " g status"),
            Err(AiRejection::InputTooShort)
        );
        let oversized = "x".repeat(MAX_AI_CANDIDATE_BYTES + 1);
        assert_eq!(
            validate_candidate("xxx", &oversized),
            Err(AiRejection::ResponseTooLarge)
        );
    }

    #[test]
    fn rejects_cursor_away_from_buffer_end() {
        assert_eq!(
            ai_suggestion(&query("git status --short", 3), "git log"),
            Err(AiRejection::CursorNotAtEnd)
        );
    }

    #[test]
    fn candidate_identity_is_stable_without_exposing_the_command() {
        let query = query("git", 3);
        let first = ai_suggestion(&query, "git status").unwrap();
        let second = ai_suggestion(&query, "git status").unwrap();
        assert_eq!(first.identity(), second.identity());
        assert!(!first.identity().contains("status"));
    }
}
