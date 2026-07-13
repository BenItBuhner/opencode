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

/// Slash-command autocomplete popover state. Opens when the input starts
/// with `/`, filters as the user types, and closes on Esc, on space (after
/// a full word), or when the leading `/` is removed.
pub struct Autocomplete {
    pub index: usize,
    pub filter: String,
    pub matches: Vec<usize>,
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
    /// with a bare `/` (no leading whitespace).
    pub fn sync_autocomplete(&mut self) {
        if let Some(rest) = self.input.strip_prefix('/') {
            let filter = rest.split_whitespace().next().unwrap_or("").to_string();
            let contains_space = rest.contains(char::is_whitespace);
            let matches = filter_commands(&filter);
            if matches.is_empty() {
                self.autocomplete = None;
                return;
            }
            let single_hit = matches.len() == 1 && COMMANDS[matches[0]].name == filter;
            if single_hit && !COMMANDS[matches[0]].takes_args && !contains_space {
                self.autocomplete = None;
                return;
            }
            if single_hit && contains_space && !COMMANDS[matches[0]].takes_args {
                self.autocomplete = None;
                return;
            }
            let index = self
                .autocomplete
                .as_ref()
                .map(|current| {
                    if current.filter == filter {
                        current.index.min(matches.len().saturating_sub(1))
                    } else {
                        0
                    }
                })
                .unwrap_or(0);
            self.autocomplete = Some(Autocomplete {
                index,
                filter,
                matches,
            });
            return;
        }
        self.autocomplete = None;
    }

    /// Replace the current `/word` fragment with the given command name,
    /// leaving the trailing space in place if the command takes arguments.
    pub fn complete_autocomplete(&mut self, keep_open: bool) -> Option<&'static CommandSpec> {
        let auto = self.autocomplete.as_ref()?;
        let &position = auto.matches.get(auto.index)?;
        let spec = &COMMANDS[position];
        let suffix = if spec.takes_args { " " } else { "" };
        self.input = format!("/{}{suffix}", spec.name);
        self.cursor = self.input.chars().count();
        if keep_open && spec.takes_args {
            self.sync_autocomplete();
        } else {
            self.autocomplete = None;
        }
        Some(spec)
    }
}

fn filter_commands(filter: &str) -> Vec<usize> {
    if filter.is_empty() {
        return (0..COMMANDS.len()).collect();
    }
    let lowered = filter.to_lowercase();
    COMMANDS
        .iter()
        .enumerate()
        .filter(|(_, spec)| {
            spec.name.to_lowercase().starts_with(&lowered)
                || spec
                    .title
                    .split_whitespace()
                    .any(|word| word.to_lowercase().starts_with(&lowered))
        })
        .map(|(index, _)| index)
        .collect()
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
