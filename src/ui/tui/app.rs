//! Pure TUI state: transcript cells folded from `AgentEvent`s, the input
//! line editor, and scroll state. No terminal I/O and no threads live here —
//! everything is driven by injected events and an injected clock (`now_ms`),
//! so tests can replay exact sequences and snapshot the rendered frames.

use crate::agent::events::AgentEvent;
use crate::provider::Usage;
use crate::session_store::ReplayItem;
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
    /// T9 `/models` listing: a count line plus one line per id,
    /// notice-styled.
    Models(Vec<String>),
    /// T10 `/sessions` listing: a count line plus one preformatted line per
    /// session (active marker included), notice-styled like `Models`.
    Sessions(Vec<String>),
    /// A submitted `/command` line (T8): echoed dim, never a prompt.
    Command(String),
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
    /// T10: rebuilt from saved history (`SessionLoaded`). Replay knows only
    /// the tool NAME — no title, no output — so these render as `⚙ name`
    /// one-liners even for tools that render block-form live. Always
    /// completed (`title` set), so FIFO pairing never touches them.
    pub replay: bool,
}

impl ToolCell {
    /// Block-form tools render as bordered boxes (OpenCode's BlockTool);
    /// the rest are one-liners (InlineTool). Replay cells are always
    /// one-liners — there is no body to box.
    pub fn is_block(&self) -> bool {
        !self.replay && matches!(self.name.as_str(), "bash" | "write" | "edit" | "todowrite")
    }
}

