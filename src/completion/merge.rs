use std::cmp::Ordering;
use std::collections::BTreeMap;

use super::{CompletionQuery, InsertionBehavior, Suggestion};

/// Deduplicates candidates by their complete insertion result and merges metadata.
///
/// Invalid edits, blank candidates, and candidates identical to the normalized
/// current buffer are discarded. Each result is represented by a canonical
/// one-edit delta relative to the shared query. The query is indexed once, while
/// retained keys own only changed bytes bounded by candidate replacement size.
/// No ranking or UI limit is applied here; downstream consumers own ordering and
/// presentation bounds.
#[must_use]
pub(crate) fn merge_suggestions(
    query: &CompletionQuery,
    batches: impl IntoIterator<Item = Vec<Suggestion>>,
) -> Vec<Suggestion> {
    let mut prepared = batches
        .into_iter()
        .flatten()
        .filter_map(|candidate| PreparedCandidate::new(query, candidate))
        .collect::<Vec<_>>();
    if prepared.is_empty() {
        return Vec::new();
    }

    let query_bytes = query.line.as_bytes();
    let forward = LceIndex::new(query_bytes);
    let reversed_query = query_bytes.iter().rev().copied().collect::<Vec<_>>();
    let reverse = LceIndex::new(&reversed_query);
    let prefix_trim = PrefixTrimIndex::new(&query.line);
    let normalized_query_len = query.line.trim_end().len();
    let mut grouped = BTreeMap::<ResultKey, Vec<Suggestion>>::new();

    for candidate in prepared.drain(..) {
        let key = ResultKey::new(query_bytes, &candidate, &forward, &reverse);
        let normalized_len =
            candidate.normalized_result_len(query.line.len(), normalized_query_len, &prefix_trim);
        if normalized_len == 0
            || (normalized_len == normalized_query_len && key.common_prefix >= normalized_query_len)
        {
            continue;
        }
        grouped.entry(key).or_default().push(candidate.suggestion);
    }

    let mut merged = Vec::with_capacity(grouped.len());
    for mut duplicates in grouped.into_values() {
        duplicates.sort_unstable_by(compare_candidates);
        let mut duplicates = duplicates.into_iter();
        let Some(mut candidate) = duplicates.next() else {
            continue;
        };
        for duplicate in duplicates {
            candidate.merge_metadata(duplicate);
        }
        merged.push(compact_candidate(candidate));
    }

    merged.into_boxed_slice().into_vec()
}

struct PreparedCandidate {
    suggestion: Suggestion,
    resolved_replacement: String,
}

impl PreparedCandidate {
    fn new(query: &CompletionQuery, candidate: Suggestion) -> Option<Self> {
        if candidate.display().trim().is_empty() {
            return None;
        }
        let edit = candidate.edit();
        if edit.range.start > edit.range.end
            || !query.line.is_char_boundary(edit.range.start)
            || !query.line.is_char_boundary(edit.range.end)
        {
            return None;
        }

        let candidate = compact_candidate(candidate);
        let edit = candidate.edit();
        let suffix = &query.line[edit.range.end..];
        let mut resolved_replacement = edit.replacement.clone();
        match candidate.insertion() {
            InsertionBehavior::Exact => {}
            InsertionBehavior::AppendSpace => {
                if !resolved_replacement
                    .chars()
                    .last()
                    .is_some_and(char::is_whitespace)
                    && suffix.is_empty()
                {
                    resolved_replacement.push(' ');
                }
            }
            InsertionBehavior::Directory => {
                if !resolved_replacement.ends_with('/') && !suffix.starts_with('/') {
                    resolved_replacement.push('/');
                }
            }
        }

        Some(Self {
            suggestion: candidate,
            resolved_replacement: compact_string(resolved_replacement),
        })
    }

    fn normalized_result_len(
        &self,
        query_len: usize,
        normalized_query_len: usize,
        prefix_trim: &PrefixTrimIndex,
    ) -> usize {
        let edit = self.suggestion.edit();
        let result_len = result_len(
            query_len,
            edit.range.start,
            edit.range.end,
            self.resolved_replacement.len(),
        );
        if edit.range.end < normalized_query_len {
            return result_len - (query_len - normalized_query_len);
        }

        let replacement_len = self.resolved_replacement.trim_end().len();
        if replacement_len == 0 {
            prefix_trim.at(edit.range.start)
        } else {
            edit.range.start + replacement_len
        }
    }
}

