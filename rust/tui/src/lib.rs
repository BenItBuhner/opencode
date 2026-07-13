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

pub mod api;
mod logo;
pub mod state;
mod theme;
pub mod ui;

use crossterm::event::{
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use serde_json::Value;
use state::{App, Dialog, HitTarget, Route, COMMANDS, LEADER_TIMEOUT};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub enum Msg {
    Sessions(Vec<Value>),
    Session(Value),
    Messages(String, Vec<Value>),
    Todos(String, Vec<Value>),
    Goal(String, Option<Value>),
    Active(Vec<String>),
    Agents(Vec<Value>),
    Models(Vec<(String, String)>),
    Toast(String),
}

/// Commands dispatched from the UI thread to the blocking worker.
#[derive(Debug, PartialEq)]
pub enum Cmd {
    Refresh,
    Prompt(String, String),
    Interrupt(String),
    LoadLists,
    SelectSession(String),
    SwitchAgent(String, String),
    SwitchModel(String, String, String),
    /// Home-route first prompt: atomically create a session bound to the
    /// selected agent/model and send the first user prompt in a single
    /// worker turn. The UI transitions to Session as soon as the created
    /// session is echoed back via `Msg::Session`.
    CreateAndPrompt {
        directory: String,
        agent: String,
        provider: String,
        model: String,
        text: String,
    },
    /// Fork out-of-workspace toggle: write the external_directory session
    /// permission rule (allow <-> ask).
    ToggleExternal(String, bool),
}

pub struct Config {
    pub base: String,
    pub authorization: Option<String>,
    pub directory: String,
    pub session: Option<String>,
}

pub fn run(config: Config) -> std::io::Result<()> {
    let base = config.base;
    let directory = config.directory;

    let (to_ui, from_worker) = mpsc::channel::<Msg>();
    let (to_worker, from_ui) = mpsc::channel::<Cmd>();
    let worker_base = base.clone();
    let worker_directory = directory.clone();
    let worker_auth = config.authorization.clone();
    let session_flag = config.session;
    std::thread::spawn(move || {
        worker(
            worker_base,
            worker_auth,
            worker_directory,
            session_flag,
            to_ui,
            from_ui,
        )
    });

    // The TUI paints its own theme like OpenTUI does; never let NO_COLOR
    // strip the palette out from under the renderer.
    crossterm::style::force_color_output(true);
    let mut terminal = ratatui::init();
    // Mouse: wheel scrolling, hover feedback, click routing, backdrop
    // dismissal. Requires the terminal to report mouse motion.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
    let mut app = App::new(directory);
    let _ = to_worker.send(Cmd::LoadLists);
    let _ = to_worker.send(Cmd::Refresh);

    while !app.should_quit {
        terminal.draw(|frame| ui::draw(frame, &mut app))?;
        while let Ok(message) = from_worker.try_recv() {
            apply(&mut app, message);
        }
        // Leader (ctrl+x) chords time out so a stray press doesn't hijack
        // the next character key.
        if let Some(at) = app.leader {
            if at.elapsed() >= LEADER_TIMEOUT {
                app.leader = None;
            }
        }
        if crossterm::event::poll(Duration::from_millis(40))? {
            match crossterm::event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    handle_key(&mut app, key.code, key.modifiers, &to_worker);
                }
                Event::Mouse(mouse) => {
                    let size = terminal.size()?;
                    handle_mouse(
                        &mut app,
                        mouse,
                        ratatui::layout::Rect::new(0, 0, size.width, size.height),
                        &to_worker,
                    );
                }
                _ => {}
            }
        }
        app.frame = app.frame.wrapping_add(1);
    }
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    ratatui::restore();
    Ok(())
}

// ---------------------------------------------------------------------------
// Mouse
// ---------------------------------------------------------------------------

