//! Application state shared between the input loop and the renderer.
//! Mirrors the TS TUI's interaction model: one focused chat route, modal list
//! dialogs above it, a leader-key sequence (ctrl+x), and a prompt editor.

use serde_json::Value;

#[derive(PartialEq, Clone, Copy)]
pub enum Screen {
    Chat,
    SessionList,
    AgentList,
    ModelList,
    Help,
}

pub struct App {
    pub screen: Screen,
    pub leader: bool,
    pub should_quit: bool,

    pub session_id: Option<String>,
    pub session_title: String,
    pub agent: String,
    pub model: String,
    pub directory: String,

    pub messages: Vec<Value>,
    pub busy: bool,
    /// Lines scrolled up from the bottom of the transcript.
    pub scroll: u16,

    pub input: String,
    pub cursor: usize,

    pub sessions: Vec<Value>,
    pub agents: Vec<Value>,
    pub models: Vec<(String, String)>,
    pub list_index: usize,

    pub toast: Option<String>,
    pub spinner_frame: usize,
}

impl App {
    pub fn new(directory: String) -> App {
        App {
            screen: Screen::Chat,
            leader: false,
            should_quit: false,
            session_id: None,
            session_title: String::new(),
            agent: "build".into(),
            model: "opencode/big-pickle".into(),
            directory,
            messages: vec![],
            busy: false,
            scroll: 0,
            input: String::new(),
            cursor: 0,
            sessions: vec![],
            agents: vec![],
            models: vec![],
            list_index: 0,
            toast: None,
            spinner_frame: 0,
        }
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
            let provider = model
                .get("providerID")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let id = model.get("id").and_then(Value::as_str).unwrap_or_default();
            if !id.is_empty() {
                self.model = format!("{provider}/{id}");
            }
        }
        self.messages.clear();
        self.scroll = 0;
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
}
