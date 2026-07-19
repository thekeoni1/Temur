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
}