pub fn handle_mouse(
    app: &mut App,
    mouse: MouseEvent,
    _screen: ratatui::layout::Rect,
    worker: &mpsc::Sender<Cmd>,
) {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            if app.autocomplete.is_some() {
                autocomplete_move(app, -1);
            } else if app.dialog == Dialog::None {
                app.scroll = app.scroll.saturating_add(3);
            } else {
                app.list_index = app.list_index.saturating_sub(1);
            }
        }
        MouseEventKind::ScrollDown => {
            if let Some(auto) = &app.autocomplete {
                if !auto.matches.is_empty() {
                    autocomplete_move(app, 1);
                }
            } else if app.dialog == Dialog::None {
                app.scroll = app.scroll.saturating_sub(3);
            } else {
                let count = ui::dialog_options(app).len();
                if app.list_index + 1 < count {
                    app.list_index += 1;
                }
            }
        }
        MouseEventKind::Moved | MouseEventKind::Drag(_) => {
            let target = hit_test(app, mouse.column, mouse.row);
            app.hover = target;
            update_row_selection(app, target);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let target = hit_test(app, mouse.column, mouse.row);
            app.pressed = target;
            update_row_selection(app, target);
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let pressed = app.pressed.take();
            let target = hit_test(app, mouse.column, mouse.row);
            if pressed.is_some() && pressed != target {
                return;
            }
            let Some(target) = target else {
                if app.dialog != Dialog::None {
                    app.dialog = Dialog::None;
                }
                return;
            };
            match target {
                HitTarget::Row(index) => {
                    if app.autocomplete.is_some() {
                        if let Some(auto) = app.autocomplete.as_mut() {
                            auto.index = index.min(auto.matches.len().saturating_sub(1));
                        }
                        commit_autocomplete(app, worker);
                    } else if app.dialog != Dialog::None {
                        app.list_index = index;
                        handle_dialog_key(app, KeyCode::Enter, KeyModifiers::NONE, worker);
                    }
                }
                HitTarget::Prompt { row, col } => {
                    place_cursor(app, row, col);
                }
                HitTarget::AgentSpan => {
                    app.open_dialog(Dialog::Agents);
                }
                HitTarget::ModelSpan => {
                    app.open_dialog(Dialog::Models);
                }
                HitTarget::GoalChip => {
                    if app.display_goal().is_some() {
                        app.open_dialog(Dialog::GoalDetails);
                    }
                }
                HitTarget::GoalBar => {
                    if app.display_goal().is_some() {
                        app.open_dialog(Dialog::GoalSummaries);
                    }
                }
                HitTarget::TipRow => {
                    app.tip_index = app.tip_index.wrapping_add(1);
                }
            }
        }
        _ => {}
    }
}

fn update_row_selection(app: &mut App, target: Option<HitTarget>) {
    let Some(HitTarget::Row(index)) = target else {
        return;
    };
    if let Some(auto) = app.autocomplete.as_mut() {
        if index < auto.matches.len() {
            auto.index = index;
        }
        return;
    }
    if app.dialog != Dialog::None && index < ui::dialog_options(app).len() {
        app.list_index = index;
    }
}

/// Resolve the topmost hit target under a screen coordinate using the
/// renderer's published geometry.
pub fn hit_test(app: &App, column: u16, row: u16) -> Option<HitTarget> {
    if let (Some(area), Some(auto)) = (app.geometry.autocomplete, &app.autocomplete) {
        if area.contains(column, row) && row >= app.geometry.autocomplete_list_top {
            let relative = (row - app.geometry.autocomplete_list_top) as usize;
            if relative < auto.matches.len() {
                return Some(HitTarget::Row(relative));
            }
        }
    }
    if let Some(area) = app.geometry.dialog_area {
        if area.contains(column, row) {
            if row >= app.geometry.dialog_list_top {
                let relative = (row - app.geometry.dialog_list_top) as usize;
                if let Some(index) = ui::dialog_row_at_offset(app, relative) {
                    return Some(HitTarget::Row(index));
                }
            }
            return None;
        }
    }
    if app.dialog != Dialog::None {
        return None;
    }
    if let Some(area) = app.geometry.goal_bar {
        if area.contains(column, row) {
            return Some(HitTarget::GoalBar);
        }
    }
    if let Some(area) = app.geometry.goal_chip {
        if area.contains(column, row) {
            return Some(HitTarget::GoalChip);
        }
    }
    if let Some(area) = app.geometry.agent_span {
        if area.contains(column, row) {
            return Some(HitTarget::AgentSpan);
        }
    }
    if let Some(area) = app.geometry.model_span {
        if area.contains(column, row) {
            return Some(HitTarget::ModelSpan);
        }
    }
    if let Some(area) = app.geometry.textarea {
        if area.contains(column, row) {
            return Some(HitTarget::Prompt {
                row: row - area.y,
                col: column.saturating_sub(area.x),
            });
        }
    }
    if let Some(area) = app.geometry.tip_row {
        if area.contains(column, row) {
            return Some(HitTarget::TipRow);
        }
    }
    None
}

