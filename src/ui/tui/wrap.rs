//! Width-aware text helpers for the transcript. Greedy word wrap over
//! display cells (unicode-width), deterministic so rendered frames can be
//! snapshot-tested. Widths are display columns and fit `usize` fine even on
//! 32-bit — these are on-screen strings, never file-sized data.

use unicode_width::UnicodeWidthChar;

fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

pub fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// Greedy word wrap at `width` display columns. Embedded newlines are hard
/// breaks; words wider than the line are split mid-word; `width == 0` is
/// treated as 1 so this can never loop.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for raw_line in text.split('\n') {
        let mut line = String::new();
        let mut line_w = 0usize;
        let mut word = String::new();
        let mut word_w = 0usize;
        let flush_word = |line: &mut String,
                          line_w: &mut usize,
                          word: &mut String,
                          word_w: &mut usize,
                          out: &mut Vec<String>| {
            if word.is_empty() {
                return;
            }
            let sep = if line.is_empty() { 0 } else { 1 };
            if *line_w + sep + *word_w <= width {
                if sep == 1 {
                    line.push(' ');
                    *line_w += 1;
                }
                line.push_str(word);
                *line_w += *word_w;
            } else if *word_w <= width {
                out.push(std::mem::take(line));
                *line_w = 0;
                line.push_str(word);
                *line_w = *word_w;
            } else {
                // Word wider than a line: hard-split it.
                for c in word.chars() {
                    let cw = char_width(c);
                    let sep = if line.is_empty() { 0 } else { 1 };
                    // Continue the current line only if the fragment starts there.
                    let _ = sep;
                    if *line_w + cw > width && !line.is_empty() {
                        out.push(std::mem::take(line));
                        *line_w = 0;
                    }
                    line.push(c);
                    *line_w += cw;
                }
            }
            word.clear();
            *word_w = 0;
        };
        for c in raw_line.chars() {
            if c == ' ' {
                flush_word(&mut line, &mut line_w, &mut word, &mut word_w, &mut out);
            } else {
                word.push(c);
                word_w += char_width(c);
            }
        }
        flush_word(&mut line, &mut line_w, &mut word, &mut word_w, &mut out);
        out.push(line);
    }
    out
}

/// `wrap` for styled runs (T8-P2, markdown): same greedy word semantics
/// and display-cell widths, but each character carries a tag (the caller
/// passes `ratatui::Style`; tests use plain chars) that survives wrap
/// points. Words may span run boundaries; adjacent same-tag output is
/// merged. `\n` is a hard break; overlong words hard-split; `width == 0`
/// is treated as 1. Inter-word spaces keep the tag of the input space
/// that separated the words (so emphasis spanning several words styles
/// its spaces too).
pub fn wrap_spans<T: Copy + PartialEq>(runs: &[(String, T)], width: usize) -> Vec<Vec<(String, T)>> {
    let width = width.max(1);
    let mut out: Vec<Vec<(String, T)>> = Vec::new();
    let mut line: Vec<(String, T)> = Vec::new();
    let mut line_w = 0usize;
    let mut word: Vec<(char, T)> = Vec::new();
    let mut word_w = 0usize;
    let mut space_tag: Option<T> = None;

    fn push_seg<T: Copy + PartialEq>(line: &mut Vec<(String, T)>, c: char, tag: T) {
        match line.last_mut() {
            Some((s, t)) if *t == tag => s.push(c),
            _ => line.push((c.to_string(), tag)),
        }
    }

    let flush_word = |line: &mut Vec<(String, T)>,
                      line_w: &mut usize,
                      word: &mut Vec<(char, T)>,
                      word_w: &mut usize,
                      space_tag: &Option<T>,
                      out: &mut Vec<Vec<(String, T)>>| {
        if word.is_empty() {
            return;
        }
        let sep = if line.is_empty() { 0 } else { 1 };
        if *line_w + sep + *word_w <= width {
            if sep == 1 {
                push_seg(line, ' ', space_tag.unwrap_or(word[0].1));
                *line_w += 1;
            }
            for (c, t) in word.iter() {
                push_seg(line, *c, *t);
            }
            *line_w += *word_w;
        } else if *word_w <= width {
            out.push(std::mem::take(line));
            *line_w = 0;
            for (c, t) in word.iter() {
                push_seg(line, *c, *t);
            }
            *line_w = *word_w;
        } else {
            // Word wider than a line: hard-split it (mirrors `wrap`).
            for (c, t) in word.iter() {
                let cw = char_width(*c);
                if *line_w + cw > width && !line.is_empty() {
                    out.push(std::mem::take(line));
                    *line_w = 0;
                }
                push_seg(line, *c, *t);
                *line_w += cw;
            }
        }
        word.clear();
        *word_w = 0;
    };

    for (text, tag) in runs {
        for c in text.chars() {
            match c {
                ' ' => {
                    flush_word(&mut line, &mut line_w, &mut word, &mut word_w, &space_tag, &mut out);
                    space_tag = Some(*tag);
                }
                '\n' => {
                    flush_word(&mut line, &mut line_w, &mut word, &mut word_w, &space_tag, &mut out);
                    out.push(std::mem::take(&mut line));
                    line_w = 0;
                    space_tag = None;
                }
                _ => {
                    word.push((c, *tag));
                    word_w += char_width(c);
                }
            }
        }
    }
    flush_word(&mut line, &mut line_w, &mut word, &mut word_w, &space_tag, &mut out);
    out.push(line);
    out
}

