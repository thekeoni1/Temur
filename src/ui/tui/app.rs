//! Pure TUI state: transcript cells folded from `AgentEvent`s, the input
//! line editor, and scroll state. No terminal I/O and no threads live here —
//! everything is driven by injected events and an injected clock (`now_ms`),
//! so tests can replay exact sequences and snapshot the rendered frames.

use crate::agent::events::AgentEvent;
use crate::provider::Usage;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Render-tick period the runtime aims for (spinner cadence).
pub const TICK_MS: u64 = 100;

/// One rendered unit of the transcript, in OpenCode's session-view shapes.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    /// User prompt: left-bar block.
    User(String),
    /// Assistant prose, appended to while streaming.
    AssistantText(String),
    /// Passive "thinking" indicator (one per contiguous run of deltas;
    /// thinking is OFF by default — this only shows if an operator flips it).
    Thinking,
    Tool(ToolCell),
    /// Out-of-band notice (refusal, guard trip, provider error): warning block.
    Notice(String),
    /// Per-response tail, OpenCode's `▣ mode · model · duration` line.
    TurnTail { secs: u64, usage: Usage },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCell {
    pub name: String,
    /// `None` while the call is still running (FIFO-paired with `ToolEnd` —
    /// see docs/TUI.md: load-bearing seam assumption).
    pub title: Option<String>,
    pub is_error: bool,
}

impl ToolCell {
    /// Block-form tools render as bordered boxes (OpenCode's BlockTool);
    /// the rest are one-liners (InlineTool).
    pub fn is_block(&self) -> bool {
        matches!(self.name.as_str(), "bash" | "write" | "edit" | "todowrite")
    }
}

/// What the runtime should do after a key event.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    Submit(String),
    /// Clean quit: agent's `read_input` gets `None`.
    Quit,
    /// Second Ctrl+C during a running turn: restore terminal and exit now.
    ForceQuit,
    /// Esc during a running turn: the runtime sets the session's cancel
    /// token; the turn lands cooperatively (T6).
    Interrupt,
}

pub struct App {
    pub cells: Vec<Cell>,
    // Input line editor.
    pub input: String,
    /// Cursor as a byte offset into `input`, always on a char boundary.
    pub cursor: usize,
    history: Vec<String>,
    hist_pos: Option<usize>,
    draft: String,
    // Turn state.
    pub busy: bool,
    pub force_quit_armed: bool,
    /// Esc was pressed this turn; shown as "interrupting…" until the turn
    /// actually lands (TurnComplete clears it).
    pub interrupting: bool,
    turn_started_ms: u64,
    // Session info for chrome.
    pub title: Option<String>,
    pub model: String,
    pub thinking: bool,
    pub cwd: String,
    pub version: String,
    pub session_usage: Usage,
    /// Wall-clock milliseconds since app start; the runtime advances this,
    /// tests set it directly (spinner frame + turn duration derive from it).
    pub now_ms: u64,
    // Scroll state (sticky bottom like OpenCode's scrollbox).
    pub stick_bottom: bool,
    pub scroll_offset: usize,
    /// Metrics recorded by the last draw; scroll keys use them.
    pub last_total_lines: usize,
    pub last_viewport_h: usize,
}

impl App {
    pub fn new(model: String, thinking: bool, cwd: String, version: String) -> Self {
        App {
            cells: Vec::new(),
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            hist_pos: None,
            draft: String::new(),
            busy: false,
            force_quit_armed: false,
            interrupting: false,
            turn_started_ms: 0,
            title: None,
            model,
            thinking,
            cwd,
            version,
            session_usage: Usage::default(),
            now_ms: 0,
            stick_bottom: true,
            scroll_offset: 0,
            last_total_lines: 0,
            last_viewport_h: 0,
        }
    }

    /// Fold one agent event into the transcript.
    pub fn fold(&mut self, ev: &AgentEvent) {
        match ev {
            AgentEvent::TextDelta(t) => {
                if let Some(Cell::AssistantText(s)) = self.cells.last_mut() {
                    s.push_str(t);
                } else {
                    self.cells.push(Cell::AssistantText(t.clone()));
                }
            }
            AgentEvent::ThinkingDelta(_) => {
                if !matches!(self.cells.last(), Some(Cell::Thinking)) {
                    self.cells.push(Cell::Thinking);
                }
            }
            AgentEvent::ToolStart { name } => {
                self.cells.push(Cell::Tool(ToolCell {
                    name: name.clone(),
                    title: None,
                    is_error: false,
                }));
            }
            AgentEvent::ToolEnd {
                name,
                title,
                is_error,
            } => {
                // FIFO pairing: complete the oldest still-running tool cell.
                // Sound while the core streams tool_use blocks in order and
                // executes them sequentially in that same order.
                let running = self.cells.iter_mut().find_map(|c| match c {
                    Cell::Tool(t) if t.title.is_none() => Some(t),
                    _ => None,
                });
                match running {
                    Some(t) => {
                        t.name = name.clone();
                        t.title = Some(title.clone());
                        t.is_error = *is_error;
                    }
                    None => {
                        // Defensive: never lose a result even if pairing broke.
                        self.cells.push(Cell::Tool(ToolCell {
                            name: name.clone(),
                            title: Some(title.clone()),
                            is_error: *is_error,
                        }));
                    }
                }
            }
            AgentEvent::Notice(n) => self.cells.push(Cell::Notice(n.clone())),
            AgentEvent::TurnComplete {
                turn_usage,
                session_usage,
            } => {
                self.session_usage = *session_usage;
                self.busy = false;
                self.force_quit_armed = false;
                self.interrupting = false;
                self.cells.push(Cell::TurnTail {
                    secs: (self.now_ms.saturating_sub(self.turn_started_ms)) / 1000,
                    usage: *turn_usage,
                });
            }
        }
    }

