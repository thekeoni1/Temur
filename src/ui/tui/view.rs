//! Frame rendering: App state → ratatui buffer. A behavioral port of
//! OpenCode's session view (header band / scrollback / prompt / status /
//! footer), monochrome-adapted: default terminal colors, dim/bold/red/yellow
//! accents only. Pure function of `App` + area, so TestBackend snapshots are
//! deterministic.

use super::app::{App, Cell};
use super::wrap::{display_width, truncate_width, wrap};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

const BAR: &str = "▌";

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

pub fn draw(app: &mut App, frame: &mut Frame) {
    let [header, transcript, input, status, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .horizontal_margin(1)
    .areas(frame.area());

    draw_header(app, frame, header);
    draw_transcript(app, frame, transcript);
    draw_input(app, frame, input);
    draw_status(app, frame, status);
    draw_footer(app, frame, footer);
}

fn draw_header(app: &App, frame: &mut Frame, area: Rect) {
    let right = format!("{} · temur {}", app.model, app.version);
    let title = app.title.as_deref().unwrap_or("new session");
    let left_budget = (area.width as usize).saturating_sub(display_width(&right) + 3);
    let title = truncate_width(title, left_budget);
    let pad = (area.width as usize)
        .saturating_sub(display_width(&title) + 2 + display_width(&right));
    let line = Line::from(vec![
        Span::styled("# ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" ".repeat(pad)),
        Span::styled(right, dim()),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// Build every transcript line at `width` columns. Separate from drawing so
/// scrolling works on exact totals.
fn transcript_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line> = Vec::new();
    let mut prev_inline_tool = false;
    for cell in &app.cells {
        let inline_tool = matches!(cell, Cell::Tool(t) if !t.is_block());
        // OpenCode groups consecutive one-liner tools; everything else gets
        // a blank separator line.
        if !out.is_empty() && !(inline_tool && prev_inline_tool) {
            out.push(Line::default());
        }
        prev_inline_tool = inline_tool;
        match cell {
            Cell::User(text) => {
                for l in wrap(text, width.saturating_sub(2)) {
                    out.push(Line::from(vec![
                        Span::styled(format!("{BAR} "), dim()),
                        Span::styled(l, Style::default().add_modifier(Modifier::BOLD)),
                    ]));
                }
            }
            Cell::AssistantText(text) => {
                for l in wrap(text.trim_end(), width.saturating_sub(3)) {
                    out.push(Line::from(format!("   {l}")));
                }
            }
            Cell::Thinking => {
                out.push(Line::from(Span::styled("   ~ thinking…", dim())));
            }
            Cell::Tool(t) => {
                let title_budget = width.saturating_sub(6);
                match (&t.title, t.is_block()) {
                    (None, _) => out.push(Line::from(format!("   ~ {}…", t.name))),
                    (Some(title), false) => {
                        let (mark, style) = if t.is_error {
                            ("✗", Style::default().fg(Color::Red))
                        } else {
                            ("⚙", dim())
                        };
                        out.push(Line::from(Span::styled(
                            format!("   {mark} {}: {}", t.name, truncate_width(title, title_budget)),
                            style,
                        )));
                    }
                    (Some(title), true) => {
                        let style = if t.is_error {
                            Style::default().fg(Color::Red)
                        } else {
                            dim()
                        };
                        let mark = if t.is_error { " ✗" } else { "" };
                        out.push(Line::from(vec![
                            Span::styled(format!("{BAR} "), dim()),
                            Span::styled(format!("# {}{mark}", t.name), style),
                        ]));
                        for l in wrap(title, width.saturating_sub(4)) {
                            out.push(Line::from(vec![
                                Span::styled(format!("{BAR} "), dim()),
                                Span::styled(format!("  {l}"), style),
                            ]));
                        }
                    }
                }
            }
            Cell::Notice(n) => {
                for l in wrap(&format!("[!] {n}"), width.saturating_sub(2)) {
                    out.push(Line::from(vec![
                        Span::styled(format!("{BAR} "), Style::default().fg(Color::Yellow)),
                        Span::styled(l, Style::default().fg(Color::Yellow)),
                    ]));
                }
            }
            Cell::TurnTail { secs, usage } => {
                out.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled("▣ ", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        format!(
                            "temur · {} · {}s · {} in / {} out · cache r{} w{}",
                            app.model,
                            secs,
                            crate::ui::fmt_tokens(usage.input_tokens),
                            crate::ui::fmt_tokens(usage.output_tokens),
                            crate::ui::fmt_tokens(usage.cache_read_input_tokens),
                            crate::ui::fmt_tokens(usage.cache_creation_input_tokens),
                        ),
                        dim(),
                    ),
                ]));
            }
        }
    }
    out
}

fn draw_transcript(app: &mut App, frame: &mut Frame, area: Rect) {
    let lines = transcript_lines(app, area.width as usize);
    let total = lines.len();
    let viewport = area.height as usize;
    app.last_total_lines = total;
    app.last_viewport_h = viewport;

    let max_offset = total.saturating_sub(viewport);
    let offset = if app.stick_bottom {
        max_offset
    } else {
        app.scroll_offset.min(max_offset)
    };
    app.scroll_offset = offset;

    let visible: Vec<Line> = lines.into_iter().skip(offset).take(viewport).collect();
    frame.render_widget(Paragraph::new(visible), area);
}

fn draw_input(app: &App, frame: &mut Frame, area: Rect) {
    let prefix = format!("{BAR} > ");
    let budget = (area.width as usize).saturating_sub(display_width(&prefix) + 1);

    if app.input.is_empty() && !app.busy {
        let line = Line::from(vec![
            Span::styled(prefix.clone(), dim()),
            Span::styled("ask anything… (exit to quit)", dim()),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        frame.set_cursor_position((area.x + display_width(&prefix) as u16, area.y));
        return;
    }

    // Horizontal scroll: keep the cursor visible by showing the tail window
    // that contains it.
    let before = &app.input[..app.cursor];
    let mut start = 0usize;
    while display_width(&before[start..]) > budget {
        let c = before[start..].chars().next().unwrap();
        start += c.len_utf8();
    }
    let visible: String = app.input[start..]
        .chars()
        .scan(0usize, |w, c| {
            *w += display_width(&c.to_string());
            if *w > budget {
                None
            } else {
                Some(c)
            }
        })
        .collect();

    let line = Line::from(vec![Span::styled(prefix.clone(), dim()), Span::raw(visible)]);
    frame.render_widget(Paragraph::new(line), area);
    let cursor_x =
        area.x + (display_width(&prefix) + display_width(&app.input[start..app.cursor])) as u16;
    frame.set_cursor_position((cursor_x.min(area.x + area.width.saturating_sub(1)), area.y));
}

fn draw_status(app: &App, frame: &mut Frame, area: Rect) {
    let left: Vec<Span> = if app.busy {
        if app.force_quit_armed {
            vec![
                Span::raw(format!("  {} working… ", app.spinner())),
                Span::styled(
                    "ctrl+c again to force-quit",
                    Style::default().fg(Color::Yellow),
                ),
            ]
        } else if app.interrupting {
            vec![
                Span::raw(format!("  {} working… ", app.spinner())),
                Span::styled("interrupting…", Style::default().fg(Color::Yellow)),
            ]
        } else {
            vec![
                Span::raw(format!("  {} working… ", app.spinner())),
                Span::styled("esc interrupt · (enter disabled during turn)", dim()),
            ]
        }
    } else {
        vec![Span::styled(
            "  enter send · ↑↓ history · pgup/pgdn scroll · ctrl+c quit",
            dim(),
        )]
    };
    if app.stick_bottom {
        frame.render_widget(Paragraph::new(Line::from(left)), area);
    } else {
        // The scroll indicator owns the right edge; the hint text clips.
        let indicator = format!(
            "[scroll {}/{}]",
            app.scroll_offset.saturating_add(app.last_viewport_h).min(app.last_total_lines),
            app.last_total_lines
        );
        let [hint_area, ind_area] = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(display_width(&indicator) as u16),
        ])
        .areas(area);
        frame.render_widget(Paragraph::new(Line::from(left)), hint_area);
        frame.render_widget(Paragraph::new(Span::styled(indicator, dim())), ind_area);
    }
}

fn draw_footer(app: &App, frame: &mut Frame, area: Rect) {
    let session = format!(
        "session {} in / {} out",
        crate::ui::fmt_tokens(app.session_usage.input_tokens),
        crate::ui::fmt_tokens(app.session_usage.output_tokens)
    );
    // Most→least verbose; pick the first that leaves the cwd some room.
    let candidates = [
        format!(
            "{} · thinking {} · {session} · cache r{} w{}",
            app.model,
            if app.thinking { "on" } else { "off" },
            crate::ui::fmt_tokens(app.session_usage.cache_read_input_tokens),
            crate::ui::fmt_tokens(app.session_usage.cache_creation_input_tokens),
        ),
        format!("{} · {session}", app.model),
        session.clone(),
    ];
    let cwd_room = display_width(&app.cwd).min(15) + 2;
    let right = candidates
        .iter()
        .find(|c| display_width(c) + cwd_room <= area.width as usize)
        .unwrap_or(&candidates[2])
        .clone();
    let cwd_budget = (area.width as usize).saturating_sub(display_width(&right) + 2);
    let cwd = truncate_width(&app.cwd, cwd_budget);
    let pad = (area.width as usize).saturating_sub(display_width(&cwd) + display_width(&right));
    let line = Line::from(vec![
        Span::styled(cwd, dim()),
        Span::raw(" ".repeat(pad)),
        Span::styled(right, dim()),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}
