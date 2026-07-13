//! Application state shared between the input loop and the renderer,
//! mirroring the TS TUI's routes (home/session), leader-key sequences,
//! DialogSelect filtering, and the prompt editor.

use serde_json::Value;
use std::time::{Duration, Instant};

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Dialog {
    None,
    Commands,
    Sessions,
    Agents,
    Models,
    Help,
    GoalDetails,
    GoalSummaries,
}

/// Explicit navigation route. Default launch lands on Home and never adopts
/// or creates a session; only `--session <id>`, an explicit `/sessions`
/// selection, or the first prompt (which atomically creates a session)
/// transitions to Session.
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Route {
    Home,
    Session,
}

/// Maximum number of visible rows in the slash-command autocomplete
/// popover. Additional entries scroll into view around the selection.
pub const AUTOCOMPLETE_MAX_ROWS: usize = 10;

/// A dynamic slash command loaded from the server via `/api/command`.
/// Merged with the built-in palette when the popover is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynamicCommand {
    pub name: String,
    pub description: String,
    /// Server commands whose template references arguments (`$ARGUMENTS`,
    /// `$1`, ...) support inline arguments after the command name.
    pub takes_args: bool,
    /// Optional label suffix such as `:mcp` for MCP-sourced commands. Skill
    /// sources are excluded upstream before the list reaches the UI.
    pub label: String,
}

/// The originating list for an autocomplete row, so `commit` can dispatch
/// the correct handler (built-in action vs. server command prompt).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryOrigin {
    Builtin(usize),
    Server(usize),
}

/// A single row in the autocomplete popover. Owned so the entry list can
/// merge static built-ins and dynamic server commands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutocompleteEntry {
    pub name: String,
    pub description: String,
    pub takes_args: bool,
    pub label: String,
    pub origin: EntryOrigin,
}

/// Keyboard / mouse input mode. A stale hover event should not hijack the
/// keyboard-driven selection while the layout is shifting underneath the
/// cursor: the popover only follows the mouse after the user actually
/// moves it (Mouse mode); every keypress flips back to Keyboard mode.
#[derive(PartialEq, Clone, Copy, Debug, Default)]
pub enum InputMode {
    #[default]
    Keyboard,
    Mouse,
}

/// Slash-command autocomplete popover state. Opens when the input starts
/// with `/`, filters as the user types, stays open on exact-match and
/// zero-match, and closes on Esc, on space (after the trigger), or when
/// the leading `/` is removed.
pub struct Autocomplete {
    /// Currently-selected entry index into `entries`.
    pub index: usize,
    /// Number of leading rows scrolled off the top so the selection stays
    /// visible within the fixed `AUTOCOMPLETE_MAX_ROWS` viewport.
    pub scroll: usize,
    /// The typed word after the leading `/` (before any whitespace).
    pub filter: String,
    /// Merged built-in + server entries, ordered by descending score.
    pub entries: Vec<AutocompleteEntry>,
}

impl Autocomplete {
    pub fn viewport_height(&self) -> usize {
        self.entries.len().clamp(1, AUTOCOMPLETE_MAX_ROWS)
    }

    /// Adjust `scroll` so `index` is within the viewport.
    pub fn ensure_visible(&mut self) {
        let viewport = self.viewport_height();
        if self.entries.is_empty() {
            self.scroll = 0;
            return;
        }
        if self.index < self.scroll {
            self.scroll = self.index;
        } else if self.index >= self.scroll + viewport {
            self.scroll = self.index + 1 - viewport;
        }
    }
}

/// A pointing-device hover target the renderer paints selected/pressed and
/// the mouse handler routes clicks to.
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum HitTarget {
    /// A row inside the currently-open dialog or the autocomplete popover.
    Row(usize),
    /// The prompt textarea at (row, column) offset within the block.
    Prompt { row: u16, col: u16 },
    /// The agent label span in the prompt agent row.
    AgentSpan,
    /// The `provider model` span in the prompt agent row.
    ModelSpan,
    /// The goal chip in the right-aligned status row.
    GoalChip,
    /// The progress-bar portion of the goal chip opens recorded summaries.
    GoalBar,
    /// The tip row on the home route (cycles when clicked).
    TipRow,
}

