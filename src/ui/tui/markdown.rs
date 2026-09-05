//! Markdown → styled transcript lines for assistant prose (T8-P2).
//!
//! Pure function of (text, width): parses per call with pulldown-cmark
//! (CommonMark + strikethrough only — tables/footnotes/tasklists are NOT
//! enabled and render as the plain paragraphs pulldown emits without them)
//! and renders owned ratatui `Line`s. Monochrome contract: DIM/BOLD/ITALIC/
//! UNDERLINED modifiers plus cyan for inline code; no themes, no
//! backgrounds, no syntax highlighting.
//!
//! Streaming: the caller re-renders the accumulating cell string every
//! frame. pulldown-cmark closes everything at end-of-input, so an unclosed
//! fence renders as a code block until its closer arrives and unclosed
//! emphasis stays literal — both pinned by tests below.

use super::wrap::{display_width, wrap_spans};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Assistant prose keeps the transcript's 3-space base indent.
const INDENT: &str = "   ";
/// Code-block gutter, mirroring the block-tool form in `view.rs`.
const GUTTER: &str = "▌ ";

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// The parser options, shared by BOTH parses in this file: the render
/// parse below and the T45 LaTeX pass's offset-iterator parse in
/// `latex::substitute_source`.
///
/// ONE constant rather than two literals kept in step by hand, because the
/// LaTeX pass computes its exclusion BYTE RANGES from its own parse: if the
/// two option sets ever differ, the same source is divided into different
/// events, the protected ranges no longer describe the text the renderer
/// sees, and code inside a construct only one parser understands stops
/// being protected. `tables_and_latex_stay_in_sync` pins it.
const MD_OPTIONS: Options = Options::ENABLE_STRIKETHROUGH.union(Options::ENABLE_TABLES);

pub fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    // T45 (P1): the LaTeX pass runs on the SOURCE, before parsing, with
    // code regions excluded by byte range. See `latex::substitute_source`
    // for why it cannot run on Text events instead.
    let src = latex::substitute_source(text.trim_end());
    let mut r = Renderer::new(width);
    for ev in Parser::new_ext(&src, MD_OPTIONS) {
        r.event(ev);
    }
    r.finish()
}

struct ListState {
    /// `Some(next number)` for ordered lists, `None` for bullets.
    next: Option<u64>,
    /// Columns the current item's marker occupies (hanging indent).
    marker_w: usize,
}

struct CodeState {
    lang: String,
    buf: String,
}

struct Renderer {
    width: usize,
    out: Vec<Line<'static>>,
    /// Inline runs of the block currently being collected.
    inline: Vec<(String, Style)>,
    /// Blank-line separation is owed before the next block.
    need_sep: bool,
    // Inline style state as depth counters so nesting composes.
    bold: u32,
    italic: u32,
    strike: u32,
    underline: u32,
    autolink_dim: u32,
    /// Per open link/image: `Some(url)` appended dim at the end, `None`
    /// for bare autolinks (rendered dim, url not repeated).
    links: Vec<Option<String>>,
    heading: Option<HeadingLevel>,
    quote_depth: usize,
    lists: Vec<ListState>,
    /// Marker text pending for the current item's first block.
    pending_marker: Option<String>,
    code: Option<CodeState>,
    /// T47: cells of the table row being assembled, each already styled by
    /// the normal inline machinery. Emptied into one wrapped line per row.
    row: Vec<Vec<(String, Style)>>,
    /// Inside a table at all. Guards the row machinery from firing on the
    /// stray `TagEnd::TableCell` a malformed document could produce.
    in_table: bool,
}

impl Renderer {
    fn new(width: usize) -> Self {
        Renderer {
            width,
            out: Vec::new(),
            inline: Vec::new(),
            need_sep: false,
            bold: 0,
            italic: 0,
            strike: 0,
            underline: 0,
            autolink_dim: 0,
            links: Vec::new(),
            heading: None,
            quote_depth: 0,
            lists: Vec::new(),
            pending_marker: None,
            code: None,
            row: Vec::new(),
            in_table: false,
        }
    }

    /// Current inline style from the open tags.
    fn style(&self) -> Style {
        let mut s = Style::default();
        if self.bold > 0 {
            s = s.add_modifier(Modifier::BOLD);
        }
        if self.italic > 0 {
            s = s.add_modifier(Modifier::ITALIC);
        }
        if self.strike > 0 || self.autolink_dim > 0 {
            s = s.add_modifier(Modifier::DIM);
        }
        if self.underline > 0 {
            s = s.add_modifier(Modifier::UNDERLINED);
        }
        if let Some(level) = self.heading {
            s = s.add_modifier(Modifier::BOLD);
            if matches!(level, HeadingLevel::H1 | HeadingLevel::H2) {
                s = s.add_modifier(Modifier::UNDERLINED);
            }
        }
        s
    }

    /// Shared block prefix: base indent, then dim quote bars, then list
    /// nesting columns (2 per level past the first).
    fn base_prefix(&self) -> Vec<(String, Style)> {
        let mut p: Vec<(String, Style)> = vec![(INDENT.to_string(), Style::default())];
        for _ in 0..self.quote_depth {
            p.push(("│ ".to_string(), dim()));
        }
        if self.lists.len() > 1 {
            p.push(("  ".repeat(self.lists.len() - 1), Style::default()));
        }
        p
    }

    /// (first-line prefix, continuation prefix) for the block about to be
    /// emitted, consuming a pending item marker if one is owed.
    fn prefixes(&mut self) -> (Vec<(String, Style)>, Vec<(String, Style)>) {
        let mut first = self.base_prefix();
        let mut cont = first.clone();
        if let Some(list) = self.lists.last() {
            match self.pending_marker.take() {
                Some(marker) => {
                    cont.push((" ".repeat(display_width(&marker)), Style::default()));
                    first.push((marker, Style::default()));
                }
                None => {
                    let pad = (" ".repeat(list.marker_w), Style::default());
                    first.push(pad.clone());
                    cont.push(pad);
                }
            }
        }
        (first, cont)
    }

    fn prefix_width(prefix: &[(String, Style)]) -> usize {
        prefix.iter().map(|(s, _)| display_width(s)).sum()
    }

