//! Shell history parsing, session merging, and explicit-tier searching.

mod cache;

use std::cmp::Ordering;
use std::collections::HashSet;

use crate::completion::{
    CommandSpec, OptionSpec, QuoteKind, ShellToken, SpecIndex, TokenizedLine, tokenize,
};

pub use cache::{
    DEFAULT_MAX_HISTORY_BYTES, DEFAULT_MAX_SESSION_ENTRIES, HistoryCache, HistoryFileKey,
};

/// A command read from persistent or current-session shell history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryEntry {
    /// Exact command text to restore to the shell buffer.
    pub command: String,
    /// Unix timestamp supplied by the shell, when present.
    pub timestamp: Option<u64>,
    /// Zsh's elapsed execution time in seconds, when present.
    pub duration: Option<u64>,
}

impl HistoryEntry {
    /// Creates an entry without shell-provided timing metadata.
    #[must_use]
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            timestamp: None,
            duration: None,
        }
    }

    /// Adds a shell-provided Unix timestamp.
    #[must_use]
    pub const fn with_timestamp(mut self, timestamp: u64) -> Self {
        self.timestamp = Some(timestamp);
        self
    }
}

/// Persistent history syntax used by a supported shell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryFormat {
    /// Zsh plain history and `EXTENDED_HISTORY` records.
    Zsh,
    /// Bash line history with optional `#<epoch>` marker lines.
    Bash,
    /// Fish's YAML-like history records.
    Fish,
}

/// Explicit match tiers used by history mode, from strongest to weakest.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum HistoryTier {
    /// The complete query and command are equal, ignoring case.
    Exact,
    /// The complete command starts with the query, ignoring case.
    Prefix,
    /// The complete command contains the query, ignoring case.
    Contains,
    /// Query characters occur in order but need not be adjacent.
    Fuzzy,
}

/// A history entry paired with its inspectable match quality.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryMatch {
    /// Matching command and shell metadata.
    pub entry: HistoryEntry,
    /// Strongest tier satisfied by this entry.
    pub tier: HistoryTier,
    /// Relative subsequence quality. This is only used to order fuzzy matches.
    pub fuzzy_score: usize,
}

/// Parses a shell history file in file order, normally oldest to newest.
///
/// Empty records and incomplete metadata-only tails are ignored. Malformed
/// metadata is isolated to its record and never prevents later commands from
/// being returned. Bytes that are not valid UTF-8 after any format-specific
/// unescaping are replaced lossily.
#[must_use]
pub fn parse_history(format: HistoryFormat, contents: &[u8]) -> Vec<HistoryEntry> {
    match format {
        HistoryFormat::Zsh => parse_zsh(&String::from_utf8_lossy(&unmetafy(contents))),
        HistoryFormat::Bash => parse_bash(&String::from_utf8_lossy(contents)),
        HistoryFormat::Fish => parse_fish(&String::from_utf8_lossy(contents)),
    }
}

/// Reverses the escaping zsh applies when writing its history file.
///
/// zsh stores a byte it treats as special by writing the marker `0x83` followed
/// by that byte with bit `0x20` flipped. Ordinary UTF-8 text contains such
/// bytes, so a history file holding accented or non-Latin commands decodes to
/// replacement characters unless the escape is reversed before decoding.
fn unmetafy(contents: &[u8]) -> Vec<u8> {
    const MARKER: u8 = 0x83;
    const FLIPPED_BIT: u8 = 0x20;

    if !contents.contains(&MARKER) {
        return contents.to_vec();
    }
    let mut decoded = Vec::with_capacity(contents.len());
    let mut bytes = contents.iter().copied();
    while let Some(byte) = bytes.next() {
        if byte != MARKER {
            decoded.push(byte);
            continue;
        }
        // A trailing marker is a truncated escape, which the bounded tail read
        // can produce. Keeping it preserves the surrounding record.
        match bytes.next() {
            Some(escaped) => decoded.push(escaped ^ FLIPPED_BIT),
            None => decoded.push(MARKER),
        }
    }
    decoded
}

/// Merges file and session entries into newest-first search order.
///
/// Each input is expected in execution order (oldest to newest). Session
/// commands are considered newer than every persistent command because the
/// shell may not have flushed them yet. Exact command duplicates retain the
/// metadata from their newest occurrence.
#[must_use]
pub fn merge_history(persistent: &[HistoryEntry], session: &[HistoryEntry]) -> Vec<HistoryEntry> {
    let mut seen = HashSet::new();
    let mut merged = Vec::with_capacity(persistent.len() + session.len());

    for entry in session.iter().rev().chain(persistent.iter().rev()) {
        if entry.command.trim().is_empty() || !seen.insert(entry.command.as_str()) {
            continue;
        }
        merged.push(entry.clone());
    }

    merged
}

/// Searches newest-first history using explicit exact, prefix, contains, and
/// fuzzy tiers.
///
/// `aliases` contains `(alias, canonical)` pairs. Alias expansion is used only
/// for lookup; the returned command always preserves the form that was
/// originally recorded. Empty queries return at most the 100 newest unique
/// commands. Non-empty results have the same cap.
#[must_use]
pub fn search_history(
    entries: &[HistoryEntry],
    query: &str,
    aliases: &[(&str, &str)],
) -> Vec<HistoryMatch> {
    const LIMIT: usize = 100;

    let query = query.trim_start();
    let entries = unique_newest(entries);
    if query.is_empty() {
        return entries
            .into_iter()
            .take(LIMIT)
            .map(|(_, entry)| HistoryMatch {
                entry: entry.clone(),
                tier: HistoryTier::Prefix,
                fuzzy_score: 0,
            })
            .collect();
    }

    let query_alternatives = command_alternatives(query, aliases);
    let use_subcommand_filter = query_alternatives
        .iter()
        .any(|alternative| command_parts(alternative).1.is_some());

    let mut matched = if use_subcommand_filter {
        collect_matches(&entries, &query_alternatives, aliases, true)
    } else {
        Vec::new()
    };
    if matched.is_empty() {
        matched = collect_matches(&entries, &query_alternatives, aliases, false);
    }

    matched.sort_by(compare_matches);
    matched
        .into_iter()
        .take(LIMIT)
        .map(|ranked| HistoryMatch {
            entry: ranked.entry,
            tier: ranked.quality.tier,
            fuzzy_score: ranked.quality.fuzzy_score,
        })
        .collect()
}

