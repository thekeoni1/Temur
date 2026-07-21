//! Fuzzy matchers for the edit tool (T6), ported from OpenCode's
//! whitespace-tolerant (line-trimmed) and block-anchor matchers onto byte
//! ranges, with one deliberate divergence: within a matcher, two or more
//! candidates are an ERROR demanding more context — OpenCode can silently
//! pick one; we never guess. No Levenshtein similarity scoring at all:
//! deterministic anchors only (less code, no O(n²) passes on 32-bit).
//!
//! Everything here is pure: (content, old_string) → candidate byte ranges
//! of `content`. The edit tool owns file I/O, error wording, and the
//! exact-match-first policy; these functions are only consulted when an
//! exact search found nothing.

use std::ops::Range;

/// Which fallback matched — the tool marks its output with this so a fuzzy
/// edit is never mistaken for an exact one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matcher {
    LineTrimmed,
    BlockAnchor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FuzzyResult {
    NoMatch,
    Unique { range: Range<usize>, matcher: Matcher },
    Ambiguous { count: usize },
}

/// The fallback pipeline: line-trimmed first, block-anchor only if the
/// stricter matcher found NOTHING (never to disambiguate — ambiguity is
/// final at the matcher that found it).
pub fn fuzzy_match(content: &str, old: &str) -> FuzzyResult {
    let candidates = line_trimmed(content, old);
    match candidates.len() {
        1 => {
            return FuzzyResult::Unique {
                range: candidates.into_iter().next().unwrap(),
                matcher: Matcher::LineTrimmed,
            }
        }
        n if n >= 2 => return FuzzyResult::Ambiguous { count: n },
        _ => {}
    }
    let candidates = block_anchor(content, old);
    match candidates.len() {
        1 => FuzzyResult::Unique {
            range: candidates.into_iter().next().unwrap(),
            matcher: Matcher::BlockAnchor,
        },
        0 => FuzzyResult::NoMatch,
        n => FuzzyResult::Ambiguous { count: n },
    }
}

/// Byte spans of each line's content, EXCLUDING the `\n` terminator (a
/// CRLF file's `\r` stays inside the span; `trim()` neutralizes it during
/// comparison). Split points are `b'\n'` positions — always char
/// boundaries in UTF-8, so every span is safe to slice.
fn line_spans(content: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            spans.push((start, i));
            start = i + 1;
        }
    }
    spans.push((start, content.len()));
    spans
}

/// `old` split into lines for matching. A trailing `\n` (empty last piece)
/// is dropped and reported separately: it means "the match extends through
/// the final line's terminator".
fn old_lines(old: &str) -> (Vec<&str>, bool) {
    let mut lines: Vec<&str> = old.split('\n').collect();
    let trailing_newline = lines.len() > 1 && lines.last() == Some(&"");
    if trailing_newline {
        lines.pop();
    }
    (lines, trailing_newline)
}

/// The byte range for matched content lines `first..=last`.
///
/// Without a trailing newline in `old`, the final line's `\r` (CRLF files)
/// is EXCLUDED so the file's own terminator survives the splice; with one,
/// the range extends through `\r\n`/`\n` entirely (a CRLF-converted
/// replacement then re-supplies the full terminator).
fn range_for(
    content: &str,
    spans: &[(usize, usize)],
    first: usize,
    last: usize,
    trailing_newline: bool,
) -> Range<usize> {
    let start = spans[first].0;
    let mut end = spans[last].1;
    if trailing_newline {
        if end < content.len() {
            end += 1; // through the \n (any \r is already inside the span)
        }
    } else if content[..end].ends_with('\r') {
        end -= 1; // leave the CRLF terminator to the file
    }
    start..end
}

/// Whitespace-tolerant matcher: every line of `old` equals the
/// corresponding content line after `trim()` — indentation and line-edge
/// whitespace differences are forgiven, interior differences are not.
pub fn line_trimmed(content: &str, old: &str) -> Vec<Range<usize>> {
    let spans = line_spans(content);
    let (lines, trailing_newline) = old_lines(old);
    if lines.is_empty() || spans.len() < lines.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 0..=(spans.len() - lines.len()) {
        let all = lines.iter().enumerate().all(|(j, l)| {
            let (s, e) = spans[i + j];
            content[s..e].trim() == l.trim()
        });
        if all {
            out.push(range_for(
                content,
                &spans,
                i,
                i + lines.len() - 1,
                trailing_newline,
            ));
        }
    }
    out
}