    fn to_line(prefix: &[(String, Style)], content: Vec<(String, Style)>) -> Line<'static> {
        let mut spans: Vec<Span<'static>> = prefix
            .iter()
            .map(|(s, st)| Span::styled(s.clone(), *st))
            .collect();
        spans.extend(content.into_iter().map(|(s, st)| Span::styled(s, st)));
        Line::from(spans)
    }

    /// Emit the owed blank separator line (quote bars stay continuous).
    fn sep(&mut self) {
        if !self.need_sep || self.out.is_empty() {
            return;
        }
        self.need_sep = false;
        if self.quote_depth > 0 {
            let bars = self.base_prefix();
            self.out.push(Self::to_line(&bars, Vec::new()));
        } else {
            self.out.push(Line::default());
        }
    }

    /// Wrap and emit the collected inline runs as one block.
    fn flush_inline(&mut self) {
        if self.inline.is_empty() && self.pending_marker.is_none() {
            return;
        }
        let spans = std::mem::take(&mut self.inline);
        let (first, cont) = self.prefixes();
        let budget = self.width.saturating_sub(Self::prefix_width(&first));
        for (i, l) in wrap_spans(&spans, budget).into_iter().enumerate() {
            let prefix = if i == 0 { &first } else { &cont };
            self.out.push(Self::to_line(prefix, l));
        }
        self.need_sep = true;
    }

    /// Emit a finished code block: dim gutter, optional dim language tag
    /// line, code lines VERBATIM (hard-split when overlong, never trimmed).
    fn flush_code(&mut self, code: CodeState) {
        let (mut first, _) = self.prefixes();
        first.push((GUTTER.to_string(), dim()));
        let budget = self.width.saturating_sub(Self::prefix_width(&first)).max(1);
        if !code.lang.is_empty() {
            self.out.push(Self::to_line(&first, vec![(code.lang.clone(), dim())]));
        }
        for raw in code.buf.trim_end_matches('\n').split('\n') {
            if raw.is_empty() {
                self.out.push(Self::to_line(&first, Vec::new()));
                continue;
            }
            for piece in hard_split(raw, budget) {
                self.out
                    .push(Self::to_line(&first, vec![(piece, Style::default())]));
            }
        }
        self.need_sep = true;
    }

    /// T47: emit one table row as ONE line, cells joined by a dim " | ".
    ///
    /// Deliberately no column widths, no alignment and no box drawing: the
    /// gap this closes is that a table used to arrive as a single
    /// run-together line of pipes, and vertical structure alone fixes that.
    /// Cell content has already been through the ordinary inline machinery,
    /// so code spans, emphasis and the T45 LaTeX pass work inside cells for
    /// free. The joined row then wraps like any other block.
    fn end_row(&mut self) {
        if self.row.is_empty() {
            return;
        }
        let cells = std::mem::take(&mut self.row);
        let mut spans: Vec<(String, Style)> = Vec::new();
        for (i, cell) in cells.into_iter().enumerate() {
            if i > 0 {
                spans.push((" | ".to_string(), dim()));
            }
            spans.extend(cell);
        }
        self.inline = spans;
        self.flush_inline();
        // Rows are consecutive lines, never separated by a blank one.
        self.need_sep = false;
    }

    fn push_text(&mut self, t: &str) {
        if let Some(code) = self.code.as_mut() {
            code.buf.push_str(t);
        } else {
            self.inline.push((t.to_string(), self.style()));
        }
    }

    fn event(&mut self, ev: Event) {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => self.sep(),
                Tag::Heading { level, .. } => {
                    self.sep();
                    self.heading = Some(level);
                }
                Tag::BlockQuote(_) => {
                    self.flush_inline();
                    self.sep();
                    self.quote_depth += 1;
                    self.need_sep = false;
                }
                Tag::CodeBlock(kind) => {
                    self.flush_inline();
                    self.sep();
                    let lang = match kind {
                        CodeBlockKind::Fenced(l) => {
                            l.split_whitespace().next().unwrap_or("").to_string()
                        }
                        CodeBlockKind::Indented => String::new(),
                    };
                    self.code = Some(CodeState { lang, buf: String::new() });
                }
                Tag::List(start) => {
                    self.flush_inline();
                    if self.lists.is_empty() {
                        self.sep();
                    }
                    self.lists.push(ListState { next: start, marker_w: 2 });
                }
                Tag::Item => {
                    self.flush_inline();
                    if let Some(list) = self.lists.last_mut() {
                        let marker = match list.next {
                            Some(n) => {
                                list.next = Some(n + 1);
                                format!("{n}. ")
                            }
                            None => "• ".to_string(),
                        };
                        list.marker_w = display_width(&marker);
                        self.pending_marker = Some(marker);
                    }
                }
                Tag::Emphasis => self.italic += 1,
                Tag::Strong => self.bold += 1,
                Tag::Strikethrough => self.strike += 1,
                Tag::Link { link_type, dest_url, .. } | Tag::Image { link_type, dest_url, .. } => {
                    if matches!(link_type, LinkType::Autolink | LinkType::Email) {
                        self.autolink_dim += 1;
                        self.links.push(None);
                    } else {
                        self.underline += 1;
                        self.links.push(Some(dest_url.to_string()));
                    }
                }
                Tag::HtmlBlock => self.sep(),
                // T47 tables. TableHead IS the header row (measured: no
                // TableRow is nested inside it), so both it and TableRow
                // close a row. The header's bold falls out of the existing
                // depth counter, which is the whole of the "header styling"
                // here: no separate style path exists to go wrong.
                Tag::Table(_) => {
                    self.sep();
                    self.in_table = true;
                }
                Tag::TableHead => self.bold += 1,
                Tag::TableRow | Tag::TableCell => {}
                // Extensions that are off (tables, footnotes, definition
                // lists, metadata, super/subscript) never start; ignore.
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => self.flush_inline(),
                TagEnd::Heading(_) => {
                    self.flush_inline();
                    self.heading = None;
                }
                TagEnd::BlockQuote(_) => {
                    self.flush_inline();
                    self.quote_depth = self.quote_depth.saturating_sub(1);
                }
                TagEnd::CodeBlock => {
                    if let Some(code) = self.code.take() {
                        self.flush_code(code);
                    }
                }
                TagEnd::List(_) => {
                    self.flush_inline();
                    self.lists.pop();
                }
                TagEnd::Item => self.flush_inline(),
                TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
                TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
                TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
                TagEnd::Link | TagEnd::Image => match self.links.pop() {
                    Some(Some(url)) => {
                        self.underline = self.underline.saturating_sub(1);
                        self.inline.push((format!(" ({url})"), dim()));
                    }
                    Some(None) => self.autolink_dim = self.autolink_dim.saturating_sub(1),
                    None => {}
                },
                TagEnd::HtmlBlock => self.flush_inline(),
                TagEnd::Table => {
                    self.in_table = false;
                    self.row.clear();
                    self.need_sep = true;
                }
                TagEnd::TableHead => {
                    self.bold = self.bold.saturating_sub(1);
                    self.end_row();
                }
                TagEnd::TableRow => self.end_row(),
                TagEnd::TableCell => {
                    if self.in_table {
                        let cell = std::mem::take(&mut self.inline);
                        self.row.push(cell);
                    }
                }
                _ => {}
            },
            Event::Text(t) => self.push_text(&t),
            Event::Code(t) => {
                let style = self.style().fg(Color::Cyan);
                self.inline.push((t.to_string(), style));
            }
            Event::Html(t) | Event::InlineHtml(t) => self.push_text(&t),
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.push_text("\n"),
            Event::Rule => {
                self.flush_inline();
                self.sep();
                let (first, _) = self.prefixes();
                let budget = self.width.saturating_sub(Self::prefix_width(&first)).max(1);
                self.out
                    .push(Self::to_line(&first, vec![("─".repeat(budget), dim())]));
                self.need_sep = true;
            }
            // FootnoteReference / TaskListMarker / math need extensions
            // that are not enabled; nothing else carries content.
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush_inline();
        if let Some(code) = self.code.take() {
            self.flush_code(code);
        }
        // Drop trailing blank lines (blank INTERIOR lines are content).
        while matches!(self.out.last(), Some(l) if l.spans.iter().all(|s| s.content.trim().is_empty()))
        {
            self.out.pop();
        }
        self.out
    }
}