/// Unique representation of a result relative to the authoritative query.
///
/// `common_prefix` is the result/query longest common prefix. The unchanged
/// query suffix starts at `query_suffix_start`; `changed` is the intervening
/// result byte range. Longest-prefix/suffix canonicalization makes equal results
/// produce equal keys even when providers describe them with different edits.
#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct ResultKey {
    common_prefix: usize,
    query_suffix_start: usize,
    changed: Box<[u8]>,
}

impl ResultKey {
    fn new(
        query: &[u8],
        candidate: &PreparedCandidate,
        forward: &LceIndex,
        reverse: &LceIndex,
    ) -> Self {
        let edit = candidate.suggestion.edit();
        let replacement = candidate.resolved_replacement.as_bytes();
        let common_prefix = result_common_prefix(
            query,
            edit.range.start,
            edit.range.end,
            replacement,
            forward,
        );
        let result_len = result_len(
            query.len(),
            edit.range.start,
            edit.range.end,
            replacement.len(),
        );
        let raw_suffix = result_common_suffix(
            query,
            edit.range.start,
            edit.range.end,
            replacement,
            reverse,
        );
        let common_suffix = raw_suffix
            .min(query.len().saturating_sub(common_prefix))
            .min(result_len.saturating_sub(common_prefix));
        let result_middle_end = result_len - common_suffix;
        let changed = copy_result_range(
            query,
            edit.range.start,
            edit.range.end,
            replacement,
            common_prefix,
            result_middle_end,
        );
        debug_assert!(changed.len() <= replacement.len());

        Self {
            common_prefix,
            query_suffix_start: query.len() - common_suffix,
            changed,
        }
    }
}

fn result_common_prefix(
    query: &[u8],
    edit_start: usize,
    edit_end: usize,
    replacement: &[u8],
    forward: &LceIndex,
) -> usize {
    let query_tail = &query[edit_start..];
    let replacement_match = common_prefix_len(query_tail, replacement);
    if replacement_match < query_tail.len().min(replacement.len()) {
        return edit_start + replacement_match;
    }
    if replacement_match == query_tail.len() {
        return query.len();
    }

    let query_after_replacement = edit_start + replacement.len();
    query_after_replacement + forward.lcp(query_after_replacement, edit_end)
}

fn result_common_suffix(
    query: &[u8],
    edit_start: usize,
    edit_end: usize,
    replacement: &[u8],
    reverse: &LceIndex,
) -> usize {
    let unchanged_suffix = query.len() - edit_end;
    let query_prefix = &query[..edit_end];
    let replacement_match = common_suffix_len(query_prefix, replacement);
    if replacement_match < query_prefix.len().min(replacement.len()) {
        return unchanged_suffix + replacement_match;
    }
    if replacement_match == query_prefix.len() {
        return query.len();
    }

    let query_before_replacement = edit_end - replacement.len();
    let reversed_left = query.len() - edit_start;
    let reversed_right = query.len() - query_before_replacement;
    unchanged_suffix + replacement.len() + reverse.lcp(reversed_left, reversed_right)
}

fn common_prefix_len(left: &[u8], right: &[u8]) -> usize {
    left.iter()
        .zip(right)
        .take_while(|(left, right)| left == right)
        .count()
}

fn common_suffix_len(left: &[u8], right: &[u8]) -> usize {
    left.iter()
        .rev()
        .zip(right.iter().rev())
        .take_while(|(left, right)| left == right)
        .count()
}

fn result_len(query_len: usize, edit_start: usize, edit_end: usize, replacement: usize) -> usize {
    edit_start + replacement + (query_len - edit_end)
}

fn copy_result_range(
    query: &[u8],
    edit_start: usize,
    edit_end: usize,
    replacement: &[u8],
    start: usize,
    end: usize,
) -> Box<[u8]> {
    if start >= end {
        return Box::default();
    }

    let mut changed = Vec::with_capacity(end - start);
    copy_overlap(&mut changed, replacement, edit_start, start, end);
    copy_overlap(
        &mut changed,
        &query[edit_end..],
        edit_start + replacement.len(),
        start,
        end,
    );
    changed.into_boxed_slice()
}

