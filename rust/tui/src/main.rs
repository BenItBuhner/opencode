//! opencode-tui: Rust TUI client for the OpenCode server, built on the
//! specialized Rust terminal stack (ratatui rendering + crossterm backend) —
//! the counterpart of the TS TUI's OpenTUI renderer and keymap.
//!
//! Interaction model mirrors packages/tui: a chat route with a prompt editor,
//! modal list dialogs, and the default keybinds from
//! packages/tui/src/config/keybind.ts (leader ctrl+x; app_exit ctrl+c /
//! <leader>q; session_new <leader>n; session_list <leader>l; agent_list
//! <leader>a; model_list <leader>m; messages_page_up/down pageup/pagedown;
//! prompt submit enter; newline alt+enter; interrupt esc).
//!
//! Usage: opencode-tui [--url http://127.0.0.1:4097] [--directory <cwd>] [--session <id>]

mod api;
mod state;
mod ui;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use serde_json::Value;
use state::{App, Screen};
use std::sync::mpsc;
use std::time::Duration;

enum Msg {
    Sessions(Vec<Value>),
    Session(Value),
    Messages(String, Vec<Value>),
    Active(Vec<String>),
    Agents(Vec<Value>),
    Models(Vec<(String, String)>),
    Toast(String),
}

enum Cmd {
    Refresh,
    Prompt(String, String),
    Interrupt(String),
    NewSession,
    LoadLists,
    SwitchAgent(String, String),
    SwitchModel(String, String, String),
}

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|item| item == name)
        .and_then(|index| args.get(index + 1).cloned())
}

fn main() -> std::io::Result<()> {
    let base = arg("--url")
        .or_else(|| std::env::var("OPENCODE_URL").ok())
        .unwrap_or_else(|| "http://127.0.0.1:4097".into());
    let directory = arg("--directory").unwrap_or_else(|| {
        std::env::current_dir()
            .expect("cwd")
            .to_string_lossy()
            .into_owned()
    });

    let (to_ui, from_worker) = mpsc::channel::<Msg>();
    let (to_worker, from_ui) = mpsc::channel::<Cmd>();
    let worker_base = base.clone();
    let worker_directory = directory.clone();
    let session_flag = arg("--session");
    std::thread::spawn(move || worker(worker_base, worker_directory, session_flag, to_ui, from_ui));

    let mut terminal = ratatui::init();
    let mut app = App::new(directory);
    let _ = to_worker.send(Cmd::LoadLists);
    let _ = to_worker.send(Cmd::Refresh);

    while !app.should_quit {
        terminal.draw(|frame| ui::draw(frame, &app))?;
        while let Ok(message) = from_worker.try_recv() {
            apply(&mut app, message);
        }
        if crossterm::event::poll(Duration::from_millis(80))? {
            if let Event::Key(key) = crossterm::event::read()? {
                if key.kind == KeyEventKind::Press {
                    handle_key(&mut app, key.code, key.modifiers, &to_worker);
                }
            }
        }
        app.spinner_frame = app.spinner_frame.wrapping_add(1);
    }
    ratatui::restore();
    Ok(())
}

fn apply(app: &mut App, message: Msg) {
    match message {
        Msg::Sessions(sessions) => app.sessions = sessions,
        Msg::Session(session) => app.adopt_session(&session),
        Msg::Messages(session_id, messages) => {
            if app.session_id.as_deref() == Some(session_id.as_str()) {
                app.messages = messages;
            }
        }
        Msg::Active(active) => {
            app.busy = app
                .session_id
                .as_deref()
                .is_some_and(|id| active.iter().any(|item| item == id));
        }
        Msg::Agents(agents) => app.agents = agents,
        Msg::Models(models) => app.models = models,
        Msg::Toast(text) => app.toast = Some(text),
    }
}

fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers, worker: &mpsc::Sender<Cmd>) {
    // app_exit: ctrl+c always quits.
    if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return;
    }
    // Leader sequence (ctrl+x, then one key), like the TS keymap.
    if code == KeyCode::Char('x') && modifiers.contains(KeyModifiers::CONTROL) {
        app.leader = true;
        return;
    }
    if app.leader {
        app.leader = false;
        match code {
            KeyCode::Char('q') => app.should_quit = true,
            KeyCode::Char('n') => {
                let _ = worker.send(Cmd::NewSession);
            }
            KeyCode::Char('l') => {
                app.screen = Screen::SessionList;
                app.list_index = 0;
            }
            KeyCode::Char('a') => {
                app.screen = Screen::AgentList;
                app.list_index = 0;
            }
            KeyCode::Char('m') => {
                app.screen = Screen::ModelList;
                app.list_index = 0;
            }
            KeyCode::Char('?') => app.screen = Screen::Help,
            _ => app.toast = Some("Unbound leader key".into()),
        }
        return;
    }

    match app.screen {
        Screen::Chat => handle_chat_key(app, code, modifiers, worker),
        Screen::Help => {
            if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
                app.screen = Screen::Chat;
            }
        }
        _ => handle_dialog_key(app, code, worker),
    }
}

fn handle_chat_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
    worker: &mpsc::Sender<Cmd>,
) {
    match code {
        KeyCode::Enter if modifiers.contains(KeyModifiers::ALT) => {
            app.insert('\n');
        }
        KeyCode::Enter => {
            let text = app.input.trim().to_string();
            if text.is_empty() {
                return;
            }
            let Some(session_id) = app.session_id.clone() else {
                let _ = worker.send(Cmd::NewSession);
                app.toast = Some("Creating session…".into());
                return;
            };
            app.input.clear();
            app.cursor = 0;
            app.scroll = 0;
            app.busy = true;
            let _ = worker.send(Cmd::Prompt(session_id, text));
        }
        KeyCode::Esc => {
            if app.busy {
                if let Some(session_id) = app.session_id.clone() {
                    let _ = worker.send(Cmd::Interrupt(session_id));
                    app.toast = Some("Interrupt requested".into());
                }
            } else {
                app.input.clear();
                app.cursor = 0;
            }
        }
        KeyCode::Backspace => app.backspace(),
        KeyCode::Left => app.move_cursor(-1),
        KeyCode::Right => app.move_cursor(1),
        KeyCode::Home => app.cursor = 0,
        KeyCode::End => app.cursor = app.input.chars().count(),
        KeyCode::PageUp => app.scroll = app.scroll.saturating_add(10),
        KeyCode::PageDown => app.scroll = app.scroll.saturating_sub(10),
        KeyCode::Char(ch) => {
            app.toast = None;
            app.insert(ch);
        }
        _ => {}
    }
}

fn handle_dialog_key(app: &mut App, code: KeyCode, worker: &mpsc::Sender<Cmd>) {
    let length = match app.screen {
        Screen::SessionList => app.sessions.len(),
        Screen::AgentList => app.agents.len(),
        Screen::ModelList => app.models.len(),
        _ => 0,
    };
    match code {
        KeyCode::Esc | KeyCode::Char('q') => app.screen = Screen::Chat,
        KeyCode::Up | KeyCode::Char('k') => {
            app.list_index = app.list_index.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.list_index + 1 < length {
                app.list_index += 1;
            }
        }
        KeyCode::Enter => {
            match app.screen {
                Screen::SessionList => {
                    if let Some(session) = app.sessions.get(app.list_index).cloned() {
                        app.adopt_session(&session);
                        let _ = worker.send(Cmd::Refresh);
                    }
                }
                Screen::AgentList => {
                    let agent = app.agents.get(app.list_index).and_then(|agent| {
                        agent
                            .get("id")
                            .or_else(|| agent.get("name"))
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    });
                    if let (Some(agent), Some(session_id)) = (agent, app.session_id.clone()) {
                        app.agent = agent.clone();
                        let _ = worker.send(Cmd::SwitchAgent(session_id, agent));
                    }
                }
                Screen::ModelList => {
                    if let (Some((provider, model)), Some(session_id)) = (
                        app.models.get(app.list_index).cloned(),
                        app.session_id.clone(),
                    ) {
                        app.model = format!("{provider}/{model}");
                        let _ = worker.send(Cmd::SwitchModel(session_id, provider, model));
                    }
                }
                _ => {}
            }
            app.screen = Screen::Chat;
        }
        _ => {}
    }
}