/// Split verbatim text (code lines) at `budget` display columns with no
/// word logic and no ellipsis — content is never lost.
fn hard_split(s: &str, budget: usize) -> Vec<String> {
    let budget = budget.max(1);
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut w = 0usize;
    for c in s.chars() {
        let cw = display_width(&c.to_string());
        if w + cw > budget && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            w = 0;
        }
        cur.push(c);
        w += cw;
    }
    out.push(cur);
    out
}

/// T45 (P1): LaTeX-to-Unicode substitution for assistant prose.
///
/// Math is NOT CommonMark. pulldown-cmark does not tag `$...$`, `$$...$$`,
/// `\(...\)` or `\[...\]`, so every delimiter, backslash command and `^`
/// reaches the renderer as ordinary text. This module is a pure text
/// transform applied to `Event::Text` OUTSIDE code blocks (and never to
/// `Event::Code`, which is a different event), so it is a substitution
/// pass rather than a parser change.
///
/// Scope, decided in the ROADMAP entry this implements: real LaTeX layout
/// is out of the question on a line-based renderer, because fractions,
/// radicals and limits need two-dimensional placement. What is achievable
/// is recognizing the delimiters, dropping them, mapping the common
/// commands to Unicode and lifting simple `^`/`_` runs. Anything
/// structural degrades honestly: the reader sees the LaTeX source for
/// what cannot map, backslash included.
///
/// Monochrome contract (view.rs:7 and this file's header) is untouched:
/// text in, text out, no styling and no color.
mod latex {
    use super::{Event, Parser, Tag};

    /// Does a `$...$` interior read as math rather than money?
    ///
    /// THE GUARD, and the one place this file departs from its brief. The
    /// brief specifies a spacing rule (opening `$` followed by a non-space,
    /// closing `$` preceded by a non-space) AND an acceptance example,
    /// `Solve $ \int 2x \cos(x^2)\,dx $`, whose delimiters are padded with
    /// exactly the spaces that rule forbids. The two cannot both hold.
    ///
    /// Resolved by making the guard SIGNAL-based: a `$` span is math only
    /// when its interior carries a LaTeX signal (`\`, `^` or `_`). That
    /// satisfies both pinned cases. "costs $5 and $10" has no signal
    /// anywhere between the dollars and passes through untouched, which is
    /// what the guard is for; the acceptance example carries `\int` and is
    /// recognized despite its padding.
    ///
    /// The brief's spacing rule is kept as a SECOND guard for the harder
    /// half: a span whose only signal is `^` or `_` and which is padded
    /// like currency stays literal. A span carrying a backslash command is
    /// unambiguous and does not need it.
    /// T48/P3 widening. The baked default model habitually pads its
    /// delimiters, so the second guard above rejected every span it
    /// produced and the LaTeX pass half-fired on the flagship model.
    /// Symmetric padding (a space on BOTH sides of the interior) now
    /// reads as math too; asymmetric padding stays rejected, since only
    /// one padded side is the currency shape the guard exists for.
    ///
    /// The widening is CARET-ONLY, and that restriction is the whole
    /// safety argument. Every live observation motivating it is padded
    /// caret math ("$ x^2 + y^3 $", "$ e^u + C $", "$ u = x^2 $"); not
    /// one is padded underscore-only. Underscore, meanwhile, is the
    /// snake_case hazard: "costs $ 5 for basic_tier and $ 10" is
    /// symmetric, carries an underscore, and is money to any reader. So
    /// underscore-only spans keep the tight-delimiter rule untouched and
    /// that sentence stays literal. A span carrying a caret has its
    /// admission ticket, and any underscore inside it then lifts as
    /// usual.
    ///
    /// DELIBERATE MISS, recorded rather than solved: padded
    /// underscore-only math ("$ x_1 $") stays literal. Zero live
    /// observations ask for it and one live hazard argues against it.
    /// Revisit only if dogfooding produces a real one.
    fn is_math(inner: &str) -> bool {
        if inner.trim().is_empty() {
            return false;
        }
        let backslash = inner.contains('\\');
        let caret = inner.contains('^');
        if !(backslash || caret || inner.contains('_')) {
            return false;
        }
        if backslash {
            return true;
        }
        let first = inner.chars().next().unwrap_or(' ');
        let last = inner.chars().last().unwrap_or(' ');
        if !first.is_whitespace() && !last.is_whitespace() {
            return true;
        }
        caret && first.is_whitespace() && last.is_whitespace()
    }

    /// Entry point: substitute over the SOURCE, with code regions cut out.
    ///
    /// WHY NOT ON `Event::Text`, which is where a substitution pass
    /// obviously belongs. CommonMark backslash escaping runs first and
    /// destroys the input this pass exists to read. Measured on
    /// pulldown-cmark 0.13.4 with the brief's own acceptance example:
    ///
    /// ```text
    /// Solve $ \int 2x \cos(x^2)\,dx $
    ///   Text("Solve $ \\int 2x \\cos(x^2)")
    ///   Text(",dx $")
    /// ```
    ///
    /// `\,` is a backslash before ASCII punctuation, so the parser strips
    /// the backslash AND splits the text node: the thin space is already a
    /// stray comma (the exact degradation the ROADMAP entry noticed in the
    /// operator's paste) and the closing `$` arrives in a different event
    /// from the opening one. `\(...\)` and `\[...\]` disappear the same
    /// way, `\(` arriving as a bare `(`. No Text-event pass can recover
    /// any of that, so the transform runs before the parser instead.
    ///
    /// The exclusion contract is unchanged and is enforced by BYTE RANGE
    /// from a first parsing pass: inline code spans, fenced and indented
    /// code blocks, and HTML keep their source bytes exactly.
    pub fn substitute_source(text: &str) -> String {
        if !has_delimiter(text) {
            return text.to_string();
        }
        let mut protected: Vec<std::ops::Range<usize>> = Vec::new();
        for (ev, range) in Parser::new_ext(text, super::MD_OPTIONS).into_offset_iter() {
            match ev {
                Event::Code(_) | Event::Html(_) | Event::InlineHtml(_) => {
                    protected.push(range)
                }
                Event::Start(Tag::CodeBlock(_)) | Event::Start(Tag::HtmlBlock) => {
                    protected.push(range)
                }
                _ => {}
            }
        }
        protected.sort_by_key(|r| r.start);
        let mut out = String::with_capacity(text.len());
        let mut at = 0usize;
        for r in protected {
            if r.start >= at {
                out.push_str(&substitute(&text[at..r.start]));
                out.push_str(&text[r.start..r.end.min(text.len())]);
                at = r.end.min(text.len());
            }
        }
        out.push_str(&substitute(&text[at..]));
        out
    }