/// Searches history with shell-aware, specification-backed command structure.
///
/// This preserves [`search_history`] matching tiers and alias lookup behavior,
/// but uses the supplied specifications to distinguish subcommands from option
/// values. Unknown roots, unknown options, and `--` before a subcommand disable
/// structural filtering conservatively. When no structured candidate matches,
/// the ordinary broad line and fuzzy search remains available.
#[must_use]
pub fn search_history_with_specs(
    entries: &[HistoryEntry],
    query: &str,
    aliases: &[(&str, &str)],
    specs: &SpecIndex,
) -> Vec<HistoryMatch> {
    const LIMIT: usize = 100;

    let query = query.trim_start();
    let entries = unique_newest(entries);
    if query.is_empty() {
        return entries
            .into_iter()
            .take(LIMIT)
            .map(|(_, entry)| HistoryMatch {
                entry: entry.clone(),
                tier: HistoryTier::Prefix,
                fuzzy_score: 0,
            })
            .collect();
    }

    let query_alternatives = tokenized_command_alternatives(query, aliases);
    let structured_queries = query_alternatives
        .iter()
        .filter_map(|alternative| spec_query_intent(alternative, specs))
        .collect::<Vec<_>>();
    let mut matched = if structured_queries.is_empty() {
        Vec::new()
    } else {
        collect_spec_matches(&entries, &structured_queries, aliases, specs)
    };
    if matched.is_empty() {
        matched = collect_tokenized_line_matches(&entries, &query_alternatives, aliases);
    }

    matched.sort_by(compare_matches);
    matched
        .into_iter()
        .take(LIMIT)
        .map(|ranked| HistoryMatch {
            entry: ranked.entry,
            tier: ranked.quality.tier,
            fuzzy_score: ranked.quality.fuzzy_score,
        })
        .collect()
}

fn parse_zsh(contents: &str) -> Vec<HistoryEntry> {
    let mut entries = Vec::new();
    let mut lines = contents.lines().peekable();

    while let Some(raw_line) = lines.next() {
        let mut line = raw_line.trim_end_matches('\r').to_owned();
        while has_unescaped_trailing_backslash(&line) {
            let Some(continuation) = lines.next() else {
                break;
            };
            // zsh stores a newline inside a command as a backslash followed by
            // a real newline, and drops the backslash when reading the file
            // back. Keeping it would change the command's meaning, since a
            // backslash inside single quotes is literal.
            line.pop();
            line.push('\n');
            line.push_str(continuation.trim_end_matches('\r'));
        }

        if line.trim().is_empty() {
            continue;
        }

        if let Some(record) = parse_zsh_extended(&line) {
            entries.push(record);
        } else if !looks_like_zsh_metadata(&line) {
            entries.push(HistoryEntry::new(line));
        }
    }

    entries
}

fn parse_zsh_extended(line: &str) -> Option<HistoryEntry> {
    let rest = line.strip_prefix(": ")?;
    let (metadata, command) = rest.split_once(';')?;
    let (timestamp, duration) = metadata.split_once(':')?;
    let timestamp = timestamp.parse().ok()?;
    let duration = duration.parse().ok()?;
    if command.trim().is_empty() {
        return None;
    }

    Some(HistoryEntry {
        command: command.to_owned(),
        timestamp: Some(timestamp),
        duration: Some(duration),
    })
}

fn looks_like_zsh_metadata(line: &str) -> bool {
    line.strip_prefix(": ").is_some_and(|rest| {
        rest.chars()
            .next()
            .is_some_and(|first| first.is_ascii_digit())
            && (rest.bytes().all(|byte| byte.is_ascii_digit())
                || rest.contains(':')
                || rest.contains(';'))
    })
}

fn has_unescaped_trailing_backslash(line: &str) -> bool {
    line.as_bytes()
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count()
        % 2
        == 1
}

fn parse_bash(contents: &str) -> Vec<HistoryEntry> {
    let mut entries = Vec::new();
    let mut pending_timestamp = None;

    for raw_line in contents.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        if let Some(timestamp) = bash_timestamp(line) {
            pending_timestamp = Some(timestamp);
            continue;
        }

        entries.push(HistoryEntry {
            command: line.to_owned(),
            timestamp: pending_timestamp.take(),
            duration: None,
        });
    }

    entries
}

fn bash_timestamp(line: &str) -> Option<u64> {
    let digits = line.strip_prefix('#')?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn parse_fish(contents: &str) -> Vec<HistoryEntry> {
    let mut entries = Vec::new();
    let mut current: Option<HistoryEntry> = None;

    for raw_line in contents.lines() {
        let line = raw_line.trim_end_matches('\r');
        let trimmed_start = line.trim_start();
        if let Some(encoded) = trimmed_start.strip_prefix("- cmd:") {
            push_nonempty(&mut entries, current.take());
            current = Some(HistoryEntry {
                command: decode_fish_command(encoded.strip_prefix(' ').unwrap_or(encoded)),
                timestamp: None,
                duration: None,
            });
            continue;
        }

        let Some(entry) = current.as_mut() else {
            continue;
        };
        if let Some(value) = trimmed_start.strip_prefix("when:") {
            entry.timestamp = value.trim().parse().ok();
        }
    }
    push_nonempty(&mut entries, current);
    entries
}

fn push_nonempty(entries: &mut Vec<HistoryEntry>, entry: Option<HistoryEntry>) {
    let Some(entry) = entry else {
        return;
    };
    if entry.command.trim().is_empty() {
        return;
    }
    entries.push(entry);
}

fn decode_fish_command(encoded: &str) -> String {
    let mut decoded = String::with_capacity(encoded.len());
    let mut chars = encoded.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }

        match chars.next() {
            Some('n') => decoded.push('\n'),
            Some('r') => decoded.push('\r'),
            Some('t') => decoded.push('\t'),
            Some('\\') | None => decoded.push('\\'),
            Some(escaped) => {
                decoded.push('\\');
                decoded.push(escaped);
            }
        }
    }
    decoded
}

