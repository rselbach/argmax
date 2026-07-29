//! Shell history parsing, session merging, and explicit-tier searching.

use std::cmp::Ordering;
use std::collections::HashSet;

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
/// being returned.
#[must_use]
pub fn parse_history(format: HistoryFormat, contents: &str) -> Vec<HistoryEntry> {
    match format {
        HistoryFormat::Zsh => parse_zsh(contents),
        HistoryFormat::Bash => parse_bash(contents),
        HistoryFormat::Fish => parse_fish(contents),
    }
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
        if entry.command.trim().is_empty() || !seen.insert(entry.command.clone()) {
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
                entry,
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

fn parse_zsh(contents: &str) -> Vec<HistoryEntry> {
    let mut entries = Vec::new();
    let mut lines = contents.lines().peekable();

    while let Some(raw_line) = lines.next() {
        let mut line = raw_line.trim_end_matches('\r').to_owned();
        while has_unescaped_trailing_backslash(&line) {
            let Some(continuation) = lines.next() else {
                break;
            };
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

fn unique_newest(entries: &[HistoryEntry]) -> Vec<(usize, HistoryEntry)> {
    let mut seen = HashSet::new();
    entries
        .iter()
        .enumerate()
        .filter_map(|(recency, entry)| {
            if entry.command.trim().is_empty() || !seen.insert(entry.command.clone()) {
                return None;
            }
            Some((recency, entry.clone()))
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

fn collect_matches(
    entries: &[(usize, HistoryEntry)],
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
                entry: entry.clone(),
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
            "  git status  \n: 1700000000:12;cargo test\n: 1700000010:0;echo done\n",
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
            ": 1700000000:1;printf 'one\\\ntwo'\n: 1700000001:bad;broken\necho valid\n: 123",
        );

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].command, "printf 'one\\\ntwo'");
        assert_eq!(entries[1].command, "echo valid");
    }

    #[test]
    fn zsh_keeps_plain_colon_commands() {
        let entries = parse_history(HistoryFormat::Zsh, ": reload\n: 123abc\n");
        assert_eq!(commands(&entries), vec![": reload", ": 123abc"]);
    }

    #[test]
    fn parses_bash_timestamps_for_only_the_following_command() {
        let entries = parse_history(
            HistoryFormat::Bash,
            "#1700000000\n  git status  \necho plain\n# not-a-time\n#1700000010\ncargo test\n",
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
        let entries = parse_history(HistoryFormat::Bash, "\nls\n  \n#1700000000\n");
        assert_eq!(entries, vec![HistoryEntry::new("ls")]);
    }

    #[test]
    fn parses_fish_records_metadata_and_escaped_newlines() {
        let entries = parse_history(
            HistoryFormat::Fish,
            "# fish history\n- cmd: echo one\\ntwo\n  when: 1700000000\n  paths:\n    - /tmp\n- cmd: printf '\\\\n'\n- cmd: cargo test\n  when: invalid\n",
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
            "  when: 12\n- cmd:   \n  when: 13\n- cmd: echo ok\\\n",
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
    fn preserves_exact_command_whitespace_during_parse_and_merge() {
        let parsed = parse_history(HistoryFormat::Bash, "  echo Greendale  \n");
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
}
