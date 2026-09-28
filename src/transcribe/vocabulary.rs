// The **vocabulary hint**'s term list, before any Provider renders it.
//
// The user's `Config::vocabulary` is normalised once — trimmed, blanks
// dropped, exact duplicates collapsed — and cut to the first `MAX_TERMS` in the
// user's order. The cap is Draft's, not a Provider's, so switching Provider
// never changes which terms count; the Settings window counts with the same
// `unique_terms`, so its caption and the transcription path can't disagree.
//
// Each Provider then renders the list into its own field at construction
// (`transcribe::build`). A term a field can't carry goes through `carried`:
// skipped and logged, never sent malformed or truncated.

/// The most terms any Provider is handed, whichever it is.
pub const MAX_TERMS: usize = 100;

/// The user's list trimmed, without blanks, and with each exact duplicate
/// after its first occurrence dropped — in the user's order, uncapped.
pub fn unique_terms(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for term in list.iter().map(|t| t.trim()) {
        if !term.is_empty() && !out.iter().any(|t| t == term) {
            out.push(term.to_string());
        }
    }
    out
}

/// The term list every Provider is built from: `unique_terms`, capped at the
/// first `MAX_TERMS`.
pub fn hint_terms(list: &[String]) -> Vec<String> {
    let mut terms = unique_terms(list);
    terms.truncate(MAX_TERMS);
    terms
}

/// The terms `provider`'s field can carry. `unfit` names why a term can't go
/// (or `None` when it can); each term it rejects is logged and left out.
pub fn carried(
    provider: &str,
    terms: &[String],
    unfit: impl Fn(&str) -> Option<&'static str>,
) -> Vec<String> {
    terms
        .iter()
        .filter(|term| match unfit(term) {
            Some(reason) => {
                tracing::warn!(
                    provider,
                    term = term.as_str(),
                    reason,
                    "vocabulary term not sent"
                );
                false
            }
            None => true,
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(terms: &[&str]) -> Vec<String> {
        terms.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn terms_are_trimmed_and_blanks_dropped() {
        assert_eq!(
            hint_terms(&list(&["  Janssen ", "", "\t", "kubectl"])),
            ["Janssen", "kubectl"]
        );
    }

    #[test]
    fn a_duplicate_keeps_its_first_place() {
        assert_eq!(
            hint_terms(&list(&["Reson8", "egui", " Reson8", "Draft"])),
            ["Reson8", "egui", "Draft"]
        );
    }

    #[test]
    fn duplicates_are_exact_not_case_folded() {
        assert_eq!(hint_terms(&list(&["egui", "EGUI"])), ["egui", "EGUI"]);
    }

    #[test]
    fn the_first_hundred_are_kept_in_order() {
        let terms: Vec<String> = (0..101).map(|i| format!("term{i}")).collect();
        let kept = hint_terms(&terms);
        assert_eq!(kept.len(), MAX_TERMS);
        assert_eq!(kept.first().map(String::as_str), Some("term0"));
        assert_eq!(kept.last().map(String::as_str), Some("term99"));
    }

    #[test]
    fn a_duplicate_before_the_cap_takes_no_place() {
        // 101 lines, but the one at 50 repeats the first: all 100 unique fit.
        let mut terms: Vec<String> = (0..100).map(|i| format!("term{i}")).collect();
        terms.insert(50, "term0".into());
        let kept = hint_terms(&terms);
        assert_eq!(kept.len(), MAX_TERMS);
        assert_eq!(kept.last().map(String::as_str), Some("term99"));
        assert_eq!(unique_terms(&terms).len(), MAX_TERMS);
    }

    #[test]
    fn an_empty_list_is_empty() {
        assert!(hint_terms(&[]).is_empty());
    }

    #[test]
    fn carried_skips_what_the_field_cannot_take() {
        let kept = carried("test", &list(&["a,b", "ab", "c"]), |t| {
            t.contains(',').then_some("contains a comma")
        });
        assert_eq!(kept, ["ab", "c"]);
    }
}
