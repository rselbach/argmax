use std::collections::BTreeSet;
use std::fmt;
use std::ops::Range;
use std::path::PathBuf;

/// Immutable input snapshot used by completion providers.
#[derive(Clone, Eq, PartialEq)]
pub struct CompletionQuery {
    /// Complete editable shell buffer.
    pub line: String,
    /// Cursor byte offset within `line`.
    pub cursor: usize,
    /// Active shell working directory.
    pub cwd: PathBuf,
    /// Monotonic session generation used to reject stale work.
    pub generation: u64,
}

impl fmt::Debug for CompletionQuery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompletionQuery")
            .field("generation", &self.generation)
            .field("cursor", &self.cursor)
            .field("line_bytes", &self.line.len())
            .field("cwd_bytes", &self.cwd.as_os_str().as_encoded_bytes().len())
            .finish()
    }
}

impl CompletionQuery {
    /// Creates a query after validating the cursor is a UTF-8 boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when the cursor falls outside the line or splits a UTF-8
    /// code point.
    pub fn new(
        line: impl Into<String>,
        cursor: usize,
        cwd: impl Into<PathBuf>,
        generation: u64,
    ) -> Result<Self, String> {
        let line = line.into();
        if !line.is_char_boundary(cursor) {
            return Err(format!("cursor {cursor} is not a valid UTF-8 boundary"));
        }

        Ok(Self {
            line,
            cursor,
            cwd: cwd.into(),
            generation,
        })
    }

    /// Returns the authoritative buffer prefix before the cursor.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.line[..self.cursor]
    }
}

/// Replacement to apply to the authoritative shell buffer.
#[derive(Clone, Eq, PartialEq)]
pub struct TextEdit {
    /// Byte range in the original query line.
    pub range: Range<usize>,
    /// Replacement text for the range.
    pub replacement: String,
}

impl fmt::Debug for TextEdit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TextEdit")
            .field("range", &self.range)
            .field("replacement_bytes", &self.replacement.len())
            .finish()
    }
}

impl TextEdit {
    /// Applies this edit to a line without executing it.
    ///
    /// # Errors
    ///
    /// Returns an error for an inverted, out-of-bounds, or non-UTF-8-boundary
    /// range.
    pub fn apply(&self, line: &str) -> Result<String, String> {
        if self.range.start > self.range.end
            || !line.is_char_boundary(self.range.start)
            || !line.is_char_boundary(self.range.end)
        {
            return Err(format!("invalid edit range {:?}", self.range));
        }

        let mut result = String::with_capacity(
            line.len() - (self.range.end - self.range.start) + self.replacement.len(),
        );
        result.push_str(&line[..self.range.start]);
        result.push_str(&self.replacement);
        result.push_str(&line[self.range.end..]);
        Ok(result)
    }
}

/// Origin of an inert suggestion.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SuggestionSource {
    /// Configured shell alias.
    Alias,
    /// Curated command specification.
    Spec,
    /// Locally inferred command specification.
    SpecInferred,
    /// Executable discovered on `PATH`.
    System,
    /// Filesystem entry.
    File,
    /// Persistent or current-session history.
    History,
    /// Explicitly enabled remote AI provider.
    Ai,
}

impl SuggestionSource {
    /// Stable user-facing source badge.
    #[must_use]
    pub const fn badge(self) -> &'static str {
        match self {
            Self::Alias => "alias",
            Self::Spec => "spec",
            Self::SpecInferred => "inferred",
            Self::System => "system",
            Self::File => "file",
            Self::History => "history",
            Self::Ai => "ai",
        }
    }

    const fn strength(self) -> u8 {
        match self {
            Self::Spec => 7,
            Self::Alias => 6,
            Self::History => 5,
            Self::SpecInferred => 4,
            Self::File => 3,
            Self::System => 2,
            Self::Ai => 1,
        }
    }
}

/// Explicit insertion behavior; renderers do not infer this from display text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InsertionBehavior {
    /// Insert exactly the replacement text.
    Exact,
    /// Append one space because another token is expected.
    AppendSpace,
    /// Append a slash and no space to continue traversal.
    Directory,
}