fn copy_overlap(
    output: &mut Vec<u8>,
    segment: &[u8],
    segment_start: usize,
    range_start: usize,
    range_end: usize,
) {
    let segment_end = segment_start + segment.len();
    let overlap_start = range_start.max(segment_start);
    let overlap_end = range_end.min(segment_end);
    if overlap_start < overlap_end {
        output.extend_from_slice(
            &segment[overlap_start - segment_start..overlap_end - segment_start],
        );
    }
}

/// Exact longest-common-extension queries over one immutable byte string.
///
/// Prefix doubling uses stable counting sorts, then Kasai LCP construction and
/// a linear-space range-minimum tree. Construction is `O(n log n)` and every
/// query is `O(log n)` without probabilistic hashes or rescanning query slices.
struct LceIndex {
    len: usize,
    suffix_rank: Vec<usize>,
    range_minimum: Vec<usize>,
    tree_base: usize,
}

impl LceIndex {
    fn new(bytes: &[u8]) -> Self {
        if bytes.is_empty() {
            return Self {
                len: 0,
                suffix_rank: Vec::new(),
                range_minimum: vec![usize::MAX; 2],
                tree_base: 1,
            };
        }

        let len = bytes.len();
        let mut suffixes = (0..len).collect::<Vec<_>>();
        let mut scratch = vec![0; len];
        let mut ranks = bytes
            .iter()
            .map(|byte| usize::from(*byte) + 1)
            .collect::<Vec<_>>();
        let mut new_ranks = vec![0; len];
        let mut counts = vec![0; len.max(257) + 1];
        let mut max_rank = 256usize;
        let mut offset = 1usize;

        loop {
            stable_counting_sort(&suffixes, &mut scratch, &mut counts, max_rank, |suffix| {
                suffix
                    .checked_add(offset)
                    .filter(|position| *position < len)
                    .map_or(0, |position| ranks[position])
            });
            stable_counting_sort(&scratch, &mut suffixes, &mut counts, max_rank, |suffix| {
                ranks[suffix]
            });

            let mut classes = 1usize;
            new_ranks[suffixes[0]] = classes;
            for pair in suffixes.windows(2) {
                let left = suffix_rank_pair(&ranks, pair[0], offset);
                let right = suffix_rank_pair(&ranks, pair[1], offset);
                if left != right {
                    classes += 1;
                }
                new_ranks[pair[1]] = classes;
            }
            std::mem::swap(&mut ranks, &mut new_ranks);
            max_rank = classes;
            if classes == len {
                break;
            }
            offset = offset.saturating_mul(2);
        }

        let mut suffix_rank = vec![0; len];
        for (rank, suffix) in suffixes.iter().copied().enumerate() {
            suffix_rank[suffix] = rank;
        }
        let adjacent_lcp = adjacent_lcp(bytes, &suffixes, &suffix_rank);
        let tree_base = len.next_power_of_two();
        let mut range_minimum = vec![usize::MAX; tree_base * 2];
        range_minimum[tree_base..tree_base + len].copy_from_slice(&adjacent_lcp);
        for index in (1..tree_base).rev() {
            range_minimum[index] = range_minimum[index * 2].min(range_minimum[index * 2 + 1]);
        }

        Self {
            len,
            suffix_rank,
            range_minimum,
            tree_base,
        }
    }

    fn lcp(&self, left: usize, right: usize) -> usize {
        if left == right {
            return self.len.saturating_sub(left);
        }
        if left >= self.len || right >= self.len {
            return 0;
        }

        let left_rank = self.suffix_rank[left];
        let right_rank = self.suffix_rank[right];
        let mut range_start = left_rank.min(right_rank) + 1 + self.tree_base;
        let mut range_end = left_rank.max(right_rank) + 1 + self.tree_base;
        let mut minimum = usize::MAX;
        while range_start < range_end {
            if range_start % 2 == 1 {
                minimum = minimum.min(self.range_minimum[range_start]);
                range_start += 1;
            }
            if range_end % 2 == 1 {
                range_end -= 1;
                minimum = minimum.min(self.range_minimum[range_end]);
            }
            range_start /= 2;
            range_end /= 2;
        }
        minimum
    }
}