/// Borrows the newest occurrence of each distinct command.
///
/// Entries are borrowed rather than cloned: this runs on every keystroke over
/// the whole history, while only the bounded set of ranked matches is ever
/// returned to the caller.
fn unique_newest(entries: &[HistoryEntry]) -> Vec<(usize, &HistoryEntry)> {
    let mut seen = HashSet::new();
    entries
        .iter()
        .enumerate()
        .filter_map(|(recency, entry)| {
            if entry.command.trim().is_empty() || !seen.insert(entry.command.as_str()) {
                return None;
            }
            Some((recency, entry))
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MatchQuality {
    tier: HistoryTier,
    fuzzy_score: usize,
}

#[derive(Debug)]
struct RankedMatch {
    entry: HistoryEntry,
    quality: MatchQuality,
    recency: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SpecQueryIntent {
    line: String,
    root: String,
    subcommands: Vec<String>,
    needle: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SpecCommandParts {
    root: String,
    subcommand: String,
}

struct SpecTraversal<'a> {
    root: &'a CommandSpec,
    node: &'a CommandSpec,
    inherited_options: Vec<&'a OptionSpec>,
    first_subcommand: Option<String>,
    awaiting_option_value: bool,
    subcommands_allowed: bool,
    options_ended: bool,
}

fn collect_spec_matches(
    entries: &[(usize, &HistoryEntry)],
    queries: &[SpecQueryIntent],
    aliases: &[(&str, &str)],
    specs: &SpecIndex,
) -> Vec<RankedMatch> {
    entries
        .iter()
        .filter_map(|(recency, entry)| {
            let candidates = tokenized_command_alternatives(&entry.command, aliases);
            let quality = candidates
                .iter()
                .filter_map(|candidate| {
                    let parts = spec_candidate_parts(candidate, specs)?;
                    queries
                        .iter()
                        .filter_map(|query| spec_structured_quality(query, candidate, &parts))
                        .min_by(compare_quality)
                })
                .min_by(compare_quality)?;
            Some(RankedMatch {
                entry: (*entry).clone(),
                quality,
                recency: *recency,
            })
        })
        .collect()
}

fn collect_tokenized_line_matches(
    entries: &[(usize, &HistoryEntry)],
    query_alternatives: &[String],
    aliases: &[(&str, &str)],
) -> Vec<RankedMatch> {
    entries
        .iter()
        .filter_map(|(recency, entry)| {
            let candidates = tokenized_command_alternatives(&entry.command, aliases);
            let quality = candidates
                .iter()
                .flat_map(|candidate| {
                    query_alternatives
                        .iter()
                        .filter_map(move |query| line_quality(query, candidate))
                })
                .min_by(compare_quality)?;
            Some(RankedMatch {
                entry: (*entry).clone(),
                quality,
                recency: *recency,
            })
        })
        .collect()
}

fn spec_structured_quality(
    query: &SpecQueryIntent,
    candidate: &str,
    parts: &SpecCommandParts,
) -> Option<MatchQuality> {
    if !query.root.eq_ignore_ascii_case(&parts.root)
        || !query
            .subcommands
            .iter()
            .any(|subcommand| subcommand.eq_ignore_ascii_case(&parts.subcommand))
    {
        return None;
    }

    if let Some(quality) = line_quality(&query.line, candidate) {
        if quality.tier != HistoryTier::Fuzzy {
            return Some(quality);
        }
    }
    let quality = line_quality(&query.needle, &parts.subcommand)?;
    if quality.tier == HistoryTier::Exact {
        return Some(MatchQuality {
            tier: HistoryTier::Prefix,
            fuzzy_score: 0,
        });
    }
    Some(quality)
}

fn spec_query_intent(command: &str, specs: &SpecIndex) -> Option<SpecQueryIntent> {
    let parsed = tokenize(command, command.len()).ok()?;
    let traversal = traverse_spec_tokens(specs, parsed.committed_tokens())?;
    let root = traversal.root.name.clone();

    if let Some(subcommand) = traversal.first_subcommand {
        return Some(SpecQueryIntent {
            line: command.to_owned(),
            root,
            subcommands: vec![subcommand.clone()],
            needle: subcommand,
        });
    }
    if traversal.awaiting_option_value || traversal.options_ended || !traversal.subcommands_allowed
    {
        return None;
    }

    let active = parsed.active_token();
    if active.cooked.is_empty() || !active.complete || active.cooked.starts_with('-') {
        return None;
    }

    let folded = active.cooked.to_lowercase();
    let mut subcommands = Vec::new();
    let mut exact = None;
    for child in &traversal.node.subcommands {
        let names =
            std::iter::once(child.name.as_str()).chain(child.aliases.iter().map(String::as_str));
        let mut matched = false;
        for name in names {
            if name.eq_ignore_ascii_case(&active.cooked) {
                exact = Some(child.name.clone());
                matched = true;
                break;
            }
            if name.to_lowercase().starts_with(&folded) {
                matched = true;
            }
        }
        if matched {
            subcommands.push(child.name.clone());
        }
    }
    if subcommands.is_empty() {
        return None;
    }

    Some(SpecQueryIntent {
        line: command.to_owned(),
        root,
        subcommands,
        needle: exact.unwrap_or_else(|| active.cooked.clone()),
    })
}

fn spec_candidate_parts(command: &str, specs: &SpecIndex) -> Option<SpecCommandParts> {
    let mut committed_line = command.to_owned();
    if !committed_line
        .chars()
        .next_back()
        .is_some_and(is_history_whitespace)
    {
        committed_line.push(' ');
    }
    let parsed = tokenize(&committed_line, committed_line.len()).ok()?;
    let traversal = traverse_spec_tokens(specs, parsed.committed_tokens())?;
    Some(SpecCommandParts {
        root: traversal.root.name.clone(),
        subcommand: traversal.first_subcommand?,
    })
}

fn traverse_spec_tokens<'a>(
    specs: &'a SpecIndex,
    tokens: &[ShellToken],
) -> Option<SpecTraversal<'a>> {
    let root_token = tokens.first()?;
    if !root_token.complete {
        return None;
    }
    let root = specs.roots().iter().find(|candidate| {
        candidate.name.eq_ignore_ascii_case(&root_token.cooked)
            || candidate
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(&root_token.cooked))
    })?;
    let mut traversal = SpecTraversal {
        root,
        node: root,
        inherited_options: Vec::new(),
        first_subcommand: None,
        awaiting_option_value: false,
        subcommands_allowed: true,
        options_ended: false,
    };

    for token in &tokens[1..] {
        if !token.complete {
            return None;
        }
        if traversal.awaiting_option_value {
            traversal.awaiting_option_value = false;
            continue;
        }
        if !traversal.options_ended {
            if token.cooked == "--" {
                traversal.options_ended = true;
                traversal.subcommands_allowed = false;
                continue;
            }
            if token.cooked.starts_with('-') {
                let Some(awaiting_value) = history_option_usage(
                    traversal.node,
                    &traversal.inherited_options,
                    &token.cooked,
                ) else {
                    traversal.subcommands_allowed = false;
                    continue;
                };
                traversal.awaiting_option_value = awaiting_value;
                continue;
            }
        }

        if traversal.subcommands_allowed {
            if let Some(child) = find_history_subcommand(traversal.node, &token.cooked) {
                if traversal.first_subcommand.is_none() {
                    traversal.first_subcommand = Some(child.name.clone());
                }
                traversal
                    .inherited_options
                    .extend(traversal.node.options.iter().filter(|option| option.global));
                traversal.node = child;
                continue;
            }
            traversal.subcommands_allowed = false;
        }
    }

    Some(traversal)
}

fn find_history_subcommand<'a>(command: &'a CommandSpec, name: &str) -> Option<&'a CommandSpec> {
    command.subcommands.iter().find(|child| {
        child.name.eq_ignore_ascii_case(name)
            || child
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
    })
}