/// Inert completion candidate produced by a provider.
#[derive(Clone, PartialEq)]
pub struct Suggestion {
    /// Edit to apply to the query buffer.
    pub(crate) edit: TextEdit,
    /// Sanitized command or value displayed in the menu.
    pub(crate) display: String,
    /// Sanitized short description.
    pub(crate) description: String,
    /// Stable icon/type lookup key.
    pub(crate) icon: String,
    /// Strongest provenance used for the primary badge.
    pub(crate) source: SuggestionSource,
    /// All merged provenances for diagnostics.
    pub(crate) sources: BTreeSet<SuggestionSource>,
    /// Static provider priority, clamped during ranking.
    pub(crate) static_priority: f64,
    /// Provider confidence, clamped during ranking.
    pub(crate) confidence: f64,
    /// Explicit insertion behavior.
    pub(crate) insertion: InsertionBehavior,
    /// Stable provider identity used as a deterministic tie-breaker.
    pub(crate) identity: String,
}

impl fmt::Debug for Suggestion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Suggestion")
            .field("edit", &self.edit)
            .field("display_bytes", &self.display.len())
            .field("description_bytes", &self.description.len())
            .field("icon_bytes", &self.icon.len())
            .field("source", &self.source)
            .field("sources", &self.sources)
            .field("static_priority", &self.static_priority)
            .field("confidence", &self.confidence)
            .field("insertion", &self.insertion)
            .field("identity_bytes", &self.identity.len())
            .finish()
    }
}

impl Suggestion {
    /// Builds a sanitized suggestion with one initial provenance.
    #[must_use]
    pub fn new(
        mut edit: TextEdit,
        display: impl AsRef<str>,
        description: impl AsRef<str>,
        icon: impl Into<String>,
        source: SuggestionSource,
        insertion: InsertionBehavior,
        identity: impl Into<String>,
    ) -> Self {
        edit.replacement.retain(|character| !character.is_control());
        Self {
            edit,
            display: sanitize_terminal_text(display.as_ref()),
            description: sanitize_terminal_text(description.as_ref()),
            icon: icon.into(),
            source,
            sources: BTreeSet::from([source]),
            static_priority: 0.5,
            confidence: 0.5,
            insertion,
            identity: identity.into(),
        }
    }

    /// Validated replacement metadata before insertion behavior is resolved.
    #[must_use]
    pub const fn edit(&self) -> &TextEdit {
        &self.edit
    }

    /// Sanitized menu text.
    #[must_use]
    pub fn display(&self) -> &str {
        &self.display
    }

    /// Sanitized short description.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Stable type/icon lookup key.
    #[must_use]
    pub fn icon(&self) -> &str {
        &self.icon
    }

    /// Strongest merged provenance.
    #[must_use]
    pub const fn source(&self) -> SuggestionSource {
        self.source
    }

    /// All merged provenances.
    #[must_use]
    pub const fn sources(&self) -> &BTreeSet<SuggestionSource> {
        &self.sources
    }

    /// Static ranking priority.
    #[must_use]
    pub const fn static_priority(&self) -> f64 {
        self.static_priority
    }

    /// Provider confidence.
    #[must_use]
    pub const fn confidence(&self) -> f64 {
        self.confidence
    }

    /// Explicit insertion behavior.
    #[must_use]
    pub const fn insertion(&self) -> InsertionBehavior {
        self.insertion
    }

    /// Stable deterministic identity.
    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Returns the complete inert line this candidate would produce.
    ///
    /// # Errors
    ///
    /// Returns the edit validation error when its range is invalid for the query.
    pub fn resulting_line(&self, query: &CompletionQuery) -> Result<String, String> {
        self.resolved_edit(&query.line)?.apply(&query.line)
    }

    /// Resolves insertion metadata into the exact edit sent to the shell.
    ///
    /// `AppendSpace` never duplicates whitespace already following the edit, and
    /// `Directory` never duplicates an existing slash. This makes the resulting
    /// line authoritative for deduplication while preserving text after a cursor
    /// in the middle of the buffer.
    ///
    /// # Errors
    ///
    /// Returns an error when the stored edit range is invalid for `line`.
    pub fn resolved_edit(&self, line: &str) -> Result<TextEdit, String> {
        // Validate the range before slicing the suffix.
        self.edit.apply(line)?;
        let suffix = &line[self.edit.range.end..];
        let mut replacement = self.edit.replacement.clone();
        match self.insertion {
            InsertionBehavior::Exact => {}
            InsertionBehavior::AppendSpace => {
                if !replacement.chars().last().is_some_and(char::is_whitespace) && suffix.is_empty()
                {
                    replacement.push(' ');
                }
            }
            InsertionBehavior::Directory => {
                if !replacement.ends_with('/') && !suffix.starts_with('/') {
                    replacement.push('/');
                }
            }
        }
        Ok(TextEdit {
            range: self.edit.range.clone(),
            replacement,
        })
    }