fn place_cursor(app: &mut App, row: u16, col: u16) {
    let mut current_row = 0u16;
    let mut char_index = 0usize;
    for (index, ch) in app.input.chars().enumerate() {
        if current_row == row {
            let column_index = index - line_start_char(&app.input, index);
            if column_index >= col as usize {
                app.cursor = index;
                return;
            }
        }
        char_index = index + 1;
        if ch == '\n' {
            if current_row == row {
                app.cursor = index;
                return;
            }
            current_row += 1;
        }
    }
    app.cursor = char_index;
}

fn line_start_char(input: &str, char_index: usize) -> usize {
    let chars: Vec<char> = input.chars().take(char_index).collect();
    for (index, ch) in chars.iter().enumerate().rev() {
        if *ch == '\n' {
            return index + 1;
        }
    }
    0
}

// ---------------------------------------------------------------------------
// Apply worker messages
// ---------------------------------------------------------------------------

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
        Msg::Goal(session_id, goal) => {
            if app.session_id.as_deref() == Some(session_id.as_str()) {
                // Retain the last snapshot so the chip survives completion
                // until the next non-goal turn (fork prompt footer behavior).
                if let Some(goal) = &goal {
                    app.retained_goal = Some(goal.clone());
                }
                app.goal = goal;
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

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

pub fn handle_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
    worker: &mpsc::Sender<Cmd>,
) {
    // input_clear: ctrl+c clears a draft before acting as app_exit.
    if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
        if app.dialog == Dialog::None && !app.input.is_empty() {
            app.input.clear();
            app.cursor = 0;
            app.autocomplete = None;
            return;
        }
        app.should_quit = true;
        return;
    }
    // command_list: ctrl+p opens the command palette from anywhere unless
    // the autocomplete popover is capturing motion (ctrl+p moves selection).
    if code == KeyCode::Char('p')
        && modifiers.contains(KeyModifiers::CONTROL)
        && app.autocomplete.is_none()
    {
        app.open_dialog(Dialog::Commands);
        return;
    }
    // Leader sequence (ctrl+x, then one key).
    if code == KeyCode::Char('x') && modifiers.contains(KeyModifiers::CONTROL) {
        app.leader = Some(Instant::now());
        return;
    }
    if app.leader.is_some() {
        app.leader = None;
        match code {
            KeyCode::Char('q') => app.should_quit = true,
            KeyCode::Char('n') => app.go_home(),
            KeyCode::Char('l') => app.open_dialog(Dialog::Sessions),
            KeyCode::Char('a') => app.open_dialog(Dialog::Agents),
            KeyCode::Char('m') => app.open_dialog(Dialog::Models),
            KeyCode::Char('?') => app.open_dialog(Dialog::Help),
            // Fork goal dialogs.
            KeyCode::Char('g') => {
                if app.display_goal().is_some() {
                    app.open_dialog(Dialog::GoalDetails);
                } else {
                    app.toast = Some("No session goal is currently set.".into());
                }
            }
            KeyCode::Char('s') => {
                if app.display_goal().is_some() {
                    app.open_dialog(Dialog::GoalSummaries);
                } else {
                    app.toast = Some("No session goal is currently set.".into());
                }
            }
            _ => app.toast = Some("Unbound leader key".into()),
        }
        return;
    }

    match app.dialog {
        Dialog::None => handle_chat_key(app, code, modifiers, worker),
        Dialog::Help | Dialog::GoalDetails => {
            if matches!(code, KeyCode::Esc | KeyCode::Enter) {
                app.dialog = Dialog::None;
            }
        }
        Dialog::GoalSummaries => match code {
            KeyCode::Esc | KeyCode::Enter => {
                app.dialog = Dialog::None;
                app.scroll = 0;
            }
            KeyCode::Up | KeyCode::PageUp => app.scroll = app.scroll.saturating_add(3),
            KeyCode::Down | KeyCode::PageDown => app.scroll = app.scroll.saturating_sub(3),
            _ => {}
        },
        _ => handle_dialog_key(app, code, modifiers, worker),
    }
}