/// Deterministic rectangles emitted by the renderer for the mouse handler.
/// Every rect is in screen coordinates; missing rects mean the region isn't
/// visible in the current layout.
#[derive(Default)]
pub struct Geometry {
    pub prompt: Option<Rectangle>,
    pub textarea: Option<Rectangle>,
    pub agent_span: Option<Rectangle>,
    pub model_span: Option<Rectangle>,
    pub goal_chip: Option<Rectangle>,
    pub goal_bar: Option<Rectangle>,
    pub tip_row: Option<Rectangle>,
    /// Dialog panel body (title + search + list). Empty when no dialog is
    /// open.
    pub dialog_area: Option<Rectangle>,
    pub dialog_list_top: u16,
    /// Autocomplete popover rect (relative to screen, above the prompt).
    pub autocomplete: Option<Rectangle>,
    pub autocomplete_list_top: u16,
    /// Number of rendered rows in the autocomplete popover (one per match).
    pub autocomplete_rows: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct Rectangle {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rectangle {
    pub fn contains(&self, column: u16, row: u16) -> bool {
        column >= self.x
            && column < self.x + self.width
            && row >= self.y
            && row < self.y + self.height
    }
}

/// One slash-command palette entry. Shared between the command dialog and
/// the autocomplete popover so both surfaces stay in sync.
pub struct CommandSpec {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub shortcut: &'static str,
    /// `/goal` accepts arguments and stays open after Tab-completion.
    pub takes_args: bool,
}

pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "new",
        title: "New session",
        description: "Return home; the next prompt starts a fresh session",
        shortcut: "ctrl+x n",
        takes_args: false,
    },
    CommandSpec {
        name: "sessions",
        title: "Switch session",
        description: "List and continue sessions",
        shortcut: "ctrl+x l",
        takes_args: false,
    },
    CommandSpec {
        name: "agents",
        title: "Switch agent",
        description: "Choose the active agent",
        shortcut: "ctrl+x a",
        takes_args: false,
    },
    CommandSpec {
        name: "models",
        title: "Switch model",
        description: "See and switch between available AI models",
        shortcut: "ctrl+x m",
        takes_args: false,
    },
    CommandSpec {
        name: "commands",
        title: "Commands",
        description: "Browse every available command and shortcut",
        shortcut: "ctrl+p",
        takes_args: false,
    },
    CommandSpec {
        name: "goal",
        title: "Manage goal",
        description: "set, edit, pause, resume, complete, status, clear",
        shortcut: "/goal",
        takes_args: true,
    },
    CommandSpec {
        name: "goal-details",
        title: "Goal details",
        description: "Show the durable session goal and status",
        shortcut: "ctrl+x g",
        takes_args: false,
    },
    CommandSpec {
        name: "goal-summaries",
        title: "Goal summaries",
        description: "Browse recorded goal state summaries",
        shortcut: "ctrl+x s",
        takes_args: false,
    },
    CommandSpec {
        name: "external-access",
        title: "Toggle out-of-workspace access",
        description: "Allow or ask before accessing external files",
        shortcut: "",
        takes_args: false,
    },
    CommandSpec {
        name: "help",
        title: "Help",
        description: "Show keybindings",
        shortcut: "ctrl+x ?",
        takes_args: false,
    },
    CommandSpec {
        name: "exit",
        title: "Exit the application",
        description: "Quit opencode-tui",
        shortcut: "ctrl+c",
        takes_args: false,
    },
    CommandSpec {
        name: "quit",
        title: "Quit",
        description: "Alias for exit",
        shortcut: "ctrl+c",
        takes_args: false,
    },
];

/// Leader (ctrl+x) sequences time out after this window, matching the TS
/// TUI's default leader hold.
pub const LEADER_TIMEOUT: Duration = Duration::from_millis(1500);

pub struct App {
    pub dialog: Dialog,
    pub route: Route,
    pub leader: Option<Instant>,
    pub should_quit: bool,

