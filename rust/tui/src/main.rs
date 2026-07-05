//! opencode-tui: Rust port of the OpenCode TUI (packages/tui), rendered with
//! the specialized Rust terminal stack — ratatui + crossterm — reproducing
//! the OpenTUI-based design: the block-letter wordmark home route, `┃`
//! gutters and per-tool icon rows in the transcript, the accent-bordered
//! prompt editor with agent row and connector, the block busy spinner, and
//! centered DialogSelect panels with fuzzy search.
//!
//! Default keybinds follow packages/tui/src/config/keybind.ts: leader ctrl+x;
//! exit ctrl+c / <leader>q; <leader>n new session; <leader>l sessions;
//! <leader>a agents; <leader>m models; ctrl+p command palette;
//! pageup/pagedown scroll; enter send; alt+enter newline; esc interrupt.
//!
//! Usage: opencode-tui [--url http://127.0.0.1:4097] [--directory <cwd>] [--session <id>]

mod api;
mod logo;
mod state;
mod theme;
mod ui;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use serde_json::Value;
use state::{App, Dialog};
use std::sync::mpsc;
use std::time::Duration;

enum Msg {
    Sessions(Vec<Value>),
    Session(Value),
    Messages(String, Vec<Value>),
    Todos(String, Vec<Value>),
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

    // The TUI paints its own theme like OpenTUI does; never let NO_COLOR
    // strip the palette out from under the renderer.
    crossterm::style::force_color_output(true);
    let mut terminal = ratatui::init();
    let mut app = App::new(directory);
    let _ = to_worker.send(Cmd::LoadLists);
    let _ = to_worker.send(Cmd::Refresh);

    while !app.should_quit {
        terminal.draw(|frame| ui::draw(frame, &app))?;
        while let Ok(message) = from_worker.try_recv() {
            apply(&mut app, message);
        }
        if crossterm::event::poll(Duration::from_millis(40))? {
            if let Event::Key(key) = crossterm::event::read()? {
                if key.kind == KeyEventKind::Press {
                    handle_key(&mut app, key.code, key.modifiers, &to_worker);
                }
            }
        }
        app.frame = app.frame.wrapping_add(1);
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
        Msg::Todos(session_id, todos) => {
            if app.session_id.as_deref() == Some(session_id.as_str()) {
                app.todos = todos;
            }
        }
        Msg::Active(active) => {
            let busy = app
                .session_id
                .as_deref()
                .is_some_and(|id| active.iter().any(|item| item == id));
            if !busy {
                app.interrupts = 0;
            }
            app.busy = busy;
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
    // command_list: ctrl+p opens the command palette.
    if code == KeyCode::Char('p') && modifiers.contains(KeyModifiers::CONTROL) {
        app.open_dialog(Dialog::Commands);
        return;
    }
    // Leader sequence (ctrl+x, then one key).
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
            KeyCode::Char('l') => app.open_dialog(Dialog::Sessions),
            KeyCode::Char('a') => app.open_dialog(Dialog::Agents),
            KeyCode::Char('m') => app.open_dialog(Dialog::Models),
            KeyCode::Char('?') => app.open_dialog(Dialog::Help),
            _ => app.toast = Some("Unbound leader key".into()),
        }
        return;
    }