    pub(crate) fn merge_metadata(&mut self, other: Self) {
        self.sources.extend(other.sources);
        if other.source.strength() > self.source.strength() {
            self.source = other.source;
        }
        if other.description.len() > self.description.len() {
            self.description = other.description;
        }
        if self.icon.is_empty() && !other.icon.is_empty() {
            self.icon = other.icon;
        }
        self.static_priority = self.static_priority.max(other.static_priority);
        self.confidence = self.confidence.max(other.confidence);
        if other.identity < self.identity {
            self.identity = other.identity;
        }
    }
}

/// Replaces terminal control data with visible spaces and strips escape bytes.
#[must_use]
pub fn sanitize_terminal_text(value: &str) -> String {
    value
        .chars()
        .filter_map(|ch| match ch {
            '\u{1b}' => None,
            '\n' | '\r' | '\t' => Some(' '),
            ch if ch.is_control() => None,
            ch => Some(ch),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_edit_without_touching_suffix() {
        let edit = TextEdit {
            range: 4..6,
            replacement: "checkout".into(),
        };
        assert_eq!(edit.apply("git ch --help").unwrap(), "git checkout --help");
    }

    #[test]
    fn rejects_edit_that_splits_unicode() {
        let edit = TextEdit {
            range: 1..2,
            replacement: String::new(),
        };
        assert!(edit.apply("é").is_err());
    }

    #[test]
    fn sanitizes_terminal_controls() {
        assert_eq!(
            sanitize_terminal_text("safe\u{1b}[31m\ntext"),
            "safe[31m text"
        );
        let suggestion = Suggestion::new(
            TextEdit {
                range: 0..0,
                replacement: "git\u{1b}[31m\n".into(),
            },
            "git",
            "",
            "command",
            SuggestionSource::Spec,
            InsertionBehavior::Exact,
            "git",
        );
        assert_eq!(suggestion.edit().replacement, "git[31m");
    }

    #[test]
    fn resolves_spacing_and_directory_metadata_into_real_edits() {
        let query = CompletionQuery::new("git com", 7, "/tmp", 1).unwrap();
        let mut suggestion = Suggestion::new(
            TextEdit {
                range: 4..7,
                replacement: "commit".into(),
            },
            "commit",
            "",
            "command",
            SuggestionSource::Spec,
            InsertionBehavior::AppendSpace,
            "spec:git:commit",
        );
        assert_eq!(suggestion.resulting_line(&query).unwrap(), "git commit ");

        suggestion.insertion = InsertionBehavior::Directory;
        suggestion.edit.replacement = "src/components".into();
        assert_eq!(
            suggestion
                .resolved_edit("git com/file")
                .unwrap()
                .replacement,
            "src/components"
        );
    }

    #[test]
    fn append_space_does_not_duplicate_existing_suffix_whitespace() {
        let query = CompletionQuery::new("git com --help", 7, "/tmp", 1).unwrap();
        let suggestion = Suggestion::new(
            TextEdit {
                range: 4..7,
                replacement: "commit".into(),
            },
            "commit",
            "",
            "command",
            SuggestionSource::Spec,
            InsertionBehavior::AppendSpace,
            "spec:git:commit",
        );
        assert_eq!(
            suggestion.resulting_line(&query).unwrap(),
            "git commit --help"
        );
    }

    #[test]
    fn debug_output_redacts_query_edit_and_suggestion_text() {
        let query = CompletionQuery::new(
            "secret-command --token hunter2",
            14,
            "/private/secret-workspace",
            7,
        )
        .unwrap();
        let suggestion = Suggestion::new(
            TextEdit {
                range: 0..14,
                replacement: "classified-replacement".to_owned(),
            },
            "classified-display",
            "classified-description",
            "classified-icon",
            SuggestionSource::Spec,
            InsertionBehavior::Exact,
            "classified-identity",
        );

        for debug in [
            format!("{query:?}"),
            format!("{:?}", suggestion.edit()),
            format!("{suggestion:?}"),
        ] {
            for secret in [
                "secret-command",
                "hunter2",
                "secret-workspace",
                "classified",
            ] {
                assert!(!debug.contains(secret));
            }
        }
    }
}
