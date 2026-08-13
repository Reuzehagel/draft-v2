// Find/replace stage — deterministic, instant, works with every provider.
// This is the local layer of issue #2 (native provider biasing lives in the
// transcribers instead). It also covers snippet expansion ("my email" -> an
// address), which no provider-side biasing does.

use crate::config::Replacement;

/// Apply one rule across `text`, returning the rewritten string. Matching is
/// non-overlapping and left-to-right: once a match is consumed the scan
/// resumes after the inserted replacement, so a rule can't rewrite its own
/// output.
pub fn apply_replacement(text: &str, rule: &Replacement) -> String {
    if rule.from.is_empty() {
        return text.to_owned();
    }

    // Char vectors so word-boundary checks and case folding are Unicode-aware
    // for the haystack while index math stays simple. Transcripts are short,
    // so the allocation is cheap.
    let hay: Vec<char> = text.chars().collect();
    let needle: Vec<char> = rule.from.chars().collect();

    let matches_at = |i: usize| -> bool {
        if i + needle.len() > hay.len() {
            return false;
        }
        let chars_match =
            (0..needle.len()).all(|k| char_eq(hay[i + k], needle[k], rule.case_sensitive));
        if !chars_match {
            return false;
        }
        if rule.whole_word {
            let left_ok = i == 0 || !is_word(hay[i - 1]);
            let right_end = i + needle.len();
            let right_ok = right_end == hay.len() || !is_word(hay[right_end]);
            left_ok && right_ok
        } else {
            true
        }
    };

    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < hay.len() {
        if matches_at(i) {
            out.push_str(&rule.to);
            i += needle.len();
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    out
}

fn char_eq(a: char, b: char, case_sensitive: bool) -> bool {
    if case_sensitive {
        a == b
    } else {
        a.eq_ignore_ascii_case(&b)
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(from: &str, to: &str, whole_word: bool, case_sensitive: bool) -> Replacement {
        Replacement {
            from: from.into(),
            to: to.into(),
            whole_word,
            case_sensitive,
            enabled: true,
        }
    }

    #[test]
    fn plain_substring_replace() {
        let r = rule("teh", "the", false, false);
        assert_eq!(apply_replacement("teh cat", &r), "the cat");
    }

    #[test]
    fn case_insensitive_matches_but_inserts_verbatim() {
        let r = rule("github", "GitHub", false, false);
        assert_eq!(
            apply_replacement("i use Github daily", &r),
            "i use GitHub daily"
        );
    }

    #[test]
    fn case_sensitive_respects_case() {
        let r = rule("api", "API", true, true);
        // "API" already cased shouldn't be touched; "api" should.
        assert_eq!(
            apply_replacement("the api and the API", &r),
            "the API and the API"
        );
    }

    #[test]
    fn whole_word_does_not_fire_mid_word() {
        let r = rule("a row", "arrow", true, false);
        assert_eq!(
            apply_replacement("draw a row of arrows", &r),
            "draw arrow of arrows"
        );
    }

    #[test]
    fn whole_word_boundary_at_string_edges() {
        let r = rule("cat", "dog", true, false);
        assert_eq!(apply_replacement("cat", &r), "dog");
        assert_eq!(apply_replacement("category", &r), "category");
    }

    #[test]
    fn snippet_expansion_phrase() {
        let r = rule("my email", "me@example.com", false, false);
        assert_eq!(
            apply_replacement("send it to my email please", &r),
            "send it to me@example.com please"
        );
    }

    #[test]
    fn replacement_is_not_rescanned() {
        // "a" -> "aa" must not loop forever or cascade into the new text.
        let r = rule("a", "aa", false, false);
        assert_eq!(apply_replacement("a b a", &r), "aa b aa");
    }

    #[test]
    fn empty_from_is_noop() {
        let r = rule("", "x", false, false);
        assert_eq!(apply_replacement("unchanged", &r), "unchanged");
    }

    #[test]
    fn delete_via_empty_to() {
        let r = rule("um ", "", false, false);
        assert_eq!(apply_replacement("um well um yes", &r), "well yes");
    }

    #[test]
    fn unicode_haystack_is_safe() {
        let r = rule("cafe", "café", false, false);
        assert_eq!(
            apply_replacement("a cafe in Zürich", &r),
            "a café in Zürich"
        );
    }
}