    pub session_id: Option<String>,
    pub session_title: String,
    pub agent: String,
    pub agent_index: usize,
    pub model: String,
    pub provider: String,
    pub directory: String,
    pub version: String,

    pub messages: Vec<Value>,
    pub busy: bool,
    pub interrupts: usize,
    /// Lines scrolled up from the bottom of the transcript.
    pub scroll: u16,

    pub input: String,
    pub cursor: usize,

    pub sessions: Vec<Value>,
    pub agents: Vec<Value>,
    pub models: Vec<(String, String)>,
    pub todos: Vec<Value>,
    pub list_index: usize,
    pub search: String,

    /// Sent prompts, oldest first. Up/Down at the beginning of an empty
    /// editor line walks this history like a shell.
    pub prompt_history: Vec<String>,
    pub history_cursor: Option<usize>,
    /// Draft preserved while walking history so Down returns to it.
    pub history_draft: Option<String>,

    pub autocomplete: Option<Autocomplete>,
    /// Server-loaded dynamic slash commands (skill sources already filtered).
    pub server_commands: Vec<DynamicCommand>,
    /// Pointer-input tracker for the autocomplete popover: a Moved event
    /// flips to Mouse mode so hover updates the selection; every keypress
    /// flips back to Keyboard so a stale hover cannot hijack it.
    pub input_mode: InputMode,

    pub toast: Option<String>,
    pub frame: usize,
    pub tip_index: usize,
    /// session.metadata.goal, when goal mode has durable state.
    pub goal: Option<Value>,
    /// The fork retains the last goal chip after completion until the next
    /// non-goal prompt, so completion state stays visible.
    pub retained_goal: Option<Value>,
    /// Fork out-of-workspace toggle state for the current session.
    pub external_allowed: bool,

    /// The hit target under the pointer this frame.
    pub hover: Option<HitTarget>,
    /// The hit target currently being pressed (mouse-down without a matching
    /// up yet). The renderer paints this bolder to give click feedback.
    pub pressed: Option<HitTarget>,

    /// Deterministic hit rectangles published by the renderer each frame
    /// and consumed by the mouse handler.
    pub geometry: Geometry,
}

impl App {
    pub fn new(directory: String) -> App {
        App {
            dialog: Dialog::None,
            route: Route::Home,
            leader: None,
            should_quit: false,
            session_id: None,
            session_title: String::new(),
            agent: "build".into(),
            agent_index: 0,
            model: "big-pickle".into(),
            provider: "opencode".into(),
            directory,
            version: env!("CARGO_PKG_VERSION").to_string(),
            messages: vec![],
            busy: false,
            interrupts: 0,
            scroll: 0,
            input: String::new(),
            cursor: 0,
            sessions: vec![],
            agents: vec![],
            models: vec![],
            todos: vec![],
            list_index: 0,
            search: String::new(),
            prompt_history: vec![],
            history_cursor: None,
            history_draft: None,
            autocomplete: None,
            server_commands: vec![],
            input_mode: InputMode::Keyboard,
            toast: None,
            frame: 0,
            tip_index: std::process::id() as usize,
            goal: None,
            retained_goal: None,
            external_allowed: false,
            hover: None,
            pressed: None,
            geometry: Geometry::default(),
        }
    }

    /// The goal the footer chip displays: live metadata, else the retained
    /// snapshot from before completion.
    pub fn display_goal(&self) -> Option<&Value> {
        self.goal.as_ref().or(self.retained_goal.as_ref())
    }

    pub fn is_home(&self) -> bool {
        self.route == Route::Home
    }

    /// Return to Home without touching the server. Any active session
    /// remains addressable via `/sessions`, but the UI shows the logo and
    /// the next prompt will atomically create a fresh session.
    pub fn go_home(&mut self) {
        self.route = Route::Home;
        self.session_id = None;
        self.session_title.clear();
        self.messages.clear();
        self.todos.clear();
        self.scroll = 0;
        self.interrupts = 0;
        self.goal = None;
        self.retained_goal = None;
        self.busy = false;
        self.autocomplete = None;
        self.history_cursor = None;
        self.history_draft = None;
    }