    fn has_delimiter(text: &str) -> bool {
        text.contains('$') || text.contains("\\(") || text.contains("\\[")
    }

    /// Delimiters are only chased WITHIN one unprotected stretch. A span whose
    /// closer landed in another markdown event passes through literally:
    /// honest degradation on a cheap rule, and the alternative is
    /// stitching state across events for a case nobody reported.
    fn substitute(text: &str) -> String {
        if !has_delimiter(text) {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut i = 0usize;
        while i < text.len() {
            let rest = &text[i..];
            // `$$` is tried before `$`, and unlike `$` it needs no signal:
            // double-dollar is not a currency form.
            if let Some(inner) = between(rest, "$$", "$$") {
                if !inner.trim().is_empty() {
                    out.push_str(render(inner).trim());
                    i += 4 + inner.len();
                    continue;
                }
            } else if let Some(inner) = between(rest, "\\(", "\\)") {
                if !inner.trim().is_empty() {
                    out.push_str(render(inner).trim());
                    i += 4 + inner.len();
                    continue;
                }
            } else if let Some(inner) = between(rest, "\\[", "\\]") {
                if !inner.trim().is_empty() {
                    out.push_str(render(inner).trim());
                    i += 4 + inner.len();
                    continue;
                }
            } else if let Some(inner) = between(rest, "$", "$") {
                if is_math(inner) {
                    out.push_str(render(inner).trim());
                    i += 2 + inner.len();
                    continue;
                }
            }
            let c = rest.chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
        out
    }

    /// The slice between `open` at the start of `s` and the next `close`,
    /// or `None` when `s` does not open with it or the closer is absent.
    fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
        let body = s.strip_prefix(open)?;
        let end = body.find(close)?;
        Some(&body[..end])
    }

    /// One recognized span's interior, with the delimiters already gone.
    fn render(src: &str) -> String {
        let mut out = String::with_capacity(src.len());
        let mut i = 0usize;
        while i < src.len() {
            let rest = &src[i..];
            let c = rest.chars().next().unwrap();
            match c {
                '\\' => {
                    let after = &rest[1..];
                    let name_len = after
                        .chars()
                        .take_while(|ch| ch.is_ascii_alphabetic())
                        .count();
                    if name_len > 0 {
                        let name = &after[..name_len];
                        if let Some(sym) = symbol(name) {
                            out.push_str(sym);
                        } else if is_text_operator(name) {
                            // `\cos` reads as `cos`: the backslash is
                            // markup, the word is the operator.
                            out.push_str(name);
                        } else {
                            // Structural or unknown (\frac, \lim, \begin):
                            // VERBATIM, backslash included. The reader sees
                            // honest LaTeX for what cannot map.
                            out.push('\\');
                            out.push_str(name);
                        }
                        i += 1 + name_len;
                    } else {
                        match after.chars().next() {
                            // Thin and medium spaces become one space.
                            Some(',') | Some(';') | Some(':') => {
                                out.push(' ');
                                i += 2;
                            }
                            // Negative thin space: nothing to render.
                            Some('!') => i += 2,
                            Some(ch) => {
                                out.push('\\');
                                out.push(ch);
                                i += 1 + ch.len_utf8();
                            }
                            None => {
                                out.push('\\');
                                i += 1;
                            }
                        }
                    }
                }
                '^' | '_' => {
                    let after = &rest[1..];
                    match take_run(after) {
                        Some((run, raw)) => {
                            match lift(run, c == '^') {
                                Some(s) => out.push_str(&s),
                                // NEVER half a run: the whole thing falls
                                // back to its literal source form.
                                None => {
                                    out.push(c);
                                    out.push_str(&after[..raw]);
                                }
                            }
                            i += 1 + raw;
                        }
                        None => {
                            out.push(c);
                            i += 1;
                        }
                    }
                }
                _ => {
                    out.push(c);
                    i += c.len_utf8();
                }
            }
        }
        out
    }

    /// The run a `^` or `_` applies to, plus how many bytes of source it
    /// occupies: a braced group, or one character.
    fn take_run(after: &str) -> Option<(&str, usize)> {
        if let Some(body) = after.strip_prefix('{') {
            let end = body.find('}')?;
            return Some((&body[..end], end + 2));
        }
        let c = after.chars().next()?;
        Some((&after[..c.len_utf8()], c.len_utf8()))
    }

    /// Lift a whole run, or nothing. The Unicode super/subscript alphabets
    /// are INCOMPLETE (no superscript `q`, no subscript `b`), so a run with
    /// one unmappable character falls back entirely.
    fn lift(run: &str, sup: bool) -> Option<String> {
        if run.is_empty() {
            return None;
        }
        run.chars()
            .map(|c| if sup { super_char(c) } else { sub_char(c) })
            .collect()
    }

    /// Parentheses are deliberately ABSENT from both alphabets even though
    /// U+207D/U+207E exist. `x^(n+1)` is the brief's pinned fallback case,
    /// and a lifted paren would either produce `x⁽n+1)` (half a run) or
    /// silently swallow a grouping the writer meant literally.
    fn super_char(c: char) -> Option<char> {
        Some(match c {
            '0' => '\u{2070}', '1' => '\u{00b9}', '2' => '\u{00b2}', '3' => '\u{00b3}',
            '4' => '\u{2074}', '5' => '\u{2075}', '6' => '\u{2076}', '7' => '\u{2077}',
            '8' => '\u{2078}', '9' => '\u{2079}',
            '+' => '\u{207a}', '-' => '\u{207b}', '=' => '\u{207c}',
            'a' => 'ᵃ', 'b' => 'ᵇ', 'c' => 'ᶜ', 'd' => 'ᵈ', 'e' => 'ᵉ', 'f' => 'ᶠ',
            'g' => 'ᵍ', 'h' => 'ʰ', 'i' => 'ⁱ', 'j' => 'ʲ', 'k' => 'ᵏ', 'l' => 'ˡ',
            'm' => 'ᵐ', 'n' => 'ⁿ', 'o' => 'ᵒ', 'p' => 'ᵖ', 'r' => 'ʳ', 's' => 'ˢ',
            't' => 'ᵗ', 'u' => 'ᵘ', 'v' => 'ᵛ', 'w' => 'ʷ', 'x' => 'ˣ', 'y' => 'ʸ',
            'z' => 'ᶻ',
            _ => return None,
        })
    }

    fn sub_char(c: char) -> Option<char> {
        Some(match c {
            '0' => '\u{2080}', '1' => '\u{2081}', '2' => '\u{2082}', '3' => '\u{2083}',
            '4' => '\u{2084}', '5' => '\u{2085}', '6' => '\u{2086}', '7' => '\u{2087}',
            '8' => '\u{2088}', '9' => '\u{2089}',
            '+' => '\u{208a}', '-' => '\u{208b}', '=' => '\u{208c}',
            'a' => 'ₐ', 'e' => 'ₑ', 'h' => 'ₕ', 'i' => 'ᵢ', 'j' => 'ⱼ', 'k' => 'ₖ',
            'l' => 'ₗ', 'm' => 'ₘ', 'n' => 'ₙ', 'o' => 'ₒ', 'p' => 'ₚ', 'r' => 'ᵣ',
            's' => 'ₛ', 't' => 'ₜ', 'u' => 'ᵤ', 'v' => 'ᵥ', 'x' => 'ₓ',
            _ => return None,
        })
    }

    /// Text operators: the backslash is markup, the word renders as itself.
    /// `\lim`, `\sup` and `\inf` are NOT here on purpose. They take limits
    /// underneath, which is exactly the two-dimensional placement this pass
    /// does not attempt, so they stay verbatim and say so.
    fn is_text_operator(name: &str) -> bool {
        matches!(
            name,
            "sin" | "cos" | "tan" | "cot" | "sec" | "csc"
                | "arcsin" | "arccos" | "arctan"
                | "sinh" | "cosh" | "tanh" | "coth"
                | "exp" | "log" | "ln" | "lg"
                | "det" | "gcd" | "arg" | "deg" | "dim" | "ker" | "max" | "min"
        )
    }

    /// The substitution table. No entry maps to U+2014: where a command's
    /// natural target would be an em dash it is left UNMAPPED and renders
    /// verbatim, per this milestone's hygiene rule.
    fn symbol(name: &str) -> Option<&'static str> {
        Some(match name {
            // Operators and relations
            "int" => "∫", "iint" => "∬", "iiint" => "∭", "oint" => "∮",
            "sum" => "∑", "prod" => "∏", "coprod" => "∐", "sqrt" => "√",
            "cdot" => "·", "times" => "×", "div" => "÷", "ast" => "∗",
            "pm" => "±", "mp" => "∓", "leq" => "≤", "le" => "≤",
            "geq" => "≥", "ge" => "≥", "neq" => "≠", "ne" => "≠",
            "approx" => "≈", "equiv" => "≡", "cong" => "≅", "sim" => "∼",
            "simeq" => "≃", "propto" => "∝", "ll" => "≪", "gg" => "≫",
            // Arrows
            "to" => "→", "rightarrow" => "→", "leftarrow" => "←",
            "leftrightarrow" => "↔", "Rightarrow" => "⇒", "Leftarrow" => "⇐",
            "Leftrightarrow" => "⇔", "mapsto" => "↦", "implies" => "⇒",
            "iff" => "⇔", "uparrow" => "↑", "downarrow" => "↓",
            // Analysis and set theory
            "infty" => "∞", "partial" => "∂", "nabla" => "∇",
            "forall" => "∀", "exists" => "∃", "nexists" => "∄",
            "in" => "∈", "notin" => "∉", "ni" => "∋",
            "subset" => "⊂", "subseteq" => "⊆", "supset" => "⊃",
            "supseteq" => "⊇", "cup" => "∪", "cap" => "∩",
            "setminus" => "∖", "emptyset" => "∅", "varnothing" => "∅",
            "neg" => "¬", "land" => "∧", "lor" => "∨",
            "therefore" => "∴", "because" => "∵",
            "angle" => "∠", "perp" => "⊥", "parallel" => "∥",
            "circ" => "∘", "bullet" => "∙", "star" => "⋆",
            "prime" => "′", "degree" => "°",
            "ldots" => "…", "dots" => "…", "cdots" => "⋯", "vdots" => "⋮",
            "aleph" => "ℵ", "hbar" => "ℏ", "ell" => "ℓ", "Re" => "ℜ", "Im" => "ℑ",
            // Lowercase Greek
            "alpha" => "α", "beta" => "β", "gamma" => "γ", "delta" => "δ",
            "epsilon" => "ε", "varepsilon" => "ε", "zeta" => "ζ", "eta" => "η",
            "theta" => "θ", "vartheta" => "ϑ", "iota" => "ι", "kappa" => "κ",
            "lambda" => "λ", "mu" => "μ", "nu" => "ν", "xi" => "ξ",
            "omicron" => "ο", "pi" => "π", "varpi" => "ϖ", "rho" => "ρ",
            "varrho" => "ϱ", "sigma" => "σ", "varsigma" => "ς", "tau" => "τ",
            "upsilon" => "υ", "phi" => "φ", "varphi" => "ϕ", "chi" => "χ",
            "psi" => "ψ", "omega" => "ω",
            // Uppercase Greek that differs from a Latin letter
            "Gamma" => "Γ", "Delta" => "Δ", "Theta" => "Θ", "Lambda" => "Λ",
            "Xi" => "Ξ", "Pi" => "Π", "Sigma" => "Σ", "Upsilon" => "Υ",
            "Phi" => "Φ", "Psi" => "Ψ", "Omega" => "Ω",
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 80;

    /// Flatten rendered lines to plain text rows (trailing spaces trimmed).
    // ------------------------------------------- T45 (P1): LaTeX pass

    /// The operator's reported case, quoted in the ROADMAP entry this
    /// milestone implements: this exact input came back as its own source.
    /// The integral sign, the lifted square, the dropped delimiters and the
    /// de-backslashed cos are the non-negotiable part.
    #[test]
    fn latex_acceptance_example_from_the_roadmap() {
        let out = plain(&render(r"Solve $ \int 2x \cos(x^2)\,dx $", 60));
        assert_eq!(out, vec!["   Solve ∫ 2x cos(x²) dx"]);
    }

    /// THE CURRENCY GUARD. Nothing between the dollars is a LaTeX signal,
    /// so nothing here is a span and the line is untouched.
    #[test]
    fn currency_passes_through_untouched() {
        assert_eq!(
            plain(&render("costs $5 and $10", 60)),
            vec!["   costs $5 and $10"]
        );
        // T48/P3 RE-PIN, and the one case this widening costs. T45 pinned
        // this padded-caret line as money; symmetric padding now reads it
        // as math, so it renders lifted. Recorded rather than worked
        // around: the caret is the admission ticket by design, and the
        // shapes a reader actually writes for money carry no caret. Both
        // of those are pinned directly below.
        assert_eq!(
            plain(&render("worth $ 5^2 and $ 10", 60)),
            vec!["   worth 5² and 10"]
        );
    }

    /// T48/P3: the money shapes the caret-only restriction exists to
    /// protect. Both are symmetrically padded and both carry an
    /// underscore, so the T45 rule would have been widened straight over
    /// them had the widening keyed on `^` or `_` together.
    #[test]
    fn padded_snake_case_money_stays_money() {
        assert_eq!(
            plain(&render("costs $ 5 for basic_tier and $ 10", 60)),
            vec!["   costs $ 5 for basic_tier and $ 10"]
        );
        assert_eq!(
            plain(&render(
                "the tiers cost $ 5_000 for team_a and $ 12_000 for team_b",
                80
            )),
            vec!["   the tiers cost $ 5_000 for team_a and $ 12_000 for team_b"]
        );
    }

    /// T48/P3 acceptance, verbatim from the operator's live transcript.
    /// The baked default model pads its delimiters, and every one of
    /// these was rejected before the widening.
    #[test]
    fn padded_caret_spans_from_the_default_model_render() {
        assert_eq!(plain(&render("$ x^2 + y^3 $", 60)), vec!["   x² + y³"]);
        assert_eq!(plain(&render("$ e^u + C $", 60)), vec!["   eᵘ + C"]);
        assert_eq!(plain(&render("$ u = x^2 $", 60)), vec!["   u = x²"]);
        // Unpadded still renders: the tight rule is untouched.
        assert_eq!(plain(&render("$x^2$", 60)), vec!["   x²"]);
        // The braced fallback is NOT part of this widening and does not
        // move: a braced run that cannot be lifted stays whole and
        // literal, padded or not.
        assert_eq!(plain(&render("$ e^{x^2} $", 60)), vec!["   e^{x^2}"]);
        assert_eq!(plain(&render("$e^{x^2}$", 60)), vec!["   e^{x^2}"]);
    }

    /// T48/P3: the widening is SYMMETRIC padding only, and CARET only.
    #[test]
    fn asymmetric_padding_and_underscore_only_padding_stay_literal() {
        assert_eq!(
            plain(&render("$ x^2$", 60)),
            vec!["   $ x^2$"],
            "one padded side is the currency shape, not math"
        );
        assert_eq!(
            plain(&render("$x^2 $", 60)),
            vec!["   $x^2 $"],
            "and the same the other way round"
        );
        // DELIBERATE MISS, recorded in the T48 row: padded
        // underscore-only math stays literal, because no live observation
        // asks for it and snake_case money argues against it.
        assert_eq!(
            plain(&render("$ x_1 $", 60)),
            vec!["   $ x_1 $"],
            "padded underscore-only is a recorded miss, not an oversight"
        );
        // A mixed span is admitted by its caret, and the underscore then
        // lifts normally inside it.
        assert_eq!(plain(&render("$ x_1^2 $", 60)), vec!["   x₁²"]);
    }

    /// Both directions of the incomplete-alphabet rule, pinned together.
    #[test]
    fn superscript_lifts_whole_runs_only() {
        assert_eq!(plain(&render(r"$x^2$", 60)), vec!["   x²"]);
        assert_eq!(
            plain(&render(r"$x^(n+1)$", 60)),
            vec!["   x^(n+1)"],
            "a run that does not map falls back whole"
        );
        assert_eq!(
            plain(&render(r"$x^{q}$", 60)),
            vec!["   x^{q}"],
            "there is no superscript q, so the braced run falls back verbatim"
        );
        assert_eq!(
            plain(&render(r"$a_{i}$", 60)),
            vec!["   aᵢ"],
            "subscripts lift on the same rule"
        );
    }

    /// Structural commands have no Unicode form and are not faked: the
    /// delimiters drop, the backslash stays.
    #[test]
    fn structural_commands_stay_verbatim_inside_the_span() {
        assert_eq!(
            plain(&render(r"$\frac{1}{2} \to \lim$", 60)),
            vec![r"   \frac{1}{2} → \lim"]
        );
    }

    /// Code spans and code blocks are the pinned exclusions, enforced by
    /// byte range before the transform runs.
    #[test]
    fn code_is_never_substituted() {
        assert_eq!(
            plain(&render("use `$x^2$` here", 60)),
            vec!["   use $x^2$ here"],
            "inline code span is byte-identical"
        );
        assert_eq!(
            plain(&render("```\n$ \\int x^2 $\n```", 60)),
            vec!["   ▌ $ \\int x^2 $"],
            "fenced block is byte-identical"
        );
    }

    /// The other three delimiter forms, and the spacing commands.
    #[test]
    fn other_delimiters_and_spacing_commands() {
        assert_eq!(
            plain(&render(r"\(\alpha \leq \beta\)", 60)),
            vec!["   α ≤ β"]
        );
        assert_eq!(plain(&render(r"\[\sum x\]", 60)), vec!["   ∑ x"]);
        assert_eq!(plain(&render(r"$$\pi r^2$$", 60)), vec!["   π r²"]);
        assert_eq!(
            plain(&render(r"$a\,b\;c$", 60)),
            vec!["   a b c"],
            "thin and medium spaces become one space each"
        );
    }

    /// An opener with no closer in the same unprotected stretch stays
    /// literal: honest degradation, no state carried across the document.
    #[test]
    fn unclosed_delimiter_passes_through() {
        assert_eq!(
            plain(&render(r"the cost is $\int with no closer", 60)),
            vec![r"   the cost is $\int with no closer"]
        );
    }

    fn plain(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn find_span<'a>(lines: &'a [Line<'static>], needle: &str) -> &'a Span<'static> {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.contains(needle))
            .unwrap_or_else(|| panic!("no span containing {needle:?}"))
    }

    fn has_mod(s: &Span<'static>, m: Modifier) -> bool {
        s.style.add_modifier.contains(m)
    }

    #[test]
    fn empty_input_renders_nothing() {
        assert!(render("", W).is_empty());
        assert!(render("   \n\n", W).is_empty());
    }

    #[test]
    fn paragraph_indents_and_wraps_at_width_minus_three() {
        let lines = render("one two three four five", 13);
        assert_eq!(plain(&lines), vec!["   one two", "   three four", "   five"]);
    }

    #[test]
    fn paragraphs_get_a_blank_separator_and_softbreak_is_a_space() {
        assert_eq!(plain(&render("a\n\nb", W)), vec!["   a", "", "   b"]);
        // A single newline is a CommonMark soft break: reflowed as a space.
        assert_eq!(plain(&render("a\nb", W)), vec!["   a b"]);
    }

    #[test]
    fn hard_break_breaks_the_line() {
        // Trailing double-space = hard break.
        assert_eq!(plain(&render("a  \nb", W)), vec!["   a", "   b"]);
    }

    #[test]
    fn headings_bold_h1_h2_also_underlined() {
        let lines = render("# Big\n\n## Second\n\n### Small", W);
        assert_eq!(plain(&lines), vec!["   Big", "", "   Second", "", "   Small"]);
        let big = find_span(&lines, "Big");
        assert!(has_mod(big, Modifier::BOLD) && has_mod(big, Modifier::UNDERLINED));
        let second = find_span(&lines, "Second");
        assert!(has_mod(second, Modifier::BOLD) && has_mod(second, Modifier::UNDERLINED));
        let small = find_span(&lines, "Small");
        assert!(has_mod(small, Modifier::BOLD) && !has_mod(small, Modifier::UNDERLINED));
    }

    #[test]
    fn emphasis_maps_to_bold_italic_dim() {
        let lines = render("**b** and *i* and ~~s~~", W);
        assert!(has_mod(find_span(&lines, "b"), Modifier::BOLD));
        assert!(has_mod(find_span(&lines, "i"), Modifier::ITALIC));
        assert!(has_mod(find_span(&lines, "s"), Modifier::DIM));
        let and = find_span(&lines, "and");
        assert_eq!(and.style.add_modifier, Modifier::empty());
    }

    #[test]
    fn inline_code_is_cyan() {
        let lines = render("run `cargo test` now", W);
        assert_eq!(find_span(&lines, "cargo test").style.fg, Some(Color::Cyan));
        assert_eq!(find_span(&lines, "run").style.fg, None);
    }

    #[test]
    fn fenced_code_block_gutter_lang_and_verbatim_lines() {
        let lines = render("```rust\nfn main() { let x = 1; }\n```", W);
        let rows = plain(&lines);
        assert_eq!(rows, vec!["   ▌ rust", "   ▌ fn main() { let x = 1; }"]);
        let gutter = find_span(&lines, "▌");
        assert!(has_mod(gutter, Modifier::DIM));
        assert!(has_mod(find_span(&lines, "rust"), Modifier::DIM));
        // Code text itself is unstyled (no highlighting, not dim).
        let code = find_span(&lines, "fn main");
        assert_eq!(code.style, Style::default());
        // Internal spacing is verbatim — never re-wrapped at word bounds.
        let lines = render("```\na   b\n```", W);
        let untrimmed: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(untrimmed.contains("a   b"));
    }

    #[test]
    fn code_block_overlong_lines_hard_split_no_loss() {
        let lines = render("```\nalpha beta gamma delta\n```", 15);
        // Budget is 15 - 5 (indent+gutter) = 10 columns, split verbatim.
        assert_eq!(plain(&lines), vec!["   ▌ alpha beta", "   ▌  gamma del", "   ▌ ta"]);
    }

    #[test]
    fn code_block_preserves_blank_interior_lines() {
        let lines = render("```\na\n\nb\n```", W);
        assert_eq!(plain(&lines), vec!["   ▌ a", "   ▌", "   ▌ b"]);
    }

    #[test]
    fn indented_code_block_has_gutter_no_lang() {
        let lines = render("para\n\n    indented code\n", W);
        assert_eq!(plain(&lines), vec!["   para", "", "   ▌ indented code"]);
    }

    #[test]
    fn unclosed_fence_streams_as_code_until_closer_arrives() {
        // Mid-stream: fence opened, closer not yet received.
        let partial = render("text\n\n```rust\nlet x = 1;", W);
        assert_eq!(plain(&partial), vec!["   text", "", "   ▌ rust", "   ▌ let x = 1;"]);
        // Once the closer lands the rendering is identical plus nothing.
        let complete = render("text\n\n```rust\nlet x = 1;\n```", W);
        assert_eq!(plain(&partial), plain(&complete));
    }

    #[test]
    fn unclosed_emphasis_stays_literal() {
        let lines = render("a *partial emph", W);
        assert_eq!(plain(&lines), vec!["   a *partial emph"]);
        assert!(!lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|s| has_mod(s, Modifier::ITALIC)));
    }

    #[test]
    fn bullet_list_wraps_with_hanging_indent() {
        let lines = render("- alpha beta gamma\n- second", 18);
        // Budget 18-5: "alpha beta" fits, "gamma" hangs under the text.
        assert_eq!(
            plain(&lines),
            vec!["   • alpha beta", "     gamma", "   • second"]
        );
    }

    #[test]
    fn ordered_list_numbers_and_start_offset() {
        let lines = render("3. three\n4. four", W);
        assert_eq!(plain(&lines), vec!["   3. three", "   4. four"]);
    }

    #[test]
    fn nested_list_indents_two_spaces_per_level() {
        let lines = render("- outer\n  - inner one two three", 22);
        assert_eq!(
            plain(&lines),
            vec!["   • outer", "     • inner one two", "       three"]
        );
    }

    #[test]
    fn blockquote_dim_bar_and_wrapped_content() {
        let lines = render("> quoted words wrap here", 15);
        assert_eq!(
            plain(&lines),
            vec!["   │ quoted", "   │ words wrap", "   │ here"]
        );
        assert!(has_mod(find_span(&lines, "│"), Modifier::DIM));
        // Two paragraphs inside one quote: the bar stays continuous.
        let lines = render("> a\n>\n> b", W);
        assert_eq!(plain(&lines), vec!["   │ a", "   │", "   │ b"]);
    }

    #[test]
    fn rule_renders_dim_dashes_to_width() {
        let lines = render("a\n\n---\n\nb", 20);
        let rows = plain(&lines);
        assert_eq!(rows[2], format!("   {}", "─".repeat(17)));
        assert!(has_mod(find_span(&lines, "─"), Modifier::DIM));
    }

    #[test]
    fn links_underline_text_and_append_dim_url() {
        let lines = render("see [docs](https://e.com/d) now", W);
        let text = find_span(&lines, "docs");
        assert!(has_mod(text, Modifier::UNDERLINED));
        let url = find_span(&lines, "(https://e.com/d)");
        assert!(has_mod(url, Modifier::DIM) && !has_mod(url, Modifier::UNDERLINED));
    }

    #[test]
    fn bare_autolinks_are_just_dim() {
        let lines = render("go to <https://e.com> now", W);
        let link = find_span(&lines, "https://e.com");
        assert!(has_mod(link, Modifier::DIM) && !has_mod(link, Modifier::UNDERLINED));
        // The url is not repeated in parentheses.
        assert!(!plain(&lines).join(" ").contains("(https://e.com)"));
    }

    #[test]
    fn table_renders_one_row_per_line() {
        // T47 DELIBERATE INVERSION: this spot previously asserted the
        // documented limitation, that tables were NOT enabled and pulldown
        // emitted the rows as one paragraph with soft breaks reflowed to
        // spaces ("| a | b | |---|---| | 1 | 2 |"). That run-together line
        // is the gap T47 closes, so the assertion is inverted rather than
        // deleted: the old string is quoted here because it is what a
        // reader upgrading from an older build will recognize.
        let lines = render("| a | b |\n|---|---|\n| 1 | 2 |", W);
        assert_eq!(plain(&lines), vec!["   a | b", "   1 | 2"]);
    }

    #[test]
    fn a_model_style_table_keeps_every_cell_on_its_own_row() {
        // The shape models actually emit for "compare A and B": a header,
        // an alignment row, and body rows with multi-word cells.
        let md = "\
| Feature | Rust | Go |
|:--------|:-----|---:|
| GC | none | yes |
| Build speed | slower | fast |
";
        let lines = render(md, W);
        assert_eq!(
            plain(&lines),
            vec![
                "   Feature | Rust | Go",
                "   GC | none | yes",
                "   Build speed | slower | fast",
            ],
            "one row per line, cells intact, alignment row consumed"
        );
    }

    #[test]
    fn the_header_row_is_bold_and_body_rows_are_not() {
        let lines = render("| head | x |\n|---|---|\n| body | y |", W);
        assert!(
            has_mod(find_span(&lines, "head"), Modifier::BOLD),
            "header cells are bold"
        );
        assert!(
            !has_mod(find_span(&lines, "body"), Modifier::BOLD),
            "body cells are not"
        );
    }

    #[test]
    fn a_table_inside_a_fenced_code_block_stays_verbatim() {
        let md = "```\n| a | b |\n|---|---|\n| 1 | 2 |\n```";
        let lines = render(md, W);
        assert_eq!(
            plain(&lines),
            vec!["   ▌ | a | b |", "   ▌ |---|---|", "   ▌ | 1 | 2 |"],
            "code blocks are never parsed as tables"
        );
    }

    #[test]
    fn latex_inside_a_table_cell_still_substitutes() {
        // The ordinary case: a cell's math goes through the T45 pass, and a
        // cell's code span is still protected from it.
        let md = "\
| expr | code |
|------|------|
| $x^2$ | `$y^2$` |
";
        let lines = render(md, W);
        assert_eq!(
            plain(&lines),
            vec!["   expr | code", "   x² | $y^2$"],
            "cell math lifts, cell code stays literal"
        );
    }

    #[test]
    fn tables_and_latex_stay_in_sync() {
        // THE OPTIONS-SYNC TEST, and it is built to FAIL if the two parses
        // ever disagree. The T45 LaTeX pass computes its exclusion BYTE
        // RANGES from its own parse; the renderer parses again to draw. If
        // one knows about tables and the other does not, the ranges describe
        // a different division of the same bytes.
        //
        // The document below is chosen because it is exactly where the two
        // divisions differ (measured, both option sets):
        //   tables ON  - the backticks sit in different CELLS, never pair,
        //                and nothing is protected, so $\alpha$ substitutes.
        //   tables OFF - the rows are one paragraph, the backticks DO pair
        //                into a code span over bytes 22..38, that span is
        //                protected, and $\alpha$ survives as literal source.
        //
        // So the alpha below is the drift detector: it is present only while
        // the options are identical. Verified by deliberately skewing the
        // LaTeX pass to ENABLE_STRIKETHROUGH alone, which turns this red.
        let md = "\
| a | b |
|---|---|
| `x $\\alpha$ | y` |
";
        let rendered = plain(&render(md, W)).join("\n");
        assert!(
            rendered.contains('\u{3b1}'),
            "cell math must substitute; a skewed LaTeX-pass option set \
             protects a code span that does not exist in the render parse, \
             leaving the source literal. Got: {rendered:?}"
        );
        assert!(
            !rendered.contains("\\alpha"),
            "no literal LaTeX may survive here: {rendered:?}"
        );
    }

    #[test]
    fn the_two_parsers_are_given_the_identical_option_set() {
        // The structural half of the guarantee above: there is ONE constant,
        // so the two parses cannot drift even if someone adds an extension
        // to only one call site. This asserts the constant's contents so
        // that adding an option is a deliberate, visible change.
        assert!(MD_OPTIONS.contains(Options::ENABLE_TABLES));
        assert!(MD_OPTIONS.contains(Options::ENABLE_STRIKETHROUGH));
        assert_eq!(
            MD_OPTIONS,
            Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES,
            "exactly these two; add one only with the range-skew risk in mind"
        );
    }

    #[test]
    fn wide_chars_hard_split_in_code_blocks() {
        let lines = render("```\n你好你好你好\n```", 11);
        // Budget 6 columns → three double-width chars per gutter line.
        assert_eq!(plain(&lines), vec!["   ▌ 你好你", "   ▌ 好你好"]);
    }

    #[test]
    fn tiny_widths_never_panic_or_lose_prose() {
        let sample = "# H\n\n- item one\n- two\n\n> quote\n\n```rust\ncode here\n```\n\n`tick` **bold** [l](u) end";
        for w in 0..=3 {
            let lines = render(sample, w);
            let joined: String = plain(&lines).join("\n");
            for token in ["H", "item", "quote", "code", "tick", "bold", "end"] {
                // Tokens may be split across lines at these widths; every
                // character must still be present in order.
                let mut rest = joined.as_str();
                let survives = token.chars().all(|c| match rest.find(c) {
                    Some(i) => {
                        rest = &rest[i + c.len_utf8()..];
                        true
                    }
                    None => false,
                });
                assert!(survives, "width {w}: token {token:?} lost:\n{joined}");
            }
        }
    }

    #[test]
    fn representative_sample_at_two_widths() {
        let sample = "## Plan\n\nFirst `cargo build`, then:\n\n- fix the *parser*\n- run **all** tests\n\n```rust\nfn main() {}\n```";
        let wide = plain(&render(sample, 40));
        assert_eq!(
            wide,
            vec![
                "   Plan",
                "",
                "   First cargo build, then:",
                "",
                "   • fix the parser",
                "   • run all tests",
                "",
                "   ▌ rust",
                "   ▌ fn main() {}",
            ]
        );
        let narrow = plain(&render(sample, 18));
        assert_eq!(
            narrow,
            vec![
                "   Plan",
                "",
                "   First cargo",
                "   build, then:",
                "",
                "   • fix the",
                "     parser",
                "   • run all tests",
                "",
                "   ▌ rust",
                "   ▌ fn main() {}",
            ]
        );
    }
}