/// Block-anchor matcher, for `old` blocks of >= 3 lines: the trimmed first
/// and last lines are anchors; a candidate is a first-anchor line paired
/// with the NEAREST closing anchor at least two lines below (so the middle
/// may differ — even in line count). A single candidate is accepted on the
/// anchors alone (OpenCode ships a 0.0 single-candidate threshold — its
/// middle-similarity score never rejects one either).
pub fn block_anchor(content: &str, old: &str) -> Vec<Range<usize>> {
    let spans = line_spans(content);
    let (lines, trailing_newline) = old_lines(old);
    if lines.len() < 3 {
        return Vec::new();
    }
    let first = lines[0].trim();
    let last = lines[lines.len() - 1].trim();
    let mut out = Vec::new();
    for i in 0..spans.len() {
        let (s, e) = spans[i];
        if content[s..e].trim() != first {
            continue;
        }
        for j in (i + 2)..spans.len() {
            let (s2, e2) = spans[j];
            if content[s2..e2].trim() == last {
                out.push(range_for(content, &spans, i, j, trailing_newline));
                break; // nearest closing anchor only
            }
        }
    }
    out
}

/// True if the file uses CRLF line endings.
pub fn is_crlf(content: &str) -> bool {
    content.contains("\r\n")
}

/// Convert lone `\n` to `\r\n` (already-CRLF pairs are left alone).
pub fn to_crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apply a matched range like the tool will: splice `new` over it.
    fn splice(content: &str, range: Range<usize>, new: &str) -> String {
        format!("{}{}{}", &content[..range.start], new, &content[range.end..])
    }

    #[track_caller]
    fn unique(content: &str, old: &str) -> (Range<usize>, Matcher) {
        match fuzzy_match(content, old) {
            FuzzyResult::Unique { range, matcher } => (range, matcher),
            other => panic!("expected Unique for {old:?}, got {other:?}"),
        }
    }

    #[test]
    fn line_trimmed_forgives_indentation_and_splices_verbatim() {
        let content = "fn main() {\n\tlet x = 1;\n}\n";
        let (range, m) = unique(content, "    let x = 1;");
        assert_eq!(m, Matcher::LineTrimmed);
        // The replacement is spliced VERBATIM — the model's indentation
        // wins inside the block.
        assert_eq!(
            splice(content, range, "    let y = 2;"),
            "fn main() {\n    let y = 2;\n}\n"
        );
    }

    #[test]
    fn interior_whitespace_still_mismatches() {
        // trim() only forgives line-EDGE whitespace: tab-vs-space inside a
        // line stays a miss (the documented T6 boundary — no
        // WhitespaceNormalized matcher).
        let content = "x\nfoo\tbar\ny\n";
        assert_eq!(fuzzy_match(content, "foo bar"), FuzzyResult::NoMatch);
    }

    #[test]
    fn crlf_file_lf_old_string_matches_and_keeps_terminator() {
        let content = "a\r\nfoo\r\nb\r\n";
        let (range, _) = unique(content, "foo");
        // The final line's \r is EXCLUDED: the file's own CRLF terminator
        // survives an LF-shaped replacement.
        assert_eq!(&content[range.clone()], "foo");
        assert_eq!(splice(content, range, "bar"), "a\r\nbar\r\nb\r\n");
    }

    #[test]
    fn crlf_file_trailing_newline_old_extends_through_crlf() {
        let content = "a\r\nfoo\r\nb\r\n";
        let (range, _) = unique(content, "foo\n");
        assert_eq!(&content[range.clone()], "foo\r\n");
        // The tool CRLF-converts the replacement on this path.
        assert_eq!(splice(content, range, &to_crlf("bar\n")), "a\r\nbar\r\nb\r\n");
    }

    #[test]
    fn trailing_newline_old_consumes_the_terminator() {
        let content = "x\na\nb\nc\n";
        let (range, _) = unique(content, "a\nb\n");
        assert_eq!(&content[range.clone()], "a\nb\n");
        assert_eq!(splice(content, range, "Q\n"), "x\nQ\nc\n");
    }

    #[test]
    fn match_at_eof_without_trailing_newline() {
        let content = "a\nb\nc";
        let (range, _) = unique(content, "b\nc");
        assert_eq!(range.end, content.len());
        assert_eq!(splice(content, range, "B\nC"), "a\nB\nC");
    }

    #[test]
    fn trailing_newline_old_at_eof_without_one_still_matches() {
        let content = "a\nb";
        let (range, _) = unique(content, "  b\n");
        assert_eq!(splice(content, range, "B\n"), "a\nB\n");
    }

    #[test]
    fn match_at_file_start() {
        let content = "a\nb";
        let (range, _) = unique(content, "  a");
        assert_eq!(range.start, 0);
        assert_eq!(splice(content, range, "A"), "A\nb");
    }

    #[test]
    fn unicode_around_and_inside_the_match() {
        let content = "α\n\tβγ\nδ\n";
        let (range, _) = unique(content, "βγ");
        assert_eq!(splice(content, range, "χ"), "α\nχ\nδ\n");
    }

    #[test]
    fn ambiguous_line_trimmed_counts_candidates() {
        let content = "a\nx\na\n";
        assert_eq!(
            fuzzy_match(content, "  a"),
            FuzzyResult::Ambiguous { count: 2 }
        );
    }

    #[test]
    fn old_longer_than_file_is_no_match_without_panic() {
        assert_eq!(fuzzy_match("a\nb", "a\nb\nc\nd"), FuzzyResult::NoMatch);
    }

    #[test]
    fn whitespace_only_middle_lines_match_empty_lines() {
        let content = "a\n   \nb\n";
        let (range, _) = unique(content, "a\n\nb");
        assert_eq!(splice(content, range, "A\n\nB"), "A\n\nB\n");
    }

    #[test]
    fn block_anchor_requires_three_lines() {
        assert!(block_anchor("a\nX\nb\n", "a\nb").is_empty());
        assert!(block_anchor("a\nb\n", "a").is_empty());
    }

    #[test]
    fn block_anchor_accepts_single_candidate_with_mangled_middle() {
        let content = "start\n  middle_actual\nend\n";
        let (range, m) = unique(content, "start\nTOTALLY DIFFERENT\nend");
        assert_eq!(m, Matcher::BlockAnchor);
        assert_eq!(&content[range], "start\n  middle_actual\nend");
    }

    #[test]
    fn block_anchor_tolerates_different_block_lengths() {
        // Actual block longer than the search block…
        let content = "start\nm1\nm2\nm3\nend\n";
        let (range, m) = unique(content, "start\nmm\nend");
        assert_eq!(m, Matcher::BlockAnchor);
        assert_eq!(&content[range], "start\nm1\nm2\nm3\nend");
        // …and shorter.
        let content = "start\nm\nend\n";
        let (range, _) = unique(content, "start\na\nb\nc\nend");
        assert_eq!(&content[range], "start\nm\nend");
    }

    #[test]
    fn block_anchor_uses_nearest_closing_anchor() {
        let content = "if {\n a\n}\nmore\n}\n";
        let (range, _) = unique(content, "if {\nXX\n}");
        assert_eq!(&content[range], "if {\n a\n}");
    }

    #[test]
    fn block_anchor_same_pair_twice_is_ambiguous() {
        let content = "start\nm\nend\nstart\nz\nend\n";
        assert_eq!(
            fuzzy_match(content, "start\nq\nend"),
            FuzzyResult::Ambiguous { count: 2 }
        );
    }

    #[test]
    fn line_trimmed_wins_over_block_anchor() {
        let content = "fn f() {\n\tbody();\n}\n";
        let (_, m) = unique(content, "fn f() {\n    body();\n}");
        assert_eq!(m, Matcher::LineTrimmed);
    }

    #[test]
    fn crlf_detection_and_conversion() {
        assert!(is_crlf("a\r\nb"));
        assert!(!is_crlf("a\nb\rc")); // lone \r is not CRLF
        assert_eq!(to_crlf("a\nb\r\nc\n"), "a\r\nb\r\nc\r\n");
        assert_eq!(to_crlf("no newline"), "no newline");
    }
}