fn history_option_usage(
    command: &CommandSpec,
    inherited: &[&OptionSpec],
    token: &str,
) -> Option<bool> {
    let (option_name, attached_with_equals) = token
        .split_once('=')
        .map_or((token, false), |(name, _)| (name, true));
    for option in command.options.iter().chain(inherited.iter().copied()) {
        if option.names().any(|name| name == option_name) {
            if attached_with_equals && !option.takes_value {
                return None;
            }
            return Some(option.takes_value && !attached_with_equals);
        }
    }

    command
        .options
        .iter()
        .chain(inherited.iter().copied())
        .filter(|option| option.takes_value)
        .flat_map(OptionSpec::names)
        .filter(|name| name.starts_with('-') && !name.starts_with("--"))
        .filter(|name| token.len() > name.len() && token.starts_with(name))
        .max_by_key(|name| name.len())
        .map(|_| false)
}

const fn is_history_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n')
}

fn collect_matches(
    entries: &[(usize, &HistoryEntry)],
    query_alternatives: &[String],
    aliases: &[(&str, &str)],
    structured: bool,
) -> Vec<RankedMatch> {
    entries
        .iter()
        .filter_map(|(recency, entry)| {
            let candidates = command_alternatives(&entry.command, aliases);
            let quality = candidates
                .iter()
                .flat_map(|candidate| {
                    query_alternatives.iter().filter_map(move |query| {
                        if structured {
                            structured_quality(query, candidate)
                        } else {
                            line_quality(query, candidate)
                        }
                    })
                })
                .min_by(compare_quality)?;
            Some(RankedMatch {
                entry: (*entry).clone(),
                quality,
                recency: *recency,
            })
        })
        .collect()
}

fn compare_matches(left: &RankedMatch, right: &RankedMatch) -> Ordering {
    left.quality.tier.cmp(&right.quality.tier).then_with(|| {
        if left.quality.tier == HistoryTier::Fuzzy {
            right
                .quality
                .fuzzy_score
                .cmp(&left.quality.fuzzy_score)
                .then_with(|| left.recency.cmp(&right.recency))
        } else {
            left.recency.cmp(&right.recency)
        }
    })
}

fn compare_quality(left: &MatchQuality, right: &MatchQuality) -> Ordering {
    left.tier
        .cmp(&right.tier)
        .then_with(|| right.fuzzy_score.cmp(&left.fuzzy_score))
}

