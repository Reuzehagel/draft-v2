// Voice formatting commands stage — issue #3. Deterministic, instant,
// provider-agnostic: spoken phrases are parsed out of the transcript and
// turned into formatting before paste.
//
//   "new line"       -> "\n"
//   "new paragraph"  -> "\n\n" (next word capitalized)
//   "scratch that"   -> deletes the previous sentence/segment
//   "all caps X"     -> uppercases the next word
//
// Matching is explicit command words only (the simple end of the design in
// the issue). Wispr-style gating heuristics (trigger word + word-count
// reduction) can tighten this later if false positives bite in practice.

/// A whitespace-separated token. `raw` keeps the provider's punctuation
/// ("line," / "that."); `core` is the alphanumeric middle, lowercased, which
/// is what command words match against.
struct Tok<'a> {
    raw: &'a str,
    core: String,
}

/// What the output is built from: words keep their raw text, breaks are the
/// newlines a command inserted. Kept apart so joining can skip the space a
/// word separator would add around a break.
enum Piece {
    Word(String),
    Break(&'static str),
}

#[derive(Clone, Copy)]
enum Cmd {
    NewLine,
    NewParagraph,
    ScratchThat,
    AllCaps,
}

pub fn apply_commands(text: &str) -> String {
    let toks: Vec<Tok> = text
        .split_whitespace()
        .map(|raw| Tok {
            raw,
            core: raw
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase(),
        })
        .collect();

    let mut out: Vec<Piece> = Vec::with_capacity(toks.len());
    // True when the next word starts a paragraph and should be capitalized.
    let mut capitalize_next = false;
    let mut i = 0;
    while i < toks.len() {
        let Some((cmd, len)) = match_command(&toks, i) else {
            let mut word = toks[i].raw.to_owned();
            if capitalize_next {
                word = capitalize_first(&word);
                capitalize_next = false;
            }
            out.push(Piece::Word(word));
            i += 1;
            continue;
        };
        i += len;
        match cmd {
            Cmd::NewLine | Cmd::NewParagraph => {
                // The pause that triggered the command usually makes the
                // provider close the clause with a comma — drop it, the break
                // replaces it. Terminal punctuation (.!?) stays.
                if let Some(Piece::Word(w)) = out.last_mut() {
                    if w.ends_with(',') {
                        w.pop();
                    }
                }
                // A break with nothing before it would paste leading newlines.
                if !out.is_empty() {
                    out.push(Piece::Break(match cmd {
                        Cmd::NewLine => "\n",
                        _ => "\n\n",
                    }));
                }
                capitalize_next = matches!(cmd, Cmd::NewParagraph);
            }
            Cmd::ScratchThat => {
                // Delete the trailing segment: pop words until the piece
                // *behind* the removal is a boundary (a break, or a word that
                // ends a sentence). The boundary itself survives only if we
                // already removed something — "tomorrow. Scratch that." must
                // take "tomorrow." with it, not stop dead on it.
                let mut popped = 0usize;
                while let Some(last) = out.last() {
                    let boundary = match last {
                        Piece::Break(_) => true,
                        Piece::Word(w) => w.ends_with(['.', '!', '?']),
                    };
                    if boundary && popped > 0 {
                        break;
                    }
                    out.pop();
                    popped += 1;
                }
            }
            Cmd::AllCaps => {
                if i < toks.len() {
                    // Uppercasing the raw token keeps its punctuation intact
                    // (and subsumes any pending capitalization).
                    out.push(Piece::Word(toks[i].raw.to_uppercase()));
                    capitalize_next = false;
                    i += 1;
                }
            }
        }
    }

    let mut s = String::with_capacity(text.len());
    for piece in &out {
        match piece {
            Piece::Word(w) => {
                if !s.is_empty() && !s.ends_with('\n') {
                    s.push(' ');
                }
                s.push_str(w);
            }
            Piece::Break(b) => {
                s.push_str(b);
            }
        }
    }
    // A scratch-that can leave a dangling break at the end.
    while s.ends_with('\n') {
        s.pop();
    }
    s
}

/// Try to match a command phrase starting at token `i`. Returns the command
/// and how many tokens it consumed. Inner tokens must be bare words (no
/// attached punctuation), so "brand new. Line two" can't fire; the last token
/// may carry trailing punctuation, which is consumed with the command.
fn match_command(toks: &[Tok], i: usize) -> Option<(Cmd, usize)> {
    const PHRASES: &[(&[&str], Cmd)] = &[
        (&["new", "line"], Cmd::NewLine),
        (&["new", "paragraph"], Cmd::NewParagraph),
        (&["scratch", "that"], Cmd::ScratchThat),
        (&["all", "caps"], Cmd::AllCaps),
    ];
    for (words, cmd) in PHRASES {
        if i + words.len() > toks.len() {
            continue;
        }
        let cores_match = words
            .iter()
            .enumerate()
            .all(|(k, w)| toks[i + k].core == *w);
        if !cores_match {
            continue;
        }
        // Every token but the last must be exactly its core (modulo case):
        // punctuation between the words means they weren't spoken as one phrase.
        let inner_bare = (0..words.len() - 1)
            .all(|k| toks[i + k].raw.len() == toks[i + k].core.len());
        if inner_bare {
            return Some((*cmd, words.len()));
        }
    }
    None
}

fn capitalize_first(w: &str) -> String {
    let mut chars = w.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_line_breaks_and_eats_comma() {
        assert_eq!(
            apply_commands("first point, new line, second point"),
            "first point\nsecond point"
        );
    }

    #[test]
    fn new_line_keeps_terminal_punctuation() {
        assert_eq!(
            apply_commands("First sentence. New line. Second sentence."),
            "First sentence.\nSecond sentence."
        );
    }

    #[test]
    fn new_paragraph_double_break_and_capitalizes() {
        assert_eq!(
            apply_commands("intro, new paragraph, the body starts here"),
            "intro\n\nThe body starts here"
        );
    }

    #[test]
    fn scratch_that_removes_previous_segment() {
        assert_eq!(
            apply_commands("Send the report tomorrow. Scratch that. Send it today."),
            "Send it today."
        );
    }

    #[test]
    fn scratch_that_stops_at_line_break() {
        assert_eq!(
            apply_commands("first point, new line, second point, scratch that"),
            "first point"
        );
    }

    #[test]
    fn scratch_that_alone_empties() {
        assert_eq!(apply_commands("scratch that"), "");
    }

    #[test]
    fn all_caps_uppercases_next_word() {
        assert_eq!(apply_commands("ship the all caps draft today"), "ship the DRAFT today");
    }

    #[test]
    fn all_caps_keeps_punctuation() {
        assert_eq!(apply_commands("it is urgent, all caps urgent."), "it is urgent, URGENT.");
    }

    #[test]
    fn all_caps_at_end_is_dropped() {
        assert_eq!(apply_commands("nothing follows all caps"), "nothing follows");
    }

    #[test]
    fn command_is_case_insensitive() {
        assert_eq!(apply_commands("one New Line two"), "one\ntwo");
    }

    #[test]
    fn punctuated_inner_word_does_not_fire() {
        assert_eq!(
            apply_commands("something brand new. Line two starts"),
            "something brand new. Line two starts"
        );
    }

    #[test]
    fn leading_break_is_suppressed() {
        assert_eq!(apply_commands("new line hello"), "hello");
    }

    #[test]
    fn consecutive_breaks_accumulate() {
        assert_eq!(apply_commands("a new line new line b"), "a\n\nb");
    }

    #[test]
    fn plain_text_is_untouched() {
        assert_eq!(
            apply_commands("just a normal sentence, nothing else."),
            "just a normal sentence, nothing else."
        );
    }
}