    match app.dialog {
        Dialog::None => handle_chat_key(app, code, modifiers, worker),
        Dialog::Help => {
            if matches!(code, KeyCode::Esc | KeyCode::Enter) {
                app.dialog = Dialog::None;
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
        KeyCode::Enter if modifiers.contains(KeyModifiers::ALT) => app.insert('\n'),
        KeyCode::Enter => {
            let text = app.input.trim().to_string();
            if text.is_empty() {
                return;
            }
            // Slash commands, like the TS prompt: /new /sessions /agents /models /help.
            if let Some(command) = text.strip_prefix('/') {
                app.input.clear();
                app.cursor = 0;
                match command {
                    "new" => {
                        let _ = worker.send(Cmd::NewSession);
                    }
                    "sessions" => app.open_dialog(Dialog::Sessions),
                    "agents" => app.open_dialog(Dialog::Agents),
                    "models" => app.open_dialog(Dialog::Models),
                    "help" => app.open_dialog(Dialog::Help),
                    "exit" | "quit" => app.should_quit = true,
                    other => app.toast = Some(format!("Unknown command: /{other}")),
                }
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
            app.toast = None;
            let _ = worker.send(Cmd::Prompt(session_id, text));
        }
        KeyCode::Esc => {
            if app.busy {
                if let Some(session_id) = app.session_id.clone() {
                    app.interrupts += 1;
                    let _ = worker.send(Cmd::Interrupt(session_id));
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
    let options = ui::dialog_options(app);
    match code {
        KeyCode::Esc => app.dialog = Dialog::None,
        KeyCode::Up => app.list_index = app.list_index.saturating_sub(1),
        KeyCode::Down => {
            if app.list_index + 1 < options.len() {
                app.list_index += 1;
            }
        }
        KeyCode::Backspace => {
            app.search.pop();
            app.list_index = 0;
        }
        KeyCode::Char(ch) => {
            app.search.push(ch);
            app.list_index = 0;
        }
        KeyCode::Enter => {
            let selected = app.list_index.min(options.len().saturating_sub(1));
            let dialog = app.dialog;
            app.dialog = Dialog::None;
            match dialog {
                Dialog::Commands => {
                    let Some(option) = options.get(selected) else {
                        return;
                    };
                    match option.title.as_str() {
                        "New session" => {
                            let _ = worker.send(Cmd::NewSession);
                        }
                        "Switch session" => app.open_dialog(Dialog::Sessions),
                        "Switch agent" => app.open_dialog(Dialog::Agents),
                        "Switch model" => app.open_dialog(Dialog::Models),
                        "Help" => app.open_dialog(Dialog::Help),
                        _ => app.should_quit = true,
                    }
                }
                Dialog::Sessions => {
                    // Map the filtered index back to the session list.
                    let filtered: Vec<&Value> = app
                        .sessions
                        .iter()
                        .filter(|session| {
                            state::fuzzy(
                                session.get("title").and_then(Value::as_str).unwrap_or(""),
                                &app.search,
                            )
                        })
                        .collect();
                    if let Some(session) = filtered.get(selected).cloned().cloned() {
                        app.adopt_session(&session);
                        let _ = worker.send(Cmd::Refresh);
                    }
                }
                Dialog::Agents => {
                    let filtered: Vec<(usize, &Value)> = app
                        .agents
                        .iter()
                        .enumerate()
                        .filter(|(_, agent)| {
                            state::fuzzy(
                                &format!(
                                    "{} {}",
                                    agent
                                        .get("id")
                                        .or_else(|| agent.get("name"))
                                        .and_then(Value::as_str)
                                        .unwrap_or(""),
                                    agent
                                        .get("description")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                ),
                                &app.search,
                            )
                        })
                        .collect();
                    if let Some((index, agent)) = filtered.get(selected) {
                        let name = agent
                            .get("id")
                            .or_else(|| agent.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or("build")
                            .to_string();
                        app.agent = name.clone();
                        app.agent_index = *index;
                        if let Some(session_id) = app.session_id.clone() {
                            let _ = worker.send(Cmd::SwitchAgent(session_id, name));
                        }
                    }
                }
                Dialog::Models => {
                    let filtered: Vec<&(String, String)> = app
                        .models
                        .iter()
                        .filter(|(provider, model)| {
                            state::fuzzy(&format!("{provider}/{model}"), &app.search)
                        })
                        .collect();
                    if let Some((provider, model)) = filtered.get(selected) {
                        app.provider = provider.clone();
                        app.model = model.clone();
                        if let Some(session_id) = app.session_id.clone() {
                            let _ = worker.send(Cmd::SwitchModel(
                                session_id,
                                provider.clone(),
                                model.clone(),
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// Worker thread: owns every blocking HTTP call. Polls messages, todos, and
/// active state (fast while a drain runs) and executes commands.
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
            Duration::from_millis(200)
        } else {
            Duration::from_millis(900)
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
            if let Ok(todos) = api.todos(id) {
                let _ = to_ui.send(Msg::Todos(id.clone(), todos));
            }
        }
    }
}
