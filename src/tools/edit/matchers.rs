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
/// final at the matcher that found it). Line spans and trimmed line
/// vectors are computed ONCE here and shared by both matchers.
pub fn fuzzy_match(content: &str, old: &str) -> FuzzyResult {
    let spans = line_spans(content);
    let trimmed: Vec<&str> = spans.iter().map(|&(s, e)| content[s..e].trim()).collect();
    let (lines, trailing_newline) = old_lines(old);
    let old_trimmed: Vec<&str> = lines.iter().map(|l| l.trim()).collect();

    let candidates = line_trimmed_impl(content, &spans, &trimmed, &old_trimmed, trailing_newline);
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
    let candidates = block_anchor_impl(content, &spans, &trimmed, &old_trimmed, trailing_newline);
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
    let trimmed: Vec<&str> = spans.iter().map(|&(s, e)| content[s..e].trim()).collect();
    let (lines, trailing_newline) = old_lines(old);
    let old_trimmed: Vec<&str> = lines.iter().map(|l| l.trim()).collect();
    line_trimmed_impl(content, &spans, &trimmed, &old_trimmed, trailing_newline)
}

fn line_trimmed_impl(
    content: &str,
    spans: &[(usize, usize)],
    trimmed: &[&str],
    old_trimmed: &[&str],
    trailing_newline: bool,
) -> Vec<Range<usize>> {
    if old_trimmed.is_empty() || spans.len() < old_trimmed.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 0..=(spans.len() - old_trimmed.len()) {
        let all = old_trimmed
            .iter()
            .enumerate()
            .all(|(j, l)| trimmed[i + j] == *l);
        if all {
            out.push(range_for(
                content,
                spans,
                i,
                i + old_trimmed.len() - 1,
                trailing_newline,
            ));
        }
    }
    out
}

/// Block-anchor matcher, for `old` blocks of >= 3 lines: the trimmed first
/// and last lines are anchors. For each first-anchor line, the closing
/// anchor is bound in two steps:
///
/// 1. If the line at the EXACT expected offset (`i + old_lines - 1`)
///    trimmed-matches the closing anchor, bind there — the common
///    weak-model case: same block shape, middle content differs.
/// 2. Otherwise fall back to the NEAREST closing anchor at least two lines
///    below, but ONLY if the candidate's middle passes a deterministic
///    similarity guard: at least half of the search block's middle lines
///    must appear trimmed-equal, order-preserving, in the candidate middle.
///    Without the guard a common closing line (`}`) could bind to an inner
///    brace or a foreign block and silently splice away real code.
///
/// Still zero Levenshtein; >= 2 surviving candidates remain an error.
pub fn block_anchor(content: &str, old: &str) -> Vec<Range<usize>> {
    let spans = line_spans(content);
    let trimmed: Vec<&str> = spans.iter().map(|&(s, e)| content[s..e].trim()).collect();
    let (lines, trailing_newline) = old_lines(old);
    let old_trimmed: Vec<&str> = lines.iter().map(|l| l.trim()).collect();
    block_anchor_impl(content, &spans, &trimmed, &old_trimmed, trailing_newline)
}

fn block_anchor_impl(
    content: &str,
    spans: &[(usize, usize)],
    trimmed: &[&str],
    old_trimmed: &[&str],
    trailing_newline: bool,
) -> Vec<Range<usize>> {
    if old_trimmed.len() < 3 {
        return Vec::new();
    }
    let first = old_trimmed[0];
    let last = *old_trimmed.last().unwrap();
    let middle = &old_trimmed[1..old_trimmed.len() - 1];
    let mut out = Vec::new();
    for i in 0..spans.len() {
        if trimmed[i] != first {
            continue;
        }
        let expected = i + old_trimmed.len() - 1; // >= i+2 (old has >= 3 lines)
        let close = if expected < spans.len() && trimmed[expected] == last {
            Some(expected)
        } else {
            ((i + 2)..spans.len())
                .find(|&j| trimmed[j] == last)
                .filter(|&j| middle_similar(middle, &trimmed[i + 1..j]))
        };
        if let Some(j) = close {
            out.push(range_for(content, spans, i, j, trailing_newline));
        }
    }
    out
}

/// The nearest-fallback similarity guard: the fraction of `search_middle`
/// lines that appear trimmed-equal, order-preserving (subsequence), in
/// `candidate_middle` must be >= 1/2.
fn middle_similar(search_middle: &[&str], candidate_middle: &[&str]) -> bool {
    let mut matched: usize = 0;
    let mut pos: usize = 0;
    for s in search_middle {
        if let Some(k) = candidate_middle[pos..].iter().position(|c| c == s) {
            matched += 1;
            pos += k + 1;
        }
    }
    matched * 2 >= search_middle.len()
}