pub fn handle_chat_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
    worker: &mpsc::Sender<Cmd>,
) {
    if app.autocomplete.is_some() && handle_autocomplete_key(app, code, modifiers, worker) {
        return;
    }
    // agent_cycle: tab / shift+tab rotate build -> plan -> goal. On Home
    // the cycle is local-only; on Session we push the change to the server.
    if (code == KeyCode::Tab || code == KeyCode::BackTab) && app.autocomplete.is_none() {
        let agent = app.cycle_agent(code == KeyCode::BackTab);
        if let (Route::Session, Some(session_id)) = (app.route, app.session_id.clone()) {
            let _ = worker.send(Cmd::SwitchAgent(session_id, agent));
        }
        return;
    }
    // input_newline: shift+enter / ctrl+j / alt+enter.
    if code == KeyCode::Char('j') && modifiers.contains(KeyModifiers::CONTROL) {
        app.insert('\n');
        return;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        match code {
            KeyCode::Char('a') => {
                let (start, _) = line_bounds_chars(&app.input, app.cursor);
                app.cursor = start;
                return;
            }
            KeyCode::Char('e') => {
                let (_, end) = line_bounds_chars(&app.input, app.cursor);
                app.cursor = end;
                return;
            }
            KeyCode::Char('b') => {
                app.move_cursor(-1);
                return;
            }
            KeyCode::Char('f') => {
                app.move_cursor(1);
                return;
            }
            KeyCode::Char('u') => {
                app.delete_to_line_start();
                return;
            }
            KeyCode::Char('k') => {
                app.delete_to_line_end();
                return;
            }
            KeyCode::Char('w') => {
                app.delete_word_back();
                return;
            }
            KeyCode::Char('d') => {
                if app.input.is_empty() {
                    app.should_quit = true;
                } else {
                    app.delete_forward();
                }
                return;
            }
            _ => {}
        }
    }
    if modifiers.contains(KeyModifiers::ALT) {
        match code {
            KeyCode::Char('b') => {
                app.cursor = app.word_start(app.cursor);
                return;
            }
            KeyCode::Char('f') => {
                app.cursor = app.word_end(app.cursor);
                return;
            }
            _ => {}
        }
    }
    match code {
        KeyCode::Enter
            if modifiers.contains(KeyModifiers::ALT) || modifiers.contains(KeyModifiers::SHIFT) =>
        {
            app.insert('\n')
        }
        KeyCode::Enter => submit_prompt(app, worker),
        KeyCode::Esc => {
            if app.busy {
                if let Some(session_id) = app.session_id.clone() {
                    app.interrupts += 1;
                    let _ = worker.send(Cmd::Interrupt(session_id));
                }
            } else {
                app.input.clear();
                app.cursor = 0;
                app.autocomplete = None;
                app.history_cursor = None;
                app.history_draft = None;
            }
        }
        KeyCode::Backspace => app.backspace(),
        KeyCode::Delete => app.delete_forward(),
        KeyCode::Left => app.move_cursor(-1),
        KeyCode::Right => app.move_cursor(1),
        // messages_first / messages_last when the editor is empty; otherwise
        // the usual editor cursor motion.
        KeyCode::Home => {
            if app.input.is_empty() {
                app.scroll = u16::MAX;
            } else {
                app.cursor = 0;
            }
        }
        KeyCode::End => {
            if app.input.is_empty() {
                app.scroll = 0;
            } else {
                app.cursor = app.input.chars().count();
            }
        }
        KeyCode::PageUp => app.scroll = app.scroll.saturating_add(10),
        KeyCode::PageDown => app.scroll = app.scroll.saturating_sub(10),
        KeyCode::Up => {
            if !app.input.contains('\n') && app.history_prev() {
                return;
            }
            app.scroll = app.scroll.saturating_add(3);
        }
        KeyCode::Down => {
            if app.history_cursor.is_some() && app.history_next() {
                return;
            }
            app.scroll = app.scroll.saturating_sub(3);
        }
        KeyCode::Char(ch) => {
            app.toast = None;
            app.insert(ch);
        }
        _ => {}
    }
}