fn stable_counting_sort(
    input: &[usize],
    output: &mut [usize],
    counts: &mut [usize],
    max_key: usize,
    mut key: impl FnMut(usize) -> usize,
) {
    counts[..=max_key].fill(0);
    for value in input.iter().copied() {
        counts[key(value)] += 1;
    }

    let mut position = 0usize;
    for count in &mut counts[..=max_key] {
        let size = *count;
        *count = position;
        position += size;
    }
    for value in input.iter().copied() {
        let key = key(value);
        output[counts[key]] = value;
        counts[key] += 1;
    }
}

fn suffix_rank_pair(ranks: &[usize], suffix: usize, offset: usize) -> (usize, usize) {
    (
        ranks[suffix],
        suffix
            .checked_add(offset)
            .filter(|position| *position < ranks.len())
            .map_or(0, |position| ranks[position]),
    )
}

fn adjacent_lcp(bytes: &[u8], suffixes: &[usize], suffix_rank: &[usize]) -> Vec<usize> {
    let mut adjacent = vec![0; bytes.len()];
    let mut matched = 0usize;
    for left in 0..bytes.len() {
        let rank = suffix_rank[left];
        if rank == 0 {
            matched = 0;
            continue;
        }
        let right = suffixes[rank - 1];
        while left + matched < bytes.len()
            && right + matched < bytes.len()
            && bytes[left + matched] == bytes[right + matched]
        {
            matched += 1;
        }
        adjacent[rank] = matched;
        matched = matched.saturating_sub(1);
    }
    adjacent
}

struct PrefixTrimIndex {
    trimmed_len: Vec<usize>,
}

impl PrefixTrimIndex {
    fn new(value: &str) -> Self {
        let mut trimmed_len = vec![0; value.len() + 1];
        let mut last_non_whitespace = 0usize;
        for (start, character) in value.char_indices() {
            let end = start + character.len_utf8();
            if !character.is_whitespace() {
                last_non_whitespace = end;
            }
            trimmed_len[end] = last_non_whitespace;
        }
        Self { trimmed_len }
    }

    fn at(&self, index: usize) -> usize {
        self.trimmed_len[index]
    }
}

fn compare_candidates(left: &Suggestion, right: &Suggestion) -> Ordering {
    left.identity()
        .cmp(right.identity())
        .then_with(|| left.edit().range.start.cmp(&right.edit().range.start))
        .then_with(|| left.edit().range.end.cmp(&right.edit().range.end))
        .then_with(|| left.edit().replacement.cmp(&right.edit().replacement))
        .then_with(|| insertion_order(left.insertion()).cmp(&insertion_order(right.insertion())))
        .then_with(|| left.source().cmp(&right.source()))
        .then_with(|| left.sources().cmp(right.sources()))
        .then_with(|| left.display().cmp(right.display()))
        .then_with(|| left.description().cmp(right.description()))
        .then_with(|| left.icon().cmp(right.icon()))
        .then_with(|| left.static_priority().total_cmp(&right.static_priority()))
        .then_with(|| left.confidence().total_cmp(&right.confidence()))
}

const fn insertion_order(insertion: InsertionBehavior) -> u8 {
    match insertion {
        InsertionBehavior::Exact => 0,
        InsertionBehavior::AppendSpace => 1,
        InsertionBehavior::Directory => 2,
    }
}

fn compact_candidate(mut candidate: Suggestion) -> Suggestion {
    candidate.edit.replacement = compact_string(candidate.edit.replacement);
    candidate.display = compact_string(candidate.display);
    candidate.description = compact_string(candidate.description);
    candidate.icon = compact_string(candidate.icon);
    candidate.identity = compact_string(candidate.identity);
    candidate
}