/// Truncate to `width` display columns, appending `…` when cut (single-line
/// tool titles and the header). Also flattens newlines to spaces.
pub fn truncate_width(s: &str, width: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    if display_width(&flat) <= width {
        return flat;
    }
    let budget = width.saturating_sub(1);
    let mut out = String::new();
    let mut w = 0usize;
    for c in flat.chars() {
        let cw = char_width(c);
        if w + cw > budget {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_at_word_boundaries() {
        assert_eq!(wrap("the quick brown fox", 10), vec!["the quick", "brown fox"]);
    }

    #[test]
    fn preserves_hard_breaks_and_empty_lines() {
        assert_eq!(wrap("a\n\nb", 10), vec!["a", "", "b"]);
    }

    #[test]
    fn splits_overlong_words() {
        assert_eq!(wrap("abcdefgh", 3), vec!["abc", "def", "gh"]);
    }

    #[test]
    fn zero_width_is_safe() {
        assert_eq!(wrap("ab", 0), vec!["a", "b"]);
    }

    #[test]
    fn truncates_with_ellipsis() {
        assert_eq!(truncate_width("hello world", 8), "hello w…");
        assert_eq!(truncate_width("short", 8), "short");
        assert_eq!(truncate_width("two\nlines", 20), "two lines");
    }

    #[test]
    fn wide_chars_count_double() {
        // CJK chars are 2 columns wide.
        assert_eq!(wrap("你好 世界", 4), vec!["你好", "世界"]);
        assert_eq!(truncate_width("你好世界", 5), "你好…");
    }

    // ------------------------------------------------- wrap_spans (T8-P2)

    fn runs(v: &[(&str, char)]) -> Vec<(String, char)> {
        v.iter().map(|(s, t)| (s.to_string(), *t)).collect()
    }

    /// Flatten one wrapped line back to plain text for content asserts.
    fn text_of(line: &[(String, char)]) -> String {
        line.iter().map(|(s, _)| s.as_str()).collect()
    }

    #[test]
    fn spans_match_plain_wrap_on_uniform_style() {
        let text = "the quick brown fox jumps over the lazy dog";
        let styled = wrap_spans(&runs(&[(text, 'a')]), 10);
        assert_eq!(
            styled.iter().map(|l| text_of(l)).collect::<Vec<_>>(),
            wrap(text, 10)
        );
        // Uniform style stays one merged segment per line.
        assert!(styled.iter().all(|l| l.len() == 1 && l[0].1 == 'a'));
    }

    #[test]
    fn spans_styles_survive_wrap_points() {
        // "plain BOLD plain" wrapped so the bold word lands mid-output.
        let r = runs(&[("one ", 'p'), ("bold", 'b'), (" three four", 'p')]);
        let lines = wrap_spans(&r, 8);
        assert_eq!(
            lines.iter().map(|l| text_of(l)).collect::<Vec<_>>(),
            vec!["one bold", "three", "four"]
        );
        // The bold run is tagged 'b' wherever it ended up.
        let bold: Vec<&(String, char)> =
            lines.iter().flatten().filter(|(_, t)| *t == 'b').collect();
        assert_eq!(bold.len(), 1);
        assert_eq!(bold[0].0, "bold");
    }

    #[test]
    fn spans_word_split_across_runs_stays_one_word() {
        // One word whose halves carry different tags must not wrap between
        // the halves.
        let r = runs(&[("aaa", 'x'), ("bbb", 'y'), (" cc", 'p')]);
        let lines = wrap_spans(&r, 6);
        assert_eq!(
            lines.iter().map(|l| text_of(l)).collect::<Vec<_>>(),
            vec!["aaabbb", "cc"]
        );
        assert_eq!(lines[0], runs(&[("aaa", 'x'), ("bbb", 'y')]));
    }

    #[test]
    fn spans_interword_space_keeps_input_space_tag() {
        // Emphasis spanning several words styles its inner space too…
        let r = runs(&[("two words", 'i'), (" plain", 'p')]);
        let lines = wrap_spans(&r, 30);
        assert_eq!(lines[0][0], ("two words".to_string(), 'i'));
        // …and the boundary space carries the plain run's tag.
        assert_eq!(lines[0][1], (" plain".to_string(), 'p'));
    }

    #[test]
    fn spans_hard_breaks_and_overlong_words() {
        let r = runs(&[("ab\ncdefgh", 'a')]);
        assert_eq!(
            wrap_spans(&r, 3).iter().map(|l| text_of(l)).collect::<Vec<_>>(),
            vec!["ab", "cde", "fgh"]
        );
        // Overlong word split across style boundary keeps both tags.
        let r = runs(&[("abc", 'x'), ("def", 'y')]);
        let lines = wrap_spans(&r, 4);
        assert_eq!(lines[0], runs(&[("abc", 'x'), ("d", 'y')]));
        assert_eq!(lines[1], runs(&[("ef", 'y')]));
    }

    #[test]
    fn spans_wide_chars_and_tiny_widths_are_safe() {
        let r = runs(&[("你好 世界", 'w')]);
        assert_eq!(
            wrap_spans(&r, 4).iter().map(|l| text_of(l)).collect::<Vec<_>>(),
            vec!["你好", "世界"]
        );
        for w in 0..=3 {
            let lines = wrap_spans(&runs(&[("你好x yz", 'w')]), w);
            let joined: String = lines.iter().map(|l| text_of(l)).collect();
            assert!(joined.contains('x') && joined.contains('z'), "width {w}");
        }
        assert_eq!(wrap_spans(&runs(&[]), 10), vec![Vec::new()]);
    }
}
