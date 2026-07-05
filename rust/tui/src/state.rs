//! Application state shared between the input loop and the renderer,
//! mirroring the TS TUI's routes (home/session), leader-key sequences,
//! DialogSelect filtering, and the prompt editor.

use serde_json::Value;

#[derive(PartialEq, Clone, Copy)]
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

pub struct App {
    pub dialog: Dialog,
    pub leader: bool,
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
}

impl App {
    pub fn new(directory: String) -> App {
        App {
            dialog: Dialog::None,
            leader: false,
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
            toast: None,
            frame: 0,
            tip_index: std::process::id() as usize,
            goal: None,
            retained_goal: None,
            external_allowed: false,
        }
    }

    /// The goal the footer chip displays: live metadata, else the retained
    /// snapshot from before completion.
    pub fn display_goal(&self) -> Option<&Value> {
        self.goal.as_ref().or(self.retained_goal.as_ref())
    }

    /// The home route shows until the session has visible messages.
    pub fn is_home(&self) -> bool {
        self.messages.is_empty()
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
    }

    /// agent_cycle (tab): rotate through the primary agent cycle order.
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
    }

    pub fn insert(&mut self, ch: char) {
        self.input.insert(self.byte_cursor(), ch);
        self.cursor += 1;
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

    pub fn agent_color(&self) -> ratatui::style::Color {
        crate::theme::agent_color(&self.agent, self.agent_index)
    }
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