/// Worker thread: owns every blocking HTTP call. Polls messages + active
/// state on an interval (fast while a drain is running) and executes commands.
fn worker(
    base: String,
    directory: String,
    session_flag: Option<String>,
    to_ui: mpsc::Sender<Msg>,
    from_ui: mpsc::Receiver<Cmd>,
) {
    let api = api::Api { base };
    let mut session_id: Option<String> = None;
    let mut busy = false;

    // Adopt --session, else the newest session for the directory, else create.
    let adopt = match session_flag {
        Some(id) => api.session(&id).ok().filter(|value| !value.is_null()),
        None => api.sessions().ok().and_then(|sessions| {
            sessions.into_iter().find(|session| {
                session
                    .get("location")
                    .and_then(|location| location.get("directory"))
                    .and_then(Value::as_str)
                    == Some(directory.as_str())
            })
        }),
    };
    let adopt = match adopt {
        Some(session) => Some(session),
        None => api.create_session(&directory).ok(),
    };
    if let Some(session) = adopt {
        session_id = session
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let _ = to_ui.send(Msg::Session(session));
    }

    loop {
        let timeout = if busy {
            Duration::from_millis(250)
        } else {
            Duration::from_millis(1_000)
        };
        match from_ui.recv_timeout(timeout) {
            Ok(Cmd::Prompt(id, text)) => {
                session_id = Some(id.clone());
                busy = true;
                if let Err(error) = api.prompt(&id, &text) {
                    let _ = to_ui.send(Msg::Toast(format!("prompt failed: {error}")));
                }
            }
            Ok(Cmd::Interrupt(id)) => {
                if let Err(error) = api.interrupt(&id) {
                    let _ = to_ui.send(Msg::Toast(format!("interrupt failed: {error}")));
                }
            }
            Ok(Cmd::NewSession) => match api.create_session(&directory) {
                Ok(session) => {
                    session_id = session
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let _ = to_ui.send(Msg::Session(session));
                }
                Err(error) => {
                    let _ = to_ui.send(Msg::Toast(format!("create failed: {error}")));
                }
            },
            Ok(Cmd::LoadLists) => {
                if let Ok(agents) = api.agents() {
                    let _ = to_ui.send(Msg::Agents(agents));
                }
                if let Ok(models) = api.models() {
                    let _ = to_ui.send(Msg::Models(models));
                }
            }
            Ok(Cmd::SwitchAgent(id, agent)) => {
                if let Err(error) = api.switch_agent(&id, &agent) {
                    let _ = to_ui.send(Msg::Toast(format!("agent switch failed: {error}")));
                }
            }
            Ok(Cmd::SwitchModel(id, provider, model)) => {
                if let Err(error) = api.switch_model(&id, &provider, &model) {
                    let _ = to_ui.send(Msg::Toast(format!("model switch failed: {error}")));
                }
            }
            Ok(Cmd::Refresh) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }

        if let Ok(sessions) = api.sessions() {
            let _ = to_ui.send(Msg::Sessions(sessions));
        }
        if let Ok(active) = api.active() {
            busy = session_id
                .as_deref()
                .is_some_and(|id| active.iter().any(|item| item == id));
            let _ = to_ui.send(Msg::Active(active));
        }
        if let Some(id) = &session_id {
            if let Ok(messages) = api.messages(id) {
                let _ = to_ui.send(Msg::Messages(id.clone(), messages));
            }
        }
    }
}