    pub fn adopt_session(&mut self, session: &Value) {
        self.session_id = session
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string);
        self.session_title = session
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if let Some(agent) = session.get("agent").and_then(Value::as_str) {
            self.agent = agent.to_string();
        }
        if let Some(model) = session.get("model") {
            if let Some(id) = model.get("id").and_then(Value::as_str) {
                self.model = id.to_string();
            }
            if let Some(provider) = model.get("providerID").and_then(Value::as_str) {
                self.provider = provider.to_string();
            }
        }
        self.messages.clear();
        self.todos.clear();
        self.scroll = 0;
        self.interrupts = 0;
        self.goal = None;
        self.retained_goal = None;
        self.route = Route::Session;
    }

    /// agent_cycle (tab): rotate through the primary agent cycle order.
    /// Home cycles are local-only; the caller decides whether to push the
    /// change to the server (only meaningful in a live session).
    pub fn cycle_agent(&mut self, reverse: bool) -> String {
        const ORDER: [&str; 3] = ["build", "plan", "goal"];
        let current = ORDER
            .iter()
            .position(|name| *name == self.agent)
            .unwrap_or(0);
        let next = if reverse {
            (current + ORDER.len() - 1) % ORDER.len()
        } else {
            (current + 1) % ORDER.len()
        };
        self.agent = ORDER[next].to_string();
        self.agent.clone()
    }

    pub fn open_dialog(&mut self, dialog: Dialog) {
        self.dialog = dialog;
        self.list_index = 0;
        self.search.clear();
        self.autocomplete = None;
    }

    pub fn insert(&mut self, ch: char) {
        self.input.insert(self.byte_cursor(), ch);
        self.cursor += 1;
        self.history_cursor = None;
        self.history_draft = None;
        self.sync_autocomplete();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let byte = self
            .input
            .char_indices()
            .nth(self.cursor - 1)
            .map(|(index, _)| index)
            .unwrap_or(0);
        self.input.remove(byte);
        self.cursor -= 1;
        self.history_cursor = None;
        self.history_draft = None;
        self.sync_autocomplete();
    }

    pub fn delete_forward(&mut self) {
        if self.cursor >= self.input.chars().count() {
            return;
        }
        let byte = self.byte_cursor();
        self.input.remove(byte);
        self.sync_autocomplete();
    }

    pub fn delete_to_line_start(&mut self) {
        let (start, _) = self.line_bounds();
        let end_byte = self.byte_cursor();
        let start_char = self
            .input
            .char_indices()
            .position(|(byte, _)| byte >= start)
            .unwrap_or_else(|| self.input.chars().count());
        self.input.replace_range(start..end_byte, "");
        self.cursor = start_char;
        self.sync_autocomplete();
    }

    pub fn delete_to_line_end(&mut self) {
        let (_, end) = self.line_bounds();
        let start = self.byte_cursor();
        if start >= end {
            return;
        }
        self.input.replace_range(start..end, "");
        self.sync_autocomplete();
    }

    pub fn delete_word_back(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let target = self.word_start(self.cursor);
        let target_byte = self
            .input
            .char_indices()
            .nth(target)
            .map(|(byte, _)| byte)
            .unwrap_or(0);
        let cursor_byte = self.byte_cursor();
        self.input.replace_range(target_byte..cursor_byte, "");
        self.cursor = target;
        self.sync_autocomplete();
    }

    fn byte_cursor(&self) -> usize {
        self.input
            .char_indices()
            .nth(self.cursor)
            .map(|(index, _)| index)
            .unwrap_or(self.input.len())
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let count = self.input.chars().count() as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, count) as usize;
    }

    /// The character offsets for the current line's start and end, in
    /// bytes (so `String::replace_range` can address them directly).
    fn line_bounds(&self) -> (usize, usize) {
        let byte = self.byte_cursor();
        let start = self.input[..byte].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let end = self.input[byte..]
            .find('\n')
            .map(|i| byte + i)
            .unwrap_or(self.input.len());
        (start, end)
    }

    /// Word-start jump: skip whitespace, then walk to the beginning of the
    /// current alphanumeric run (like readline `M-b`).
    pub fn word_start(&self, from: usize) -> usize {
        let chars: Vec<char> = self.input.chars().collect();
        let mut index = from.min(chars.len());
        while index > 0 && chars[index - 1].is_whitespace() {
            index -= 1;
        }
        while index > 0 && !chars[index - 1].is_whitespace() {
            index -= 1;
        }
        index
    }

    pub fn word_end(&self, from: usize) -> usize {
        let chars: Vec<char> = self.input.chars().collect();
        let mut index = from.min(chars.len());
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        while index < chars.len() && !chars[index].is_whitespace() {
            index += 1;
        }
        index
    }

    pub fn agent_color(&self) -> ratatui::style::Color {
        crate::theme::agent_color(&self.agent, self.agent_index)
    }

    // ---------------------------------------------------------------------
    // Prompt history
    // ---------------------------------------------------------------------

    pub fn push_history(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        if self.prompt_history.last().map(String::as_str) == Some(text.as_str()) {
            return;
        }
        self.prompt_history.push(text);
    }

    pub fn history_prev(&mut self) -> bool {
        if self.prompt_history.is_empty() {
            return false;
        }
        let next = match self.history_cursor {
            Some(0) => return true,
            Some(index) => index - 1,
            None => {
                self.history_draft = Some(self.input.clone());
                self.prompt_history.len().saturating_sub(1)
            }
        };
        self.history_cursor = Some(next);
        self.input = self.prompt_history[next].clone();
        self.cursor = self.input.chars().count();
        self.autocomplete = None;
        true
    }

    pub fn history_next(&mut self) -> bool {
        let Some(index) = self.history_cursor else {
            return false;
        };
        let last = self.prompt_history.len().saturating_sub(1);
        if index >= last {
            self.history_cursor = None;
            self.input = self.history_draft.take().unwrap_or_default();
            self.cursor = self.input.chars().count();
            return true;
        }
        self.history_cursor = Some(index + 1);
        self.input = self.prompt_history[index + 1].clone();
        self.cursor = self.input.chars().count();
        true
    }

    // ---------------------------------------------------------------------
    // Autocomplete
    // ---------------------------------------------------------------------

    /// Sync the autocomplete popover to the current input. Called after
    /// every input mutation. The popover opens only when the input begins
    /// with a bare `/` (no leading whitespace) and closes once whitespace
    /// appears after the trigger, matching the TS TUI behavior.
    pub fn sync_autocomplete(&mut self) {
        let Some(rest) = self.input.strip_prefix('/') else {
            self.autocomplete = None;
            return;
        };
        if rest.contains(char::is_whitespace) {
            self.autocomplete = None;
            return;
        }
        let filter = rest.to_string();
        let entries = autocomplete_entries(&filter, &self.server_commands);
        let carry_index = self
            .autocomplete
            .as_ref()
            .filter(|current| current.filter == filter)
            .map(|current| current.index)
            .unwrap_or(0);
        let index = if entries.is_empty() {
            0
        } else {
            carry_index.min(entries.len() - 1)
        };
        let mut auto = Autocomplete {
            index,
            scroll: self
                .autocomplete
                .as_ref()
                .map(|current| current.scroll)
                .unwrap_or(0),
            filter,
            entries,
        };
        auto.ensure_visible();
        self.autocomplete = Some(auto);
    }

    /// Replace the current `/word` fragment with the selected entry name,
    /// leaving a trailing space in place for arg-taking commands. Returns
    /// the committed entry so the caller can decide whether to submit.
    pub fn complete_autocomplete(&mut self, keep_open: bool) -> Option<AutocompleteEntry> {
        let auto = self.autocomplete.as_ref()?;
        let entry = auto.entries.get(auto.index)?.clone();
        let suffix = if entry.takes_args { " " } else { "" };
        self.input = format!("/{}{suffix}", entry.name);
        self.cursor = self.input.chars().count();
        if keep_open && entry.takes_args {
            self.sync_autocomplete();
        } else {
            self.autocomplete = None;
        }
        Some(entry)
    }
}