/// An in-flight Tab-completion cycle (T9): the candidate list computed when
/// the cycle started, and the position the input currently shows.
struct Completion {
    candidates: Vec<String>,
    index: usize,
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
    /// Model ids from the most recent `/models` listing (T9): Tab
    /// completion candidates for `/model <id>`. Session-lifetime cache,
    /// refreshed on every listing; DROPPED when a switch changes the
    /// provider (T16) — one provider's listing must never complete or
    /// judge another provider's ids.
    pub model_ids: Vec<String>,
    /// The provider the cached `model_ids` were listed from — seeded with
    /// the startup provider by the constructor callers, then tracked via
    /// [`AgentEvent::ModelSwitched`].
    pub provider: String,
    /// Profile names for `/model` Tab completion (T9), from SessionInfo.
    pub profiles: Vec<String>,
    /// Session keys from the most recent `/sessions` listing (T10): Tab
    /// completion candidates for `/resume <key>`. Session-lifetime cache,
    /// refreshed on every listing — same policy as `model_ids`.
    pub session_keys: Vec<String>,
    /// T9 Tab cycle: `Some` only between a Tab/BackTab and the next
    /// non-Tab key (any edit, cursor, or history key invalidates it).
    completion: Option<Completion>,
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
            model_ids: Vec::new(),
            provider: String::new(),
            profiles: Vec::new(),
            session_keys: Vec::new(),
            completion: None,
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
                    replay: false,
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
                            replay: false,
                        }));
                    }
                }
            }
            AgentEvent::Notice(n) => self.cells.push(Cell::Notice(n.clone())),
            // T9 `/models`: render the listing AND cache the ids as Tab
            // completion candidates for `/model <id>`.
            AgentEvent::ModelsListed(ids) => {
                self.model_ids = ids.clone();
                self.cells.push(Cell::Models(ids.clone()));
            }
            // T10 `/sessions`: render the listing AND cache the keys as Tab
            // completion candidates for `/resume <key>` (the ModelsListed
            // pattern).
            AgentEvent::SessionsListed { lines, keys } => {
                self.session_keys = keys.clone();
                self.cells.push(Cell::Sessions(lines.clone()));
            }
            // T10 resume: rebuild the transcript from the replayed history.
            // SessionCleared semantics first (transcript, title claim, usage
            // totals), then one cell per item — markdown re-renders the
            // assistant text at draw time — then the resume summary as a
            // Notice. The title claim works exactly as live: the first user
            // prompt names the session (this is what fixes the "new session"
            // header after --continue). Input and completion state are
            // deliberately untouched: resuming must not eat a half-typed
            // line.
            AgentEvent::SessionLoaded { items, notice } => {
                self.cells.clear();
                self.title = None;
                self.session_usage = Usage::default();
                for item in items {
                    match item {
                        ReplayItem::User(t) => {
                            if self.title.is_none() {
                                self.title = Some(t.clone());
                            }
                            self.cells.push(Cell::User(t.clone()));
                        }
                        ReplayItem::Assistant(t) => {
                            self.cells.push(Cell::AssistantText(t.clone()))
                        }
                        ReplayItem::Tool { name } => {
                            self.cells.push(Cell::Tool(ToolCell {
                                name: name.clone(),
                                title: Some(name.clone()),
                                is_error: false,
                                replay: true,
                            }))
                        }
                    }
                }
                self.cells.push(Cell::Notice(notice.clone()));
                self.busy = false;
            }
            // T8 chrome/state signals; the confirmation Notice arrives
            // separately, so these fold silently into chrome. A provider
            // change drops the cached `/models` ids (T16): they described
            // the OLD provider's catalog.
            AgentEvent::ModelSwitched { model, provider } => {
                if *provider != self.provider {
                    self.model_ids.clear();
                    self.provider = provider.clone();
                }
                self.model = model.clone();
            }
            AgentEvent::ThinkingChanged(on) => self.thinking = *on,
            AgentEvent::SessionCleared => {
                // The wipe mirrors Session::clear_history: transcript,
                // title claim, and usage totals all reset; the post-clear
                // Notice (sent after this event) survives into the fresh
                // transcript.
                self.cells.clear();
                self.title = None;
                self.session_usage = Usage::default();
            }
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

    /// Record a submitted COMMAND line (T8): echoed dim in the transcript
    /// and recallable via ↑ like any input, but never a prompt — no title
    /// claim, no User cell, no busy state (commands execute between turns).
    pub fn submit_command(&mut self, line: &str) {
        self.cells.push(Cell::Command(line.to_string()));
        self.history.push(line.to_string());
        self.hist_pos = None;
        self.draft.clear();
        self.input.clear();
        self.cursor = 0;
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
        // T9: any non-Tab key ends a completion cycle — edits, cursor
        // moves, and history recalls all restart completion from whatever
        // the input then says. (history state is untouched by completion:
        // applying a candidate edits `input` only, like typing does.)
        if !matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            self.completion = None;
        }

        match key.code {
            // T9 Tab completion: cycle-in-place, only while idle and only
            // with the cursor at end-of-input; BackTab cycles backwards.
            // (The unconditional disarm above already applies.)
            KeyCode::Tab | KeyCode::BackTab => {
                if !self.busy && self.cursor == self.input.len() {
                    self.cycle_completion(key.code == KeyCode::BackTab);
                }
            }
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

    /// Start or advance the Tab cycle (T9). Candidates are computed ONCE
    /// per cycle from the input the cycle started on; applying one replaces
    /// the whole line and keeps the cursor at the end.
    fn cycle_completion(&mut self, back: bool) {
        match &mut self.completion {
            Some(c) => {
                let n = c.candidates.len();
                c.index = if back { (c.index + n - 1) % n } else { (c.index + 1) % n };
                self.input = c.candidates[c.index].clone();
            }
            None => {
                let candidates = crate::commands::complete(
                    &self.input,
                    &self.profiles,
                    &self.model_ids,
                    &self.session_keys,
                );
                if candidates.is_empty() {
                    return; // nothing to complete: strict no-op
                }
                let index = if back { candidates.len() - 1 } else { 0 };
                self.input = candidates[index].clone();
                self.completion = Some(Completion { candidates, index });
            }
        }
        self.cursor = self.input.len();
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