/// Leading whitespace of a line (everything before the first
/// non-whitespace character; the whole line if it is blank).
fn leading_ws(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

/// Strip the longest common char suffix from a pair of leading-whitespace
/// strings, leaving only the differing prefixes — the per-line delta.
fn ws_delta<'a>(old_ws: &'a str, file_ws: &'a str) -> (&'a str, &'a str) {
    let mut o = old_ws.char_indices().rev();
    let mut f = file_ws.char_indices().rev();
    let (mut oi, mut fi) = (old_ws.len(), file_ws.len());
    loop {
        match (o.next(), f.next()) {
            (Some((io, co)), Some((jf, cf))) if co == cf => {
                oi = io;
                fi = jf;
            }
            _ => break,
        }
    }
    (&old_ws[..oi], &file_ws[..fi])
}

/// F3: uniform indentation-delta re-application for the line-trimmed path.
///
/// For every matched non-blank line pair, the leading-whitespace delta
/// (old line vs file line, longest common suffix removed) must be
/// IDENTICAL; that delta is then re-applied to each non-blank `new` line
/// that carries the old prefix (others are spliced verbatim). Returns the
/// adjusted replacement, or `None` when the delta is inconsistent across
/// lines — the caller rejects the candidate rather than guessing, because
/// splicing the model's indentation verbatim corrupts
/// indentation-significant blocks (nested Python was the review case).
pub fn reindent_replacement(
    content: &str,
    range: &Range<usize>,
    old: &str,
    new: &str,
) -> Option<String> {
    let (old_ls, _) = old_lines(old);
    let matched = &content[range.clone()];
    let mut delta: Option<(&str, &str)> = None;
    for (ol, fl) in old_ls.iter().zip(matched.split('\n')) {
        if ol.trim().is_empty() {
            continue; // blank lines carry no indentation signal
        }
        let d = ws_delta(leading_ws(ol), leading_ws(fl));
        match delta {
            None => delta = Some(d),
            Some(prev) if prev == d => {}
            Some(_) => return None, // inconsistent — reject the candidate
        }
    }
    match delta {
        None | Some(("", "")) => Some(new.to_string()), // no signal / no delta
        Some((from, to)) => {
            let adjusted: Vec<String> = new
                .split('\n')
                .map(|line| {
                    if !line.trim().is_empty() && line.starts_with(from) {
                        format!("{to}{}", &line[from.len()..])
                    } else {
                        line.to_string()
                    }
                })
                .collect();
            Some(adjusted.join("\n"))
        }
    }
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
        // The MATCHER splices verbatim — indentation correction is the
        // tool's job via reindent_replacement (F3), tested separately.
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
    fn block_anchor_tolerates_different_block_lengths_with_similar_middle() {
        // Actual block longer than the search block: the nearest-fallback
        // guard passes because the search middle (m1, m2) appears in order
        // in the candidate middle (2/2 >= 1/2)…
        let content = "start\nm1\nm2\nm3\nend\n";
        let (range, m) = unique(content, "start\nm1\nm2\nend");
        assert_eq!(m, Matcher::BlockAnchor);
        assert_eq!(&content[range], "start\nm1\nm2\nm3\nend");
        // …and shorter: half of (m, x) appears (1/2 >= 1/2).
        let content = "start\nm\nend\n";
        let (range, _) = unique(content, "start\nm\nx\nend");
        assert_eq!(&content[range], "start\nm\nend");
    }

    #[test]
    fn block_anchor_dissimilar_middle_refuses_length_mismatch() {
        // F1 regression (review scenario: nearest-anchor short splice).
        // Pre-fix, the nearest closing anchor was bound with NO middle
        // check, deleting real code on a length mismatch; now a fallback
        // candidate with a dissimilar middle is refused outright.
        let content = "start\nm1\nm2\nm3\nend\n";
        assert_eq!(fuzzy_match(content, "start\nmm\nend"), FuzzyResult::NoMatch);
        let content = "start\nm\nend\n";
        assert_eq!(
            fuzzy_match(content, "start\na\nb\nc\nend"),
            FuzzyResult::NoMatch
        );
    }

    #[test]
    fn block_anchor_inner_brace_does_not_bind() {
        // F1 regression (review scenario: inner-brace bind on `}`). The
        // nearest `}` after `fn a() {` is the IF's closing brace; binding
        // there would splice away tail() and the real closing brace.
        let content = "fn a() {\n    if x {\n        inner();\n    }\n    tail();\n}\n";
        assert_eq!(
            fuzzy_match(content, "fn a() {\n    body();\n}"),
            FuzzyResult::NoMatch
        );
    }

    #[test]
    fn block_anchor_prefers_exact_offset_over_nearer_anchor() {
        // Both line 2 and line 3 trimmed-match the closing anchor; the one
        // at the exact expected offset (same block shape) wins over the
        // nearer one.
        let content = "s\nx\ne\ne\n";
        let (range, m) = unique(content, "s\nq\nr\ne");
        assert_eq!(m, Matcher::BlockAnchor);
        assert_eq!(&content[range], "s\nx\ne\ne");
    }

    #[test]
    fn block_anchor_exact_offset_bind_ignores_farther_anchor() {
        // The expected-offset arm binds the block-shaped candidate; the
        // stray later `}` is never considered.
        let content = "if {\n a\n}\nmore\n}\n";
        let (range, _) = unique(content, "if {\nXX\n}");
        assert_eq!(&content[range], "if {\n a\n}");
    }

    #[test]
    fn block_anchor_exact_offset_disambiguates_repeated_anchors() {
        // Review scenario variant: two begin/end blocks; the search block
        // matches only the second by shape. Pre-fix the first `begin`
        // bound the nearest `end` (a mis-splice) and the result was a
        // spurious ambiguity; now the dissimilar first candidate is
        // refused and the true block matches uniquely.
        let content = "begin\na()\nend\nbegin\nb()\nc()\nd()\nend\n";
        // c2() keeps line-trimmed from matching; 2/3 middle lines match.
        let (range, m) = unique(content, "begin\nb()\nc2()\nd()\nend");
        assert_eq!(m, Matcher::BlockAnchor);
        assert_eq!(&content[range], "begin\nb()\nc()\nd()\nend");
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

    // ------------------------------------------------- F3: reindentation

    /// Match `old` via line-trimmed and return the reindented replacement.
    #[track_caller]
    fn reindent(content: &str, old: &str, new: &str) -> Option<String> {
        let (range, m) = unique(content, old);
        assert_eq!(m, Matcher::LineTrimmed);
        reindent_replacement(content, &range, old, new)
    }

    #[test]
    fn reindent_adds_uniform_missing_indent() {
        // Model wrote the block one nesting level shallower than the file
        // (4-vs-8 nested Python): the uniform +4 delta is re-applied.
        let content = "def f():\n        if cond:\n            do_a()\n";
        assert_eq!(
            reindent(content, "    if cond:\n        do_a()", "    if cond:\n        do_b()"),
            Some("        if cond:\n            do_b()".into())
        );
    }

    #[test]
    fn reindent_strips_uniform_extra_indent() {
        let content = "a()\nb()\nrest\n";
        assert_eq!(
            reindent(content, "  a()\n  b()", "  c()\n  d()"),
            Some("c()\nd()".into())
        );
    }

    #[test]
    fn reindent_swaps_tab_for_spaces() {
        // Wholesale style swap (file tabs, model spaces): the pair delta is
        // uniform, so new lines carrying the model prefix get the file's.
        let content = "fn main() {\n\tlet x = 1;\n}\n";
        assert_eq!(
            reindent(content, "    let x = 1;", "    let y = 2;"),
            Some("\tlet y = 2;".into())
        );
    }

    #[test]
    fn reindent_zero_delta_is_verbatim() {
        let content = "a\n  keep\nb\n";
        assert_eq!(reindent(content, "  keep", "  kept"), Some("  kept".into()));
    }

    #[test]
    fn reindent_inconsistent_delta_rejects() {
        // Line 1 delta is +1 space, line 2 delta is -1: no uniform rule
        // exists, so the candidate is rejected rather than guessed at.
        let content = "  aa\nbb\n";
        let (range, _) = unique(content, " aa\n bb");
        assert_eq!(reindent_replacement(content, &range, " aa\n bb", "x"), None);
    }

    #[test]
    fn reindent_blank_lines_carry_no_signal_and_stay_unprefixed() {
        let content = "    a\n\n    b\n";
        // Old's blank middle line is skipped for the delta; new's blank
        // line is not given trailing whitespace.
        assert_eq!(
            reindent(content, "a\n\nb", "c\n\nd"),
            Some("    c\n\n    d".into())
        );
    }

    #[test]
    fn reindent_leaves_nonmatching_new_lines_verbatim() {
        // Removal delta: new lines missing the old prefix are left alone
        // instead of being rejected (the model dedented below block base).
        let content = "a\nb\n";
        assert_eq!(reindent(content, "  a\n  b", "A\nB"), Some("A\nB".into()));
    }

    #[test]
    fn crlf_detection_and_conversion() {
        assert!(is_crlf("a\r\nb"));
        assert!(!is_crlf("a\nb\rc")); // lone \r is not CRLF
        assert_eq!(to_crlf("a\nb\r\nc\n"), "a\r\nb\r\nc\r\n");
        assert_eq!(to_crlf("no newline"), "no newline");
    }
}