fn line_bounds_chars(input: &str, cursor: usize) -> (usize, usize) {
    let chars: Vec<char> = input.chars().collect();
    let mut start = cursor.min(chars.len());
    while start > 0 && chars[start - 1] != '\n' {
        start -= 1;
    }
    let mut end = cursor.min(chars.len());
    while end < chars.len() && chars[end] != '\n' {
        end += 1;
    }
    (start, end)
}

fn autocomplete_move(app: &mut App, delta: isize) {
    let Some(auto) = app.autocomplete.as_mut() else {
        return;
    };
    if auto.matches.is_empty() {
        return;
    }
    let count = auto.matches.len() as isize;
    auto.index = ((auto.index as isize + delta).rem_euclid(count)) as usize;
}

/// Returns `true` when the key was consumed by the autocomplete popover.
fn handle_autocomplete_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
    worker: &mpsc::Sender<Cmd>,
) -> bool {
    match code {
        KeyCode::Up => {
            autocomplete_move(app, -1);
            true
        }
        KeyCode::Down => {
            autocomplete_move(app, 1);
            true
        }
        KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => {
            autocomplete_move(app, -1);
            true
        }
        KeyCode::Char('n') if modifiers.contains(KeyModifiers::CONTROL) => {
            autocomplete_move(app, 1);
            true
        }
        KeyCode::Tab => {
            app.complete_autocomplete(true);
            true
        }
        KeyCode::Esc => {
            app.autocomplete = None;
            true
        }
        KeyCode::Enter
            if !modifiers.contains(KeyModifiers::ALT)
                && !modifiers.contains(KeyModifiers::SHIFT) =>
        {
            commit_autocomplete(app, worker);
            true
        }
        _ => false,
    }
}

fn commit_autocomplete(app: &mut App, worker: &mpsc::Sender<Cmd>) {
    // Prefer an exact command match if the user already typed one, so
    // Enter after `/help` executes rather than re-completes.
    let filter = app
        .autocomplete
        .as_ref()
        .map(|auto| auto.filter.clone())
        .unwrap_or_default();
    let exact = COMMANDS.iter().position(|spec| spec.name == filter);
    if let Some(index) = exact {
        if let Some(auto) = app.autocomplete.as_mut() {
            auto.index = auto
                .matches
                .iter()
                .position(|value| *value == index)
                .unwrap_or(auto.index);
        }
        submit_prompt(app, worker);
        return;
    }
    let spec = app.complete_autocomplete(false);
    if spec.is_some_and(|command| !command.takes_args) {
        submit_prompt(app, worker);
    }
}