fn line_quality(query: &str, candidate: &str) -> Option<MatchQuality> {
    let query = query.to_lowercase();
    let candidate = candidate.to_lowercase();
    if candidate == query {
        return Some(MatchQuality {
            tier: HistoryTier::Exact,
            fuzzy_score: usize::MAX,
        });
    }
    if candidate.starts_with(&query) {
        return Some(MatchQuality {
            tier: HistoryTier::Prefix,
            fuzzy_score: 0,
        });
    }
    if candidate.contains(&query) {
        return Some(MatchQuality {
            tier: HistoryTier::Contains,
            fuzzy_score: 0,
        });
    }

    fuzzy_subsequence_score(&query, &candidate).map(|fuzzy_score| MatchQuality {
        tier: HistoryTier::Fuzzy,
        fuzzy_score,
    })
}

fn structured_quality(query: &str, candidate: &str) -> Option<MatchQuality> {
    let (query_root, query_subcommand) = command_parts(query);
    let (candidate_root, candidate_subcommand) = command_parts(candidate);
    let query_subcommand = query_subcommand?;
    let candidate_subcommand = candidate_subcommand?;
    if !query_root.eq_ignore_ascii_case(candidate_root) {
        return None;
    }

    if let Some(quality) = line_quality(query, candidate) {
        if quality.tier != HistoryTier::Fuzzy {
            return Some(quality);
        }
    }

    let query_subcommand = query_subcommand.to_lowercase();
    let candidate_subcommand = candidate_subcommand.to_lowercase();
    if candidate_subcommand.starts_with(&query_subcommand) {
        return Some(MatchQuality {
            tier: HistoryTier::Prefix,
            fuzzy_score: 0,
        });
    }
    if candidate_subcommand.contains(&query_subcommand) {
        return Some(MatchQuality {
            tier: HistoryTier::Contains,
            fuzzy_score: 0,
        });
    }

    fuzzy_subsequence_score(&query_subcommand, &candidate_subcommand).map(|fuzzy_score| {
        MatchQuality {
            tier: HistoryTier::Fuzzy,
            fuzzy_score,
        }
    })
}

fn command_parts(command: &str) -> (&str, Option<&str>) {
    let mut tokens = command.split_whitespace();
    let root = tokens.next().unwrap_or_default();
    let subcommand = tokens.find(|token| !token.starts_with('-'));
    (root, subcommand)
}

fn command_alternatives(command: &str, aliases: &[(&str, &str)]) -> Vec<String> {
    let original = command.to_owned();
    let mut alternatives = vec![original.clone()];
    let mut current = original;

    for _ in 0..aliases.len() {
        let Some(expanded) = expand_root_alias(&current, aliases) else {
            break;
        };
        if alternatives
            .iter()
            .any(|alternative| alternative.eq_ignore_ascii_case(&expanded))
        {
            break;
        }
        alternatives.push(expanded.clone());
        current = expanded;
    }

    alternatives
}

fn tokenized_command_alternatives(command: &str, aliases: &[(&str, &str)]) -> Vec<String> {
    const MAX_ALIASES: usize = 4_096;

    let original = command.to_owned();
    let mut alternatives = vec![original.clone()];
    let mut current = original;

    for _ in 0..aliases.len().min(MAX_ALIASES) {
        let Some(expanded) = expand_root_alias_tokenized(&current, aliases) else {
            break;
        };
        if alternatives
            .iter()
            .any(|alternative| alternative.eq_ignore_ascii_case(&expanded))
        {
            break;
        }
        alternatives.push(expanded.clone());
        current = expanded;
    }

    alternatives
}

fn expand_root_alias_tokenized(command: &str, aliases: &[(&str, &str)]) -> Option<String> {
    let parsed = tokenize(command, command.len()).ok()?;
    let root = parsed.tokens.first()?;
    if !is_plain_history_token(&parsed, root) {
        return None;
    }
    let (_, canonical) = aliases
        .iter()
        .take(4_096)
        .find(|(alias, _)| root.cooked.eq_ignore_ascii_case(alias.trim()))?;
    let canonical = canonical.trim();
    if canonical.is_empty() {
        return None;
    }

    let mut expanded =
        String::with_capacity(command.len() - (root.raw.end - root.raw.start) + canonical.len());
    expanded.push_str(&command[..root.raw.start]);
    expanded.push_str(canonical);
    expanded.push_str(&command[root.raw.end..]);
    Some(expanded)
}

fn is_plain_history_token(line: &TokenizedLine, token: &ShellToken) -> bool {
    token.quote == QuoteKind::Unquoted
        && token.complete
        && token.raw_text(&line.line) == Some(token.cooked.as_str())
}

fn expand_root_alias(command: &str, aliases: &[(&str, &str)]) -> Option<String> {
    let root_end = command
        .char_indices()
        .find_map(|(index, character)| character.is_whitespace().then_some(index))
        .unwrap_or(command.len());
    let root = &command[..root_end];
    let (_, canonical) = aliases
        .iter()
        .find(|(alias, _)| root.eq_ignore_ascii_case(alias.trim()))?;
    let canonical = canonical.trim();
    if canonical.is_empty() {
        return None;
    }

    let remainder = &command[root_end..];
    if remainder.is_empty() {
        Some(canonical.to_owned())
    } else {
        Some(format!("{canonical}{remainder}"))
    }
}

