use std::collections::BTreeMap;

use super::{CompletionQuery, Suggestion};

/// Deduplicates candidates by their complete insertion result and merges metadata.
///
/// Invalid edits, blank candidates, and candidates identical to the normalized
/// current buffer are discarded. No UI limit is applied here: FR-029 requires
/// ranking before `ui.max-suggestions` truncation. Providers enforce their own
/// safety bounds before returning data.
#[must_use]
pub fn merge_suggestions(
    query: &CompletionQuery,
    batches: impl IntoIterator<Item = Vec<Suggestion>>,
) -> Vec<Suggestion> {
    let normalized_query = normalize_line(&query.line);
    let mut merged = BTreeMap::<String, Suggestion>::new();

    for candidate in batches.into_iter().flatten() {
        if candidate.display.trim().is_empty() {
            continue;
        }
        let Ok(result) = candidate.resulting_line(query) else {
            continue;
        };
        if result.trim().is_empty() || normalize_line(&result) == normalized_query {
            continue;
        }

        if let Some(existing) = merged.get_mut(&result) {
            existing.merge_metadata(candidate);
        } else {
            merged.insert(result, candidate);
        }
    }

    merged.into_values().collect()
}

fn normalize_line(line: &str) -> String {
    line.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::completion::{InsertionBehavior, SuggestionSource, TextEdit};

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
}