pub fn submit_prompt(app: &mut App, worker: &mpsc::Sender<Cmd>) {
    let text = app.input.trim().to_string();
    if text.is_empty() {
        return;
    }
    if let Some(command) = text.strip_prefix('/') {
        let (name, args) = command.split_once(' ').unwrap_or((command, ""));
        app.input.clear();
        app.cursor = 0;
        app.autocomplete = None;
        match name {
            "new" => app.go_home(),
            "sessions" => app.open_dialog(Dialog::Sessions),
            "agents" => app.open_dialog(Dialog::Agents),
            "models" => app.open_dialog(Dialog::Models),
            "commands" => app.open_dialog(Dialog::Commands),
            "help" => app.open_dialog(Dialog::Help),
            "exit" | "quit" => app.should_quit = true,
            // Fork /goal command: template "$ARGUMENTS" steers the goal
            // agent; set/edit/resume auto-switch to it first.
            "goal" => run_goal_command(app, args, worker),
            "goal-details" => {
                if app.display_goal().is_some() {
                    app.open_dialog(Dialog::GoalDetails);
                } else {
                    app.toast = Some("No session goal is currently set.".into());
                }
            }
            "goal-summaries" => {
                if app.display_goal().is_some() {
                    app.open_dialog(Dialog::GoalSummaries);
                } else {
                    app.toast = Some("No session goal is currently set.".into());
                }
            }
            "external-access" => toggle_external_access(app, worker),
            other => app.toast = Some(format!("Unknown command: /{other}")),
        }
        return;
    }
    app.push_history(text.clone());
    app.input.clear();
    app.cursor = 0;
    app.scroll = 0;
    app.toast = None;
    app.history_cursor = None;
    app.history_draft = None;
    match (app.route, app.session_id.clone()) {
        (Route::Session, Some(session_id)) => {
            app.busy = true;
            let _ = worker.send(Cmd::Prompt(session_id, text));
        }
        _ => {
            app.busy = true;
            app.toast = Some("Creating session…".into());
            let _ = worker.send(Cmd::CreateAndPrompt {
                directory: app.directory.clone(),
                agent: app.agent.clone(),
                provider: app.provider.clone(),
                model: app.model.clone(),
                text,
            });
        }
    }
}

fn run_goal_command(app: &mut App, args: &str, worker: &mpsc::Sender<Cmd>) {
    let action = args
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    let Some(session_id) = app.session_id.clone() else {
        app.toast = Some("Open a session before using /goal".into());
        return;
    };
    if matches!(action.as_str(), "set" | "edit" | "resume") && app.agent != "goal" {
        app.agent = "goal".into();
        let _ = worker.send(Cmd::SwitchAgent(session_id.clone(), "goal".into()));
    }
    if args.is_empty() {
        app.toast = Some("Usage: /goal set|edit|pause|resume|complete|status|clear …".into());
        return;
    }
    app.busy = true;
    let _ = worker.send(Cmd::Prompt(
        session_id,
        format!("Manage the session goal: {args}"),
    ));
}

fn toggle_external_access(app: &mut App, worker: &mpsc::Sender<Cmd>) {
    if let Some(session_id) = app.session_id.clone() {
        app.external_allowed = !app.external_allowed;
        let _ = worker.send(Cmd::ToggleExternal(session_id, app.external_allowed));
        return;
    }
    app.toast = Some("Open or start a session before changing out-of-workspace permissions".into());
}

pub fn handle_dialog_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
    worker: &mpsc::Sender<Cmd>,
) {
    let options = ui::dialog_options(app);
    let move_selection = |app: &mut App, delta: isize| {
        let count = ui::dialog_options(app).len() as isize;
        if count == 0 {
            return;
        }
        let next = (app.list_index as isize + delta).rem_euclid(count);
        app.list_index = next as usize;
    };
    match code {
        KeyCode::Esc => app.dialog = Dialog::None,
        KeyCode::Up => move_selection(app, -1),
        KeyCode::Down => move_selection(app, 1),
        KeyCode::PageUp => move_selection(app, -5),
        KeyCode::PageDown => move_selection(app, 5),
        KeyCode::Home => app.list_index = 0,
        KeyCode::End => app.list_index = options.len().saturating_sub(1),
        KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => move_selection(app, -1),
        KeyCode::Char('n') if modifiers.contains(KeyModifiers::CONTROL) => move_selection(app, 1),
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
                Dialog::Commands => commit_command(app, options.get(selected), worker),
                Dialog::Sessions => commit_session(app, selected, worker),
                Dialog::Agents => commit_agent(app, selected, worker),
                Dialog::Models => commit_model(app, selected, worker),
                _ => {}
            }
        }
        _ => {}
    }
}