fn compact_string(value: String) -> String {
    value.into_boxed_str().into_string()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::*;
    use crate::completion::{SuggestionSource, TextEdit};

    fn suggestion(source: SuggestionSource, description: &str) -> Suggestion {
        Suggestion::new(
            TextEdit {
                range: 0..2,
                replacement: "git".into(),
            },
            "git",
            description,
            "command",
            source,
            InsertionBehavior::AppendSpace,
            format!("{}:git", source.badge()),
        )
    }

    #[test]
    fn merges_by_result_and_keeps_rich_metadata() {
        let query = CompletionQuery::new("gi", 2, Path::new("/tmp"), 1).unwrap();
        let merged = merge_suggestions(
            &query,
            [
                vec![suggestion(SuggestionSource::System, "")],
                vec![suggestion(
                    SuggestionSource::Spec,
                    "distributed version control",
                )],
            ],
        );

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].source, SuggestionSource::Spec);
        assert_eq!(merged[0].sources.len(), 2);
        assert_eq!(merged[0].description, "distributed version control");
    }

    #[test]
    fn drops_candidate_identical_to_current_buffer() {
        let query = CompletionQuery::new("git", 3, Path::new("/tmp"), 1).unwrap();
        let mut candidate = suggestion(SuggestionSource::Spec, "git");
        candidate.edit.range = 0..3;
        assert!(merge_suggestions(&query, [vec![candidate]]).is_empty());
    }

    #[test]
    fn handles_unicode_whitespace_without_materializing_result_keys() {
        let query = CompletionQuery::new("gi\u{2003}", 2, Path::new("/tmp"), 1).unwrap();
        let mut candidate = suggestion(SuggestionSource::Spec, "git");
        candidate.insertion = InsertionBehavior::Exact;
        candidate.edit.replacement = "gi".into();
        assert!(merge_suggestions(&query, [vec![candidate]]).is_empty());
    }

    #[test]
    fn lce_index_matches_naive_suffix_comparison_exactly() {
        for bytes in [
            b"".as_slice(),
            b"a",
            b"aaaaaa",
            b"banana",
            b"abracadabra",
            &[0, 255, 0, 255, 1],
        ] {
            let index = LceIndex::new(bytes);
            for left in 0..=bytes.len() {
                for right in 0..=bytes.len() {
                    assert_eq!(
                        index.lcp(left, right),
                        common_prefix_len(&bytes[left..], &bytes[right..]),
                        "left={left}, right={right}, bytes={bytes:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn lce_index_is_exact_for_every_short_ternary_string() {
        for len in 0..=7 {
            let combinations = 3usize.pow(len);
            for mut encoded in 0..combinations {
                let mut bytes = vec![0; len as usize];
                for byte in &mut bytes {
                    *byte = [0, 1, 2][encoded % 3];
                    encoded /= 3;
                }
                let index = LceIndex::new(&bytes);
                for left in 0..=bytes.len() {
                    for right in 0..=bytes.len() {
                        assert_eq!(
                            index.lcp(left, right),
                            common_prefix_len(&bytes[left..], &bytes[right..]),
                            "left={left}, right={right}, bytes={bytes:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn canonical_keys_match_materialized_results_for_overlapping_edits() {
        let query = CompletionQuery::new("aaaa  ", 6, Path::new("/tmp"), 1).unwrap();
        let mut candidates = Vec::new();
        for start in 0..=query.line.len() {
            for end in start..=query.line.len() {
                for replacement in ["", "a", "b", "aa", " "] {
                    candidates.push(Suggestion::new(
                        TextEdit {
                            range: start..end,
                            replacement: replacement.into(),
                        },
                        format!("candidate-{start}-{end}-{replacement:?}"),
                        "",
                        "command",
                        SuggestionSource::Spec,
                        InsertionBehavior::Exact,
                        format!("identity-{start}-{end}-{replacement:?}"),
                    ));
                }
            }
        }
        let want = candidates
            .iter()
            .filter_map(|candidate| candidate.resulting_line(&query).ok())
            .filter(|result| {
                !result.trim().is_empty() && result.trim_end() != query.line.trim_end()
            })
            .collect::<BTreeSet<_>>();

        let merged = merge_suggestions(&query, [candidates]);
        let got = merged
            .iter()
            .map(|candidate| candidate.resulting_line(&query).unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(got, want);
        assert_eq!(merged.len(), want.len());
    }

    #[test]
    fn canonical_key_equivalence_matches_results_across_unicode_and_insertions() {
        for line in ["", "abc", "banana", "éclair", "  a  "] {
            let query = CompletionQuery::new(line, line.len(), Path::new("/tmp"), 1).unwrap();
            let forward = LceIndex::new(query.line.as_bytes());
            let reversed = query.line.bytes().rev().collect::<Vec<_>>();
            let reverse = LceIndex::new(&reversed);
            let mut boundaries = query
                .line
                .char_indices()
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            boundaries.push(query.line.len());
            let mut by_result = BTreeMap::<String, ResultKey>::new();
            let mut by_key = BTreeMap::<ResultKey, String>::new();

            for start in boundaries.iter().copied() {
                for end in boundaries.iter().copied().filter(|end| *end >= start) {
                    for replacement in ["", "a", "é", "aa", " "] {
                        for insertion in [
                            InsertionBehavior::Exact,
                            InsertionBehavior::AppendSpace,
                            InsertionBehavior::Directory,
                        ] {
                            let suggestion = Suggestion::new(
                                TextEdit {
                                    range: start..end,
                                    replacement: replacement.into(),
                                },
                                "candidate",
                                "",
                                "command",
                                SuggestionSource::Spec,
                                insertion,
                                format!("{start}-{end}-{replacement:?}-{insertion:?}"),
                            );
                            let result = suggestion.resulting_line(&query).unwrap();
                            let prepared = PreparedCandidate::new(&query, suggestion).unwrap();
                            let key = ResultKey::new(
                                query.line.as_bytes(),
                                &prepared,
                                &forward,
                                &reverse,
                            );

                            if let Some(existing) = by_result.get(&result) {
                                assert!(*existing == key, "line={line:?}, result={result:?}");
                            }
                            if let Some(existing) = by_key.get(&key) {
                                assert_eq!(existing, &result, "line={line:?}, result={result:?}");
                            }
                            by_result
                                .entry(result.clone())
                                .or_insert_with(|| key.clone());
                            by_key.entry(key).or_insert(result);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn invalid_edits_are_removed_before_any_ordering_key_is_built() {
        let query = CompletionQuery::new("é", 2, Path::new("/tmp"), 1).unwrap();
        let invalid = [1..2, std::ops::Range { start: 2, end: 1 }, 0..3]
            .into_iter()
            .enumerate()
            .map(|(index, range)| {
                Suggestion::new(
                    TextEdit {
                        range,
                        replacement: "bad".into(),
                    },
                    "bad",
                    "",
                    "command",
                    SuggestionSource::Spec,
                    InsertionBehavior::Exact,
                    format!("invalid-{index}"),
                )
            });
        let valid = Suggestion::new(
            TextEdit {
                range: 0..2,
                replacement: "ok".into(),
            },
            "ok",
            "",
            "command",
            SuggestionSource::Spec,
            InsertionBehavior::Exact,
            "valid",
        );
        let candidates = invalid.chain([valid]).collect();

        let merged = merge_suggestions(&query, [candidates]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].identity(), "valid");
    }

    #[test]
    fn canonical_key_storage_is_bounded_by_changed_candidate_bytes() {
        const QUERY_BYTES: usize = 256 * 1024;
        const CANDIDATES: usize = 4_096;

        let prefix = "x".repeat(QUERY_BYTES - 1);
        let line = format!("{prefix}q");
        let query = CompletionQuery::new(&line, line.len(), Path::new("/tmp"), 1).unwrap();
        let forward = LceIndex::new(query.line.as_bytes());
        let reversed = query.line.bytes().rev().collect::<Vec<_>>();
        let reverse = LceIndex::new(&reversed);
        let mut changed_bytes = 0usize;
        let mut replacement_bytes = 0usize;
        let mut candidates = Vec::with_capacity(CANDIDATES);

        for index in 0..CANDIDATES {
            let candidate = Suggestion::new(
                TextEdit {
                    range: prefix.len()..line.len(),
                    replacement: format!("value-{index:04}"),
                },
                format!("value-{index:04}"),
                "",
                "command",
                SuggestionSource::Spec,
                InsertionBehavior::Exact,
                format!("candidate-{index:04}"),
            );
            let prepared = PreparedCandidate::new(&query, candidate.clone()).unwrap();
            let key = ResultKey::new(query.line.as_bytes(), &prepared, &forward, &reverse);
            changed_bytes += key.changed.len();
            replacement_bytes += prepared.resolved_replacement.len();
            candidates.push(candidate);
        }

        assert!(changed_bytes <= replacement_bytes);
        assert!(changed_bytes < QUERY_BYTES);
        assert!(QUERY_BYTES.saturating_mul(CANDIDATES) >= 1024 * 1024 * 1024);

        let merged = merge_suggestions(&query, [candidates]);
        assert_eq!(merged.len(), CANDIDATES);
        assert_eq!(merged.capacity(), merged.len());
    }
}