    /// Record a submitted prompt (the runtime also forwards it to the agent).
    pub fn submit(&mut self, line: &str) {
        if self.title.is_none() {
            self.title = Some(line.to_string());
        }
        self.cells.push(Cell::User(line.to_string()));
        self.history.push(line.to_string());
        self.hist_pos = None;
        self.draft.clear();
        self.input.clear();
        self.cursor = 0;
        self.busy = true;
        self.force_quit_armed = false;
        self.turn_started_ms = self.now_ms;
        self.stick_bottom = true;
    }

    /// The agent is back at the prompt (authoritative idle signal).
    pub fn prompt_open(&mut self) {
        self.busy = false;
        self.force_quit_armed = false;
        self.interrupting = false;
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        // Ignore key-release events (crossterm reports them on some
        // terminals when enhanced flags are on).
        if key.kind == KeyEventKind::Release {
            return Action::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // Any key other than a second Ctrl+C disarms the force-quit prompt.
        let was_armed = self.force_quit_armed;
        self.force_quit_armed = false;

        match key.code {
            KeyCode::Char('c') if ctrl => {
                if self.busy {
                    if was_armed {
                        return Action::ForceQuit;
                    }
                    self.force_quit_armed = true;
                } else if self.input.is_empty() {
                    return Action::Quit;
                } else {
                    self.input.clear();
                    self.cursor = 0;
                }
            }
            KeyCode::Char('d') if ctrl => {
                if !self.busy && self.input.is_empty() {
                    return Action::Quit;
                }
            }
            // Ctrl+M / Ctrl+J are the terminal-conventional Enter bytes
            // (\r / \n); input that raced the raw-mode switch arrives
            // icrnl-translated, so treat them all as Enter.
            KeyCode::Enter | KeyCode::Char('m') | KeyCode::Char('j') if key.code == KeyCode::Enter || ctrl => {
                if !self.busy && !self.input.trim().is_empty() {
                    let line = self.input.trim().to_string();
                    return Action::Submit(line);
                }
            }
            KeyCode::Char(c) if !ctrl => {
                self.input.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            KeyCode::Backspace => {
                if let Some(prev) = self.prev_boundary() {
                    self.input.remove(prev);
                    self.cursor = prev;
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.input.len() {
                    self.input.remove(self.cursor);
                }
            }
            KeyCode::Left => {
                if let Some(prev) = self.prev_boundary() {
                    self.cursor = prev;
                }
            }
            KeyCode::Right => {
                if self.cursor < self.input.len() {
                    let c = self.input[self.cursor..].chars().next().unwrap();
                    self.cursor += c.len_utf8();
                }
            }
            // Esc while a turn runs = cooperative interrupt (T6). Idle Esc
            // is a no-op. A second Esc just re-requests — idempotent. Note
            // the disarm at the top of this fn: Esc also participates in
            // the "any key disarms force-quit" rule.
            KeyCode::Esc => {
                if self.busy {
                    self.interrupting = true;
                    return Action::Interrupt;
                }
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.len(),
            KeyCode::Up => self.history_move(-1),
            KeyCode::Down => self.history_move(1),
            KeyCode::PageUp => self.scroll_page(-1),
            KeyCode::PageDown => self.scroll_page(1),
            _ => {}
        }
        Action::None
    }

    fn prev_boundary(&self) -> Option<usize> {
        if self.cursor == 0 {
            return None;
        }
        self.input[..self.cursor].char_indices().last().map(|(i, _)| i)
    }

    fn history_move(&mut self, dir: i32) {
        if self.history.is_empty() {
            return;
        }
        match (self.hist_pos, dir) {
            (None, -1) => {
                self.draft = self.input.clone();
                self.hist_pos = Some(self.history.len() - 1);
            }
            (Some(0), -1) => {}
            (Some(p), -1) => self.hist_pos = Some(p - 1),
            (None, _) => return,
            (Some(p), _) => {
                if p + 1 >= self.history.len() {
                    self.hist_pos = None;
                    self.input = std::mem::take(&mut self.draft);
                    self.cursor = self.input.len();
                    return;
                }
                self.hist_pos = Some(p + 1);
            }
        }
        if let Some(p) = self.hist_pos {
            self.input = self.history[p].clone();
            self.cursor = self.input.len();
        }
    }

    fn scroll_page(&mut self, dir: i32) {
        let page = self.last_viewport_h.max(1);
        let max_offset = self.last_total_lines.saturating_sub(self.last_viewport_h);
        // Current effective offset (sticky = pinned to the bottom).
        let cur = if self.stick_bottom {
            max_offset
        } else {
            self.scroll_offset.min(max_offset)
        };
        if dir < 0 {
            self.scroll_offset = cur.saturating_sub(page);
            self.stick_bottom = false;
        } else {
            let next = cur.saturating_add(page);
            if next >= max_offset {
                self.stick_bottom = true;
                self.scroll_offset = max_offset;
            } else {
                self.scroll_offset = next;
            }
        }
    }

    /// Spinner glyph for the current tick.
    pub fn spinner(&self) -> char {
        const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        FRAMES[((self.now_ms / TICK_MS) % FRAMES.len() as u64) as usize]
    }
}