fn fuzzy_subsequence_score(query: &str, candidate: &str) -> Option<usize> {
    if query.is_empty() {
        return Some(0);
    }

    let query: Vec<_> = query.chars().collect();
    let candidate: Vec<_> = candidate.chars().collect();
    let mut query_index = 0;
    let mut first_match = None;
    let mut previous_match = None;
    let mut consecutive = 0;
    let mut boundaries = 0;
    let mut gaps = 0;

    for (candidate_index, character) in candidate.iter().enumerate() {
        if query.get(query_index) != Some(character) {
            continue;
        }
        first_match.get_or_insert(candidate_index);
        if previous_match.is_some_and(|previous| previous + 1 == candidate_index) {
            consecutive += 1;
        } else if let Some(previous) = previous_match {
            gaps += candidate_index.saturating_sub(previous + 1);
        }
        if candidate_index == 0
            || candidate[candidate_index - 1].is_whitespace()
            || matches!(candidate[candidate_index - 1], '-' | '_' | '/' | '.')
        {
            boundaries += 1;
        }
        previous_match = Some(candidate_index);
        query_index += 1;
        if query_index == query.len() {
            break;
        }
    }

    if query_index != query.len() {
        return None;
    }

    let base = query.len() * 1_000 + consecutive * 100 + boundaries * 50;
    let penalty = first_match.unwrap_or_default() * 5
        + gaps * 10
        + candidate.len().saturating_sub(query.len());
    Some(base.saturating_sub(penalty))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands(entries: &[HistoryEntry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.command.as_str()).collect()
    }

    fn matches(entries: &[HistoryMatch]) -> Vec<(&str, HistoryTier)> {
        entries
            .iter()
            .map(|entry| (entry.entry.command.as_str(), entry.tier))
            .collect()
    }

    #[test]
    fn parses_plain_and_extended_zsh_history() {
        let entries = parse_history(
            HistoryFormat::Zsh,
            "  git status  \n: 1700000000:12;cargo test\n: 1700000010:0;echo done\n".as_bytes(),
        );

        assert_eq!(
            entries,
            vec![
                HistoryEntry::new("  git status  "),
                HistoryEntry {
                    command: "cargo test".into(),
                    timestamp: Some(1_700_000_000),
                    duration: Some(12),
                },
                HistoryEntry {
                    command: "echo done".into(),
                    timestamp: Some(1_700_000_010),
                    duration: Some(0),
                },
            ]
        );
    }

    #[test]
    fn zsh_preserves_continued_commands_and_skips_malformed_metadata() {
        let entries = parse_history(
            HistoryFormat::Zsh,
            ": 1700000000:1;printf 'one\\\ntwo'\n: 1700000001:bad;broken\necho valid\n: 123"
                .as_bytes(),
        );

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].command, "printf 'one\ntwo'");
        assert_eq!(entries[1].command, "echo valid");
    }

    #[test]
    fn zsh_recovers_metafied_multibyte_commands() {
        // zsh writes 0xc3 0x9f ("ß") as 0xc3 0x83 0xbf, because 0x9f is in the
        // range it escapes. The same applies to "ü" as 0xc3 0xbc, which needs
        // no escape, so both forms appear in one record.
        let mut contents = Vec::from(b": 1700000000:0;echo Gr\xc3\xbc\xc3".as_slice());
        contents.extend_from_slice(b"\x83\xbfe\n");

        let entries = parse_history(HistoryFormat::Zsh, &contents);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "echo Grüße");
        assert!(!entries[0].command.contains('\u{FFFD}'));
    }

    #[test]
    fn only_zsh_history_is_unescaped_and_a_truncated_escape_is_kept() {
        let bash = b"echo \xc3\x83\xbf\n";
        assert_eq!(
            parse_history(HistoryFormat::Bash, bash)[0].command,
            String::from_utf8_lossy(b"echo \xc3\x83\xbf").as_ref()
        );

        let truncated = b"echo done\n\x83";
        let entries = parse_history(HistoryFormat::Zsh, truncated);
        assert_eq!(entries[0].command, "echo done");
    }

    #[test]
    fn zsh_keeps_plain_colon_commands() {
        let entries = parse_history(HistoryFormat::Zsh, ": reload\n: 123abc\n".as_bytes());
        assert_eq!(commands(&entries), vec![": reload", ": 123abc"]);
    }

    #[test]
    fn parses_bash_timestamps_for_only_the_following_command() {
        let entries = parse_history(
            HistoryFormat::Bash,
            "#1700000000\n  git status  \necho plain\n# not-a-time\n#1700000010\ncargo test\n"
                .as_bytes(),
        );

        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].command, "  git status  ");
        assert_eq!(entries[0].timestamp, Some(1_700_000_000));
        assert_eq!(entries[1], HistoryEntry::new("echo plain"));
        assert_eq!(entries[2], HistoryEntry::new("# not-a-time"));
        assert_eq!(entries[3].timestamp, Some(1_700_000_010));
    }

    #[test]
    fn bash_ignores_blank_records_and_incomplete_timestamp_tail() {
        let entries = parse_history(HistoryFormat::Bash, "\nls\n  \n#1700000000\n".as_bytes());
        assert_eq!(entries, vec![HistoryEntry::new("ls")]);
    }

    #[test]
    fn parses_fish_records_metadata_and_escaped_newlines() {
        let entries = parse_history(
            HistoryFormat::Fish,
            "# fish history\n- cmd: echo one\\ntwo\n  when: 1700000000\n  paths:\n    - /tmp\n- cmd: printf '\\\\n'\n- cmd: cargo test\n  when: invalid\n".as_bytes(),
        );

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].command, "echo one\ntwo");
        assert_eq!(entries[0].timestamp, Some(1_700_000_000));
        assert_eq!(entries[1].command, "printf '\\n'");
        assert_eq!(entries[2], HistoryEntry::new("cargo test"));
    }

    #[test]
    fn fish_tolerates_empty_and_malformed_tail_records() {
        let entries = parse_history(
            HistoryFormat::Fish,
            "  when: 12\n- cmd:   \n  when: 13\n- cmd: echo ok\\\n".as_bytes(),
        );
        assert_eq!(entries, vec![HistoryEntry::new("echo ok\\")]);
    }

    #[test]
    fn merge_prefers_newest_session_occurrence_and_metadata() {
        let persistent = vec![
            HistoryEntry::new("git status").with_timestamp(10),
            HistoryEntry::new("cargo test").with_timestamp(20),
            HistoryEntry::new("git status").with_timestamp(30),
        ];
        let session = vec![
            HistoryEntry::new("echo session"),
            HistoryEntry::new(" git status ").with_timestamp(40),
            HistoryEntry::new("cargo fmt"),
        ];

        let merged = merge_history(&persistent, &session);
        assert_eq!(
            commands(&merged),
            vec![
                "cargo fmt",
                " git status ",
                "echo session",
                "git status",
                "cargo test"
            ]
        );
        assert_eq!(merged[1].timestamp, Some(40));
    }

    #[test]
    fn merge_is_exact_and_deterministic_without_timestamps() {
        let persistent = vec![
            HistoryEntry::new("Git status"),
            HistoryEntry::new("git status"),
        ];
        let merged = merge_history(&persistent, &[]);
        assert_eq!(commands(&merged), vec!["git status", "Git status"]);
    }

    #[test]
    fn empty_search_returns_at_most_one_hundred_newest_unique_commands() {
        let mut entries: Vec<_> = (0..105)
            .map(|number| HistoryEntry::new(format!("echo {number}")))
            .collect();
        entries.insert(1, HistoryEntry::new("echo 0"));

        let found = search_history(&entries, "", &[]);
        assert_eq!(found.len(), 100);
        assert_eq!(found[0].entry.command, "echo 0");
        assert_eq!(found[1].entry.command, "echo 1");
    }

    #[test]
    fn orders_first_three_tiers_only_by_recency() {
        let entries = vec![
            HistoryEntry::new("before git status after"),
            HistoryEntry::new("git status --short"),
            HistoryEntry::new("GIT STATUS"),
            HistoryEntry::new("git status --branch"),
        ];

        let found = search_history(&entries, "git status", &[]);
        assert_eq!(
            matches(&found),
            vec![
                ("GIT STATUS", HistoryTier::Exact),
                ("git status --short", HistoryTier::Prefix),
                ("git status --branch", HistoryTier::Prefix),
            ]
        );
    }

    #[test]
    fn fuzzy_matches_sort_by_quality_then_recency() {
        let entries = vec![HistoryEntry::new("g---t---s"), HistoryEntry::new("g-t-s")];

        let found = search_history(&entries, "gts", &[]);
        assert_eq!(found[0].entry.command, "g-t-s");
        assert!(found.iter().all(|entry| entry.tier == HistoryTier::Fuzzy));
        assert!(found[0].fuzzy_score > found[1].fuzzy_score);
    }

    #[test]
    fn fuzzy_quality_ties_resolve_by_recency() {
        let entries = vec![HistoryEntry::new("a-b-c"), HistoryEntry::new("a_b_c")];
        let found = search_history(&entries, "abc", &[]);
        assert_eq!(found[0].entry.command, "a-b-c");
    }

    #[test]
    fn multi_token_search_keeps_root_and_filters_subcommand() {
        let entries = vec![
            HistoryEntry::new("echo git checkout"),
            HistoryEntry::new("git --no-pager checkout main"),
            HistoryEntry::new("git commit -m message"),
            HistoryEntry::new("git checkout feature"),
        ];

        let found = search_history(&entries, "git che", &[]);
        assert_eq!(
            commands(
                &found
                    .iter()
                    .map(|item| item.entry.clone())
                    .collect::<Vec<_>>()
            ),
            vec!["git --no-pager checkout main", "git checkout feature"]
        );
        assert!(found.iter().all(|item| item.tier == HistoryTier::Prefix));
    }

    #[test]
    fn multi_token_search_falls_back_when_subcommand_has_no_match() {
        let entries = vec![
            HistoryEntry::new("echo greeting today"),
            HistoryEntry::new("cargo test"),
        ];
        let found = search_history(&entries, "git", &[]);
        assert_eq!(found[0].entry.command, "echo greeting today");
        assert_eq!(found[0].tier, HistoryTier::Fuzzy);
    }

    #[test]
    fn aliases_and_canonical_commands_are_equivalent_both_ways() {
        let aliases = [("g", "git"), ("gst", "git status")];
        let entries = vec![
            HistoryEntry::new("g checkout feature"),
            HistoryEntry::new("git status --short"),
            HistoryEntry::new("gst --branch"),
        ];

        let canonical = search_history(&entries, "git che", &aliases);
        assert_eq!(canonical[0].entry.command, "g checkout feature");
        let alias = search_history(&entries, "gst", &aliases);
        assert_eq!(
            commands(
                &alias
                    .iter()
                    .map(|item| item.entry.clone())
                    .collect::<Vec<_>>()
            ),
            vec!["git status --short", "gst --branch"]
        );
    }

    #[test]
    fn alias_expansion_handles_chains_without_looping() {
        let aliases = [("gs", "gst"), ("gst", "git status"), ("loop", "loop")];
        let entries = vec![HistoryEntry::new("git status --short")];
        assert_eq!(
            search_history(&entries, "gs", &aliases)[0].entry.command,
            "git status --short"
        );
        assert!(search_history(&entries, "loop", &aliases).is_empty());
    }

    #[test]
    fn spec_search_skips_separate_quoted_and_attached_option_values() {
        let entries = vec![
            HistoryEntry::new("git -C Greendale checkout main"),
            HistoryEntry::new("git --git-dir=/tmp/repo checkout feature"),
            HistoryEntry::new("git -CGreendale checkout campus"),
            HistoryEntry::new("git -C checkout status"),
            HistoryEntry::new("git -C repo commit"),
        ];

        let found = search_history_with_specs(
            &entries,
            "git -C 'Greendale campus' che",
            &[],
            &history_specs(),
        );

        assert_eq!(
            commands(
                &found
                    .iter()
                    .map(|item| item.entry.clone())
                    .collect::<Vec<_>>()
            ),
            [
                "git -C Greendale checkout main",
                "git --git-dir=/tmp/repo checkout feature",
                "git -CGreendale checkout campus",
            ]
        );
        assert!(found.iter().all(|item| item.tier == HistoryTier::Prefix));
    }

    #[test]
    fn spec_search_canonicalizes_root_and_subcommand_aliases() {
        let entries = vec![
            HistoryEntry::new("git --no-pager checkout main"),
            HistoryEntry::new("g checkout feature"),
            HistoryEntry::new("git -C 'Greendale campus' co courtyard"),
            HistoryEntry::new("git commit -m message"),
        ];

        let found = search_history_with_specs(
            &entries,
            "g -C 'Greendale campus' co",
            &[],
            &history_specs(),
        );

        assert_eq!(
            commands(
                &found
                    .iter()
                    .map(|item| item.entry.clone())
                    .collect::<Vec<_>>()
            ),
            [
                "git --no-pager checkout main",
                "g checkout feature",
                "git -C 'Greendale campus' co courtyard",
            ]
        );
        assert!(found.iter().all(|item| item.tier == HistoryTier::Prefix));
    }

    #[test]
    fn spec_search_uses_tokenized_multiword_aliases_for_lookup_only() {
        let aliases = [("gc", "git checkout")];
        let entries = vec![
            HistoryEntry::new("git checkout main"),
            HistoryEntry::new("gc Greendale"),
            HistoryEntry::new("git commit"),
        ];

        let found = search_history_with_specs(&entries, "gc ma", &aliases, &history_specs());

        assert_eq!(found[0].entry.command, "git checkout main");
        assert!(!found.iter().any(|item| item.entry.command == "git commit"));
        let alias = search_history_with_specs(&entries, "gc", &aliases, &history_specs());
        assert!(
            alias
                .iter()
                .any(|item| item.entry.command == "gc Greendale")
        );
    }

    #[test]
    fn spec_search_respects_option_terminator_position() {
        let entries = vec![
            HistoryEntry::new("echo git -- che"),
            HistoryEntry::new("git checkout main"),
            HistoryEntry::new("git commit ma"),
        ];

        let broad = search_history_with_specs(&entries, "git -- che", &[], &history_specs());
        assert_eq!(broad[0].entry.command, "echo git -- che");
        assert_eq!(broad[0].tier, HistoryTier::Contains);

        let structured =
            search_history_with_specs(&entries, "git checkout -- ma", &[], &history_specs());
        assert_eq!(commands_from_matches(&structured), ["git checkout main"]);
    }

    #[test]
    fn spec_search_treats_unknown_roots_and_options_conservatively() {
        let candidates = vec![
            HistoryEntry::new("git --mystery checkout"),
            HistoryEntry::new("git checkout main"),
        ];
        let structured = search_history_with_specs(&candidates, "git che", &[], &history_specs());
        assert_eq!(commands_from_matches(&structured), ["git checkout main"]);

        let broad_entries = vec![HistoryEntry::new("echo git --mystery repo che")];
        let unknown_option = search_history_with_specs(
            &broad_entries,
            "git --mystery repo che",
            &[],
            &history_specs(),
        );
        assert_eq!(
            unknown_option[0].entry.command,
            "echo git --mystery repo che"
        );

        let unknown_root = search_history_with_specs(
            &[HistoryEntry::new("echo mystery che")],
            "mystery che",
            &[],
            &history_specs(),
        );
        assert_eq!(unknown_root[0].entry.command, "echo mystery che");
    }

    #[test]
    fn spec_search_falls_back_broadly_when_structure_has_no_candidate() {
        let entries = vec![HistoryEntry::new("echo git checkout reminder")];

        let found = search_history_with_specs(&entries, "git che", &[], &history_specs());

        assert_eq!(found[0].entry.command, "echo git checkout reminder");
        assert_eq!(found[0].tier, HistoryTier::Contains);
    }

    #[test]
    fn preserves_exact_command_whitespace_during_parse_and_merge() {
        let parsed = parse_history(HistoryFormat::Bash, "  echo Greendale  \n".as_bytes());
        assert_eq!(parsed[0].command, "  echo Greendale  ");

        let merged = merge_history(
            &[HistoryEntry::new("git status")],
            &[HistoryEntry::new("git status ")],
        );
        assert_eq!(commands(&merged), ["git status ", "git status"]);
    }

    #[test]
    fn trailing_query_space_is_meaningful() {
        let entries = vec![
            HistoryEntry::new("github auth status"),
            HistoryEntry::new("git checkout main"),
        ];
        let found = search_history(&entries, "git ", &[]);
        assert_eq!(found[0].entry.command, "git checkout main");
        assert_eq!(found[0].tier, HistoryTier::Prefix);
        assert_eq!(found[1].entry.command, "github auth status");
        assert_eq!(found[1].tier, HistoryTier::Fuzzy);
    }

    fn history_specs() -> SpecIndex {
        let git = CommandSpec::new("git", "version control")
            .with_alias("g")
            .with_option(
                OptionSpec::new("-C", "select a working directory")
                    .takes_value(true)
                    .global(true),
            )
            .with_option(
                OptionSpec::new("--git-dir", "select a repository")
                    .takes_value(true)
                    .global(true),
            )
            .with_option(OptionSpec::new("--no-pager", "disable paging").global(true))
            .with_subcommand(CommandSpec::new("checkout", "switch branches").with_alias("co"))
            .with_subcommand(
                CommandSpec::new("commit", "record changes")
                    .with_option(OptionSpec::new("-m", "set the message").takes_value(true)),
            )
            .with_subcommand(CommandSpec::new("status", "show status"));
        SpecIndex::new([git]).unwrap()
    }

    fn commands_from_matches(entries: &[HistoryMatch]) -> Vec<&str> {
        entries
            .iter()
            .map(|entry| entry.entry.command.as_str())
            .collect()
    }
}