/// Build the merged, ranked autocomplete entries for `filter`. Built-in
/// palette entries and dynamic server commands are scored using a fuzzy
/// prefix/subsequence rule that boosts full prefix matches, keeps
/// subsequence matches visible, and preserves an exact-name match at the
/// top of the list.
pub fn autocomplete_entries(
    filter: &str,
    server_commands: &[DynamicCommand],
) -> Vec<AutocompleteEntry> {
    let mut scored: Vec<(i32, AutocompleteEntry)> = vec![];
    for (index, spec) in COMMANDS.iter().enumerate() {
        let entry = AutocompleteEntry {
            name: spec.name.to_string(),
            description: spec.description.to_string(),
            takes_args: spec.takes_args,
            label: String::new(),
            origin: EntryOrigin::Builtin(index),
        };
        if let Some(score) = fuzzy_score(&entry.name, &entry.description, filter) {
            scored.push((score, entry));
        }
    }
    for (index, command) in server_commands.iter().enumerate() {
        let entry = AutocompleteEntry {
            name: command.name.clone(),
            description: command.description.clone(),
            takes_args: command.takes_args,
            label: command.label.clone(),
            origin: EntryOrigin::Server(index),
        };
        if let Some(score) = fuzzy_score(&entry.name, &entry.description, filter) {
            scored.push((score, entry));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    scored.into_iter().map(|(_, entry)| entry).collect()
}

/// Prefix/subsequence scoring on `name` (primary) and `description`
/// (secondary). Returns `None` when neither matches. Higher is better.
/// An exact match, a full prefix match, and a substring match all score
/// distinctly above a subsequence hit; an empty filter accepts every row.
pub fn fuzzy_score(name: &str, description: &str, filter: &str) -> Option<i32> {
    if filter.is_empty() {
        return Some(0);
    }
    let needle = filter.to_lowercase();
    let name_lc = name.to_lowercase();
    if let Some(score) = name_score(&name_lc, &needle) {
        return Some(1_000 + score - name.len() as i32);
    }
    // Description acts as a secondary key but never as a subsequence:
    // single-letter needles like "s" would otherwise light up every entry
    // whose description contains that letter, drowning out real hits.
    if needle.chars().count() >= 3 {
        let description_lc = description.to_lowercase();
        if description_word_prefix(&description_lc, &needle) || description_lc.contains(&needle) {
            return Some(200 - description.len() as i32 / 8);
        }
    }
    None
}

fn description_word_prefix(target: &str, needle: &str) -> bool {
    target
        .split(|ch: char| !ch.is_alphanumeric())
        .any(|word| word.starts_with(needle))
}

fn name_score(target: &str, needle: &str) -> Option<i32> {
    if target == needle {
        return Some(10_000);
    }
    if target.starts_with(needle) {
        return Some(5_000 + needle.len() as i32 * 10);
    }
    if target.contains(needle) {
        return Some(2_000 + needle.len() as i32 * 5);
    }
    subsequence_score(target, needle)
}

fn subsequence_score(target: &str, needle: &str) -> Option<i32> {
    let mut chars = target.chars();
    let mut positions = vec![];
    let mut cursor = 0usize;
    for wanted in needle.chars() {
        let mut found = false;
        for ch in chars.by_ref() {
            cursor += 1;
            if ch == wanted {
                positions.push(cursor - 1);
                found = true;
                break;
            }
        }
        if !found {
            return None;
        }
    }
    // Tighter clusters (smaller total span) score higher, matched-count
    // matters too, and later starts penalize slightly.
    let span = positions.last()? - positions.first()?;
    Some(500 + needle.len() as i32 * 4 - span as i32 - positions[0] as i32 / 2)
}

/// Case-insensitive subsequence match, like the DialogSelect fuzzy filter.
pub fn fuzzy(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let haystack = haystack.to_lowercase();
    let mut chars = haystack.chars();
    needle
        .to_lowercase()
        .chars()
        .all(|wanted| chars.by_ref().any(|ch| ch == wanted))
}