fn commit_command(app: &mut App, option: Option<&ui::DialogOption>, worker: &mpsc::Sender<Cmd>) {
    let Some(option) = option else {
        return;
    };
    match option.title.as_str() {
        "New session" => app.go_home(),
        "Switch session" => app.open_dialog(Dialog::Sessions),
        "Switch agent" => app.open_dialog(Dialog::Agents),
        "Switch model" => app.open_dialog(Dialog::Models),
        "Commands" => app.open_dialog(Dialog::Commands),
        "Help" => app.open_dialog(Dialog::Help),
        "Goal details" => {
            if app.display_goal().is_some() {
                app.open_dialog(Dialog::GoalDetails);
            } else {
                app.toast = Some("No session goal is currently set.".into());
            }
        }
        "Goal summaries" => {
            if app.display_goal().is_some() {
                app.open_dialog(Dialog::GoalSummaries);
            } else {
                app.toast = Some("No session goal is currently set.".into());
            }
        }
        "Manage goal" => {
            app.input = "/goal ".into();
            app.cursor = app.input.chars().count();
            app.sync_autocomplete();
        }
        "Toggle out-of-workspace access" => {
            toggle_external_access(app, worker);
        }
        _ => app.should_quit = true,
    }
}

fn commit_session(app: &mut App, selected: usize, worker: &mpsc::Sender<Cmd>) {
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
        if let Some(session_id) = app.session_id.clone() {
            let _ = worker.send(Cmd::SelectSession(session_id));
        }
    }
}

fn commit_agent(app: &mut App, selected: usize, worker: &mpsc::Sender<Cmd>) {
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
        if let (Route::Session, Some(session_id)) = (app.route, app.session_id.clone()) {
            let _ = worker.send(Cmd::SwitchAgent(session_id, name));
        }
    }
}

fn commit_model(app: &mut App, selected: usize, worker: &mpsc::Sender<Cmd>) {
    let filtered: Vec<&(String, String)> = app
        .models
        .iter()
        .filter(|(provider, model)| state::fuzzy(&format!("{provider}/{model}"), &app.search))
        .collect();
    if let Some((provider, model)) = filtered.get(selected) {
        app.provider = provider.clone();
        app.model = model.clone();
        if let (Route::Session, Some(session_id)) = (app.route, app.session_id.clone()) {
            let _ = worker.send(Cmd::SwitchModel(
                session_id,
                provider.clone(),
                model.clone(),
            ));
        }
    }
}

/// Worker thread: owns every blocking HTTP call. Polls messages, todos, and
/// active state (fast while a drain runs) and executes commands.
fn worker(
    base: String,
    authorization: Option<String>,
    _directory: String,
    session_flag: Option<String>,
    to_ui: mpsc::Sender<Msg>,
    from_ui: mpsc::Receiver<Cmd>,
) {
    let api = api::Api {
        base,
        authorization,
    };
    let mut session_id: Option<String> = None;
    let mut busy = false;

    // `--session <id>` explicitly adopts. Default launch stays on Home and
    // does not adopt or create anything until the user does.
    if let Some(id) = session_flag {
        if let Ok(session) = api.session(&id) {
            if !session.is_null() {
                session_id = session
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let _ = to_ui.send(Msg::Session(session));
            }
        }
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
            Ok(Cmd::CreateAndPrompt {
                directory,
                agent,
                provider,
                model,
                text,
            }) => match api.create_session_with(&directory, &agent, &provider, &model) {
                Ok(session) => {
                    let created: Option<String> = session
                        .get("id")
                        .and_then(Value::as_str)
                        .map(|value| value.to_string());
                    session_id.clone_from(&created);
                    let _ = to_ui.send(Msg::Session(session));
                    if let Some(created_id) = created {
                        busy = true;
                        if let Err(error) = api.prompt(&created_id, &text) {
                            let _ = to_ui.send(Msg::Toast(format!("prompt failed: {error}")));
                        }
                    }
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
            Ok(Cmd::SelectSession(id)) => {
                session_id = Some(id);
                busy = false;
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
            Ok(Cmd::ToggleExternal(id, allow)) => {
                let message = match api.set_external_permission(&id, allow) {
                    Ok(()) if allow => "Always allowing out-of-workspace access for this session",
                    Ok(()) => "Out-of-workspace access will ask for permission",
                    Err(_) => "Failed to update out-of-workspace permission",
                };
                let _ = to_ui.send(Msg::Toast(message.to_string()));
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
            if let Ok(goal) = api.goal(id) {
                let _ = to_ui.send(Msg::Goal(id.clone(), goal));
            }
        }
    }
}
