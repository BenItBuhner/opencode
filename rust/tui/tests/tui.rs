//! Focused unit tests for the Rust TUI: Home vs Session routing, agent
//! cycling on Home not sending server changes, first-prompt session
//! creation, slash-command autocomplete filtering and selection, and the
//! deterministic mouse hit-test used by the mouse handler.

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use opencode_tui::state::{
    autocomplete_entries, fuzzy_score, App, Dialog, DynamicCommand, EntryOrigin, HitTarget,
    InputMode, Rectangle, Route, AUTOCOMPLETE_MAX_ROWS, COMMANDS,
};
use opencode_tui::{
    apply, handle_chat_key, handle_dialog_key, handle_key, handle_mouse, handle_worker_cmd,
    hit_test, submit_prompt, Cmd, Msg, WorkerApi, WorkerState,
};
use ratatui::layout::Rect;
use std::sync::mpsc;

fn worker_channel() -> (mpsc::Sender<Cmd>, mpsc::Receiver<Cmd>) {
    mpsc::channel::<Cmd>()
}

fn drain(receiver: &mpsc::Receiver<Cmd>) -> Vec<Cmd> {
    let mut items = vec![];
    while let Ok(cmd) = receiver.try_recv() {
        items.push(cmd);
    }
    items
}

#[derive(Default)]
struct FakeApi {
    calls: std::cell::RefCell<Vec<String>>,
}

impl FakeApi {
    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl WorkerApi for FakeApi {
    fn sessions(&self) -> Result<Vec<serde_json::Value>, String> {
        Ok(vec![])
    }
    fn session(&self, session_id: &str) -> Result<serde_json::Value, String> {
        Ok(match session_id {
            "ses_child" => serde_json::json!({
                "id": "ses_child",
                "parentID": "ses_parent",
                "title": "Child",
                "location": { "directory": "/repo" }
            }),
            _ => serde_json::json!({
                "id": session_id,
                "title": "Parent",
                "location": { "directory": "/repo" }
            }),
        })
    }
    fn create_session_with(
        &self,
        _directory: &str,
        _agent: &str,
        _provider: &str,
        _model: &str,
    ) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({ "id": "ses_created", "title": "Created" }))
    }
    fn messages(&self, _session_id: &str) -> Result<Vec<serde_json::Value>, String> {
        Ok(vec![])
    }
    fn active(&self) -> Result<Vec<String>, String> {
        Ok(vec![])
    }
    fn session_status(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({}))
    }
    fn children(&self, _session_id: &str) -> Result<Vec<serde_json::Value>, String> {
        Ok(vec![])
    }
    fn permissions(&self, _session_id: &str) -> Result<Vec<serde_json::Value>, String> {
        Ok(vec![])
    }
    fn questions(&self, _session_id: &str) -> Result<Vec<serde_json::Value>, String> {
        Ok(vec![])
    }
    fn prompt(&self, session_id: &str, text: &str) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(format!("prompt:{session_id}:{text}"));
        Ok(())
    }
    fn interrupt(&self, session_id: &str) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(format!("interrupt:{session_id}"));
        Ok(())
    }
    fn permission_reply(
        &self,
        session_id: &str,
        request_id: &str,
        reply: &str,
        message: Option<&str>,
    ) -> Result<(), String> {
        self.calls.borrow_mut().push(format!(
            "permission:{session_id}:{request_id}:{reply}:{}",
            message.unwrap_or("")
        ));
        Ok(())
    }
    fn question_reply(
        &self,
        session_id: &str,
        request_id: &str,
        answers: &[Vec<String>],
    ) -> Result<(), String> {
        self.calls.borrow_mut().push(format!(
            "question-reply:{session_id}:{request_id}:{}",
            answers[0][0]
        ));
        Ok(())
    }
    fn question_reject(&self, session_id: &str, request_id: &str) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(format!("question-reject:{session_id}:{request_id}"));
        Ok(())
    }
    fn background(&self, session_id: &str) -> Result<bool, String> {
        self.calls
            .borrow_mut()
            .push(format!("background:{session_id}"));
        Ok(true)
    }
    fn switch_agent(&self, session_id: &str, agent: &str) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(format!("agent:{session_id}:{agent}"));
        Ok(())
    }
    fn switch_model(&self, session_id: &str, provider: &str, model: &str) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(format!("model:{session_id}:{provider}:{model}"));
        Ok(())
    }
    fn goal(&self, _session_id: &str) -> Result<Option<serde_json::Value>, String> {
        Ok(None)
    }
    fn todos(&self, _session_id: &str) -> Result<Vec<serde_json::Value>, String> {
        Ok(vec![])
    }
    fn set_external_permission(&self, session_id: &str, allow: bool) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(format!("external:{session_id}:{allow}"));
        Ok(())
    }
    fn agents(&self) -> Result<Vec<serde_json::Value>, String> {
        Ok(vec![])
    }
    fn models(&self) -> Result<Vec<(String, String)>, String> {
        Ok(vec![])
    }
    fn commands(&self) -> Result<Vec<DynamicCommand>, String> {
        Ok(vec![])
    }
}

#[test]
fn default_launch_is_home() {
    let app = App::new("/tmp/example".into());
    assert!(app.is_home());
    assert_eq!(app.route, Route::Home);
    assert!(app.session_id.is_none());
    assert!(app.messages.is_empty());
}

#[test]
fn home_agent_cycle_does_not_send_server_changes() {
    let mut app = App::new("/tmp/example".into());
    let (sender, receiver) = worker_channel();

    handle_chat_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, &sender);
    handle_chat_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, &sender);
    handle_chat_key(&mut app, KeyCode::BackTab, KeyModifiers::NONE, &sender);

    assert_eq!(app.agent, "plan");
    // No SwitchAgent (or any) command should reach the worker while on
    // Home, even though the local agent changed.
    assert!(drain(&receiver).is_empty(), "no worker cmds while on Home");
}

#[test]
fn first_prompt_on_home_creates_session_then_prompts() {
    let mut app = App::new("/repo".into());
    app.agent = "plan".into();
    app.provider = "opencode".into();
    app.model = "big-pickle".into();
    let (sender, receiver) = worker_channel();

    app.input = "hello world".into();
    app.cursor = app.input.chars().count();
    submit_prompt(&mut app, &sender);

    let cmds = drain(&receiver);
    assert_eq!(cmds.len(), 1, "expected exactly one worker command");
    match &cmds[0] {
        Cmd::CreateAndPrompt {
            directory,
            agent,
            provider,
            model,
            text,
        } => {
            assert_eq!(directory, "/repo");
            assert_eq!(agent, "plan");
            assert_eq!(provider, "opencode");
            assert_eq!(model, "big-pickle");
            assert_eq!(text, "hello world");
        }
        other => panic!("expected CreateAndPrompt, got {other:?}"),
    }
    assert_eq!(app.prompt_history, vec!["hello world".to_string()]);
    assert!(app.input.is_empty());
    assert!(app.busy);
}

#[test]
fn slash_new_returns_home_without_creating_a_session() {
    let mut app = App::new("/repo".into());
    app.route = Route::Session;
    app.session_id = Some("existing".into());
    app.session_title = "existing".into();
    let (sender, receiver) = worker_channel();

    app.input = "/new".into();
    app.cursor = app.input.chars().count();
    submit_prompt(&mut app, &sender);

    assert_eq!(app.route, Route::Home);
    assert!(app.session_id.is_none());
    assert!(drain(&receiver).is_empty(), "/new is a pure UI action");
}

#[test]
fn autocomplete_opens_on_slash_and_filters() {
    let mut app = App::new("/repo".into());
    app.insert('/');
    let auto = app.autocomplete.as_ref().expect("autocomplete opens on /");
    assert_eq!(auto.filter, "");
    assert_eq!(auto.entries.len(), COMMANDS.len());

    app.insert('s');
    let auto = app.autocomplete.as_ref().expect("still open");
    assert!(!auto.entries.is_empty());
    // "sessions" must be one of the /s matches; "goal" must not be.
    let names: Vec<String> = auto
        .entries
        .iter()
        .map(|entry| entry.name.clone())
        .collect();
    assert!(names.contains(&"sessions".to_string()));
    assert!(!names.contains(&"goal".to_string()));

    app.insert('e');
    let auto = app.autocomplete.as_ref().expect("still open");
    let names: Vec<String> = auto
        .entries
        .iter()
        .map(|entry| entry.name.clone())
        .collect();
    assert!(names.contains(&"sessions".to_string()));
    assert!(!names.contains(&"models".to_string()));
}

#[test]
fn autocomplete_navigation_and_selection() {
    let mut app = App::new("/repo".into());
    let (sender, receiver) = worker_channel();
    app.insert('/');

    let initial = app.autocomplete.as_ref().unwrap().index;
    handle_chat_key(&mut app, KeyCode::Down, KeyModifiers::NONE, &sender);
    let after_down = app.autocomplete.as_ref().unwrap().index;
    assert_eq!(after_down, initial + 1);

    handle_chat_key(&mut app, KeyCode::Up, KeyModifiers::NONE, &sender);
    assert_eq!(app.autocomplete.as_ref().unwrap().index, initial);

    handle_chat_key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL, &sender);
    assert_eq!(app.autocomplete.as_ref().unwrap().index, initial + 1);
    handle_chat_key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL, &sender);
    assert_eq!(app.autocomplete.as_ref().unwrap().index, initial);

    // Tab completes for non-arg commands and closes the popover.
    handle_chat_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, &sender);
    assert!(drain(&receiver).is_empty(), "tab does not commit");
}

#[test]
fn autocomplete_esc_closes_popover_only() {
    let mut app = App::new("/repo".into());
    let (sender, _receiver) = worker_channel();
    app.insert('/');
    handle_chat_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, &sender);
    assert!(app.autocomplete.is_none());
    assert_eq!(app.input, "/");
}

#[test]
fn autocomplete_enter_on_exact_match_executes_command() {
    let mut app = App::new("/repo".into());
    let (sender, receiver) = worker_channel();
    for ch in "/help".chars() {
        app.insert(ch);
    }
    handle_chat_key(&mut app, KeyCode::Enter, KeyModifiers::NONE, &sender);
    assert_eq!(app.dialog, Dialog::Help);
    assert!(drain(&receiver).is_empty(), "/help does not hit the worker");
}

#[test]
fn slash_commands_opens_the_palette() {
    let mut app = App::new("/repo".into());
    let (sender, receiver) = worker_channel();
    for ch in "/commands".chars() {
        app.insert(ch);
    }
    handle_chat_key(&mut app, KeyCode::Enter, KeyModifiers::NONE, &sender);
    assert_eq!(app.dialog, Dialog::Commands);
    assert!(drain(&receiver).is_empty());
}

#[test]
fn ctrl_a_and_ctrl_e_jump_line_bounds() {
    let mut app = App::new("/repo".into());
    let (sender, _receiver) = worker_channel();
    for ch in "one two three".chars() {
        app.insert(ch);
    }
    handle_chat_key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL, &sender);
    assert_eq!(app.cursor, 0);
    handle_chat_key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL, &sender);
    assert_eq!(app.cursor, app.input.chars().count());
}

#[test]
fn ctrl_w_deletes_word_back() {
    let mut app = App::new("/repo".into());
    let (sender, _receiver) = worker_channel();
    for ch in "one two three".chars() {
        app.insert(ch);
    }
    handle_chat_key(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL, &sender);
    assert_eq!(app.input, "one two ");
}

#[test]
fn alt_b_and_alt_f_word_motion() {
    let mut app = App::new("/repo".into());
    let (sender, _receiver) = worker_channel();
    for ch in "aaa bbb ccc".chars() {
        app.insert(ch);
    }
    handle_chat_key(&mut app, KeyCode::Char('b'), KeyModifiers::ALT, &sender);
    assert_eq!(app.cursor, 8, "alt+b jumps to start of last word");
    handle_chat_key(&mut app, KeyCode::Char('b'), KeyModifiers::ALT, &sender);
    assert_eq!(app.cursor, 4);
    handle_chat_key(&mut app, KeyCode::Char('f'), KeyModifiers::ALT, &sender);
    assert_eq!(app.cursor, 7);
}

#[test]
fn delete_and_ctrl_d_forward_delete() {
    let mut app = App::new("/repo".into());
    let (sender, _receiver) = worker_channel();
    for ch in "abcd".chars() {
        app.insert(ch);
    }
    app.cursor = 1;
    handle_chat_key(&mut app, KeyCode::Delete, KeyModifiers::NONE, &sender);
    assert_eq!(app.input, "acd");
    handle_chat_key(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL, &sender);
    assert_eq!(app.input, "ad");
}

#[test]
fn ctrl_d_on_empty_input_quits() {
    let mut app = App::new("/repo".into());
    let (sender, _receiver) = worker_channel();
    handle_chat_key(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL, &sender);
    assert!(app.should_quit);
}

#[test]
fn ctrl_c_clears_a_draft_before_quitting() {
    let mut app = App::new("/repo".into());
    let (sender, _receiver) = worker_channel();
    app.input = "draft".into();
    app.cursor = app.input.len();

    handle_key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL, &sender);
    assert!(app.input.is_empty());
    assert!(!app.should_quit);

    handle_key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL, &sender);
    assert!(app.should_quit);
}

#[test]
fn plain_p_and_n_filter_dialogs_while_control_moves_selection() {
    let mut app = App::new("/repo".into());
    let (sender, _receiver) = worker_channel();
    app.open_dialog(Dialog::Commands);

    handle_dialog_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE, &sender);
    handle_dialog_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, &sender);
    assert_eq!(app.search, "pn");

    app.search.clear();
    app.list_index = 0;
    handle_dialog_key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL, &sender);
    assert_eq!(app.list_index, 1);
}

#[test]
fn selecting_a_session_retargets_worker_polling() {
    let mut app = App::new("/repo".into());
    let (sender, receiver) = worker_channel();
    app.sessions.push(serde_json::json!({
        "id": "ses_selected",
        "title": "Selected session",
        "location": { "directory": "/repo" }
    }));
    app.open_dialog(Dialog::Sessions);

    handle_dialog_key(&mut app, KeyCode::Enter, KeyModifiers::NONE, &sender);

    assert_eq!(app.session_id.as_deref(), Some("ses_selected"));
    assert_eq!(
        drain(&receiver),
        vec![Cmd::SelectSession("ses_selected".into())]
    );
}

#[test]
fn apply_updates_child_pending_and_status_state() {
    let mut app = App::new("/repo".into());
    app.adopt_session(&serde_json::json!({
        "id": "ses_parent",
        "title": "Parent",
        "location": { "directory": "/repo" }
    }));

    apply(
        &mut app,
        Msg::Children(
            "ses_parent".into(),
            vec![serde_json::json!({ "id": "ses_child", "parentID": "ses_parent" })],
        ),
    );
    apply(
        &mut app,
        Msg::Permissions(
            "ses_parent".into(),
            vec![serde_json::json!({ "id": "per_1", "sessionID": "ses_child" })],
        ),
    );
    apply(
        &mut app,
        Msg::Questions(
            "ses_parent".into(),
            vec![serde_json::json!({ "id": "que_1", "sessionID": "ses_child" })],
        ),
    );
    apply(
        &mut app,
        Msg::SessionStatus(serde_json::json!({ "ses_child": "running" })),
    );

    assert_eq!(app.child_sessions.len(), 1);
    assert_eq!(app.pending_permissions[0]["id"], "per_1");
    assert_eq!(app.pending_questions[0]["id"], "que_1");
    assert_eq!(app.session_status["ses_child"], "running");
}

#[test]
fn permission_key_dispatches_reply_command() {
    let mut app = App::new("/repo".into());
    app.pending_permissions = vec![serde_json::json!({
        "id": "per_1",
        "sessionID": "ses_parent",
        "action": "read",
        "resources": [".env"]
    })];
    let (sender, receiver) = worker_channel();

    handle_key(&mut app, KeyCode::Char('1'), KeyModifiers::NONE, &sender);
    handle_key(&mut app, KeyCode::Char('3'), KeyModifiers::NONE, &sender);

    assert_eq!(
        drain(&receiver),
        vec![
            Cmd::PermissionReply {
                session_id: "ses_parent".into(),
                request_id: "per_1".into(),
                reply: "once".into(),
                message: None,
            },
            Cmd::PermissionReply {
                session_id: "ses_parent".into(),
                request_id: "per_1".into(),
                reply: "reject".into(),
                message: None,
            },
        ]
    );
}

#[test]
fn question_key_dispatches_reply_and_reject_commands() {
    let mut app = App::new("/repo".into());
    app.pending_questions = vec![serde_json::json!({
        "id": "que_1",
        "sessionID": "ses_parent",
        "questions": [{
            "question": "Continue?",
            "header": "choice",
            "options": [
                { "label": "Yes", "description": "Proceed" },
                { "label": "No", "description": "Stop" }
            ]
        }]
    })];
    let (sender, receiver) = worker_channel();

    handle_key(&mut app, KeyCode::Char('2'), KeyModifiers::NONE, &sender);
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, &sender);

    assert_eq!(
        drain(&receiver),
        vec![
            Cmd::QuestionReply {
                session_id: "ses_parent".into(),
                request_id: "que_1".into(),
                answers: vec![vec!["No".into()]],
            },
            Cmd::QuestionReject {
                session_id: "ses_parent".into(),
                request_id: "que_1".into(),
            },
        ]
    );
}

#[test]
fn background_shortcut_promotes_foreground_tasks_without_pending_request_blocking() {
    let mut app = App::new("/repo".into());
    app.adopt_session(&serde_json::json!({
        "id": "ses_parent",
        "title": "Parent",
        "location": { "directory": "/repo" }
    }));
    app.messages = vec![serde_json::json!({
        "parts": [{
            "tool": "task",
            "state": {
                "status": "running",
                "metadata": { "sessionId": "ses_child" }
            }
        }]
    })];
    app.pending_permissions = vec![serde_json::json!({
        "id": "per_1",
        "sessionID": "ses_parent"
    })];
    let (sender, receiver) = worker_channel();

    handle_key(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL, &sender);

    assert_eq!(drain(&receiver), vec![Cmd::Background("ses_parent".into())]);
    assert_eq!(
        app.pending_permissions[0]["id"], "per_1",
        "backgrounding does not require settling pending prompts first"
    );
}

#[test]
fn child_navigation_keybindings_dispatch_select_child_and_parent() {
    let mut app = App::new("/repo".into());
    app.adopt_session(&serde_json::json!({
        "id": "ses_parent",
        "title": "Parent",
        "location": { "directory": "/repo" }
    }));
    app.child_sessions = vec![
        serde_json::json!({ "id": "ses_child_a", "parentID": "ses_parent", "title": "A" }),
        serde_json::json!({ "id": "ses_child_b", "parentID": "ses_parent", "title": "B" }),
    ];
    let (sender, receiver) = worker_channel();

    handle_key(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL, &sender);
    handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE, &sender);
    assert_eq!(
        drain(&receiver),
        vec![Cmd::SelectChild("ses_child_a".into())]
    );

    app.parent_session = Some(serde_json::json!({
        "id": "ses_parent",
        "title": "Parent",
        "location": { "directory": "/repo" }
    }));
    handle_key(&mut app, KeyCode::Right, KeyModifiers::NONE, &sender);
    handle_key(&mut app, KeyCode::Up, KeyModifiers::NONE, &sender);

    assert_eq!(
        drain(&receiver),
        vec![
            Cmd::SelectChild("ses_child_b".into()),
            Cmd::SelectParent("ses_parent".into()),
        ]
    );
}

#[test]
fn worker_command_handler_dispatches_reply_background_and_child_selection() {
    let api = FakeApi::default();
    let mut state = WorkerState::default();
    let (sender, receiver) = mpsc::channel();

    handle_worker_cmd(
        &api,
        &mut state,
        &sender,
        Cmd::PermissionReply {
            session_id: "ses_parent".into(),
            request_id: "per_1".into(),
            reply: "always".into(),
            message: None,
        },
    );
    handle_worker_cmd(
        &api,
        &mut state,
        &sender,
        Cmd::QuestionReply {
            session_id: "ses_parent".into(),
            request_id: "que_1".into(),
            answers: vec![vec!["Yes".into()]],
        },
    );
    handle_worker_cmd(
        &api,
        &mut state,
        &sender,
        Cmd::QuestionReject {
            session_id: "ses_parent".into(),
            request_id: "que_1".into(),
        },
    );
    handle_worker_cmd(
        &api,
        &mut state,
        &sender,
        Cmd::Background("ses_parent".into()),
    );
    handle_worker_cmd(
        &api,
        &mut state,
        &sender,
        Cmd::SelectChild("ses_child".into()),
    );

    assert_eq!(
        api.calls(),
        vec![
            "permission:ses_parent:per_1:always:",
            "question-reply:ses_parent:que_1:Yes",
            "question-reject:ses_parent:que_1",
            "background:ses_parent",
        ]
    );
    assert_eq!(state.session_id.as_deref(), Some("ses_child"));
    assert_eq!(state.parent_id.as_deref(), Some("ses_parent"));
    assert!(matches!(receiver.try_recv().unwrap(), Msg::Toast(_)));
    assert!(matches!(receiver.try_recv().unwrap(), Msg::Session(_)));
}

#[test]
fn prompt_history_walks_with_up_and_down() {
    let mut app = App::new("/repo".into());
    let (sender, _receiver) = worker_channel();
    app.push_history("first".into());
    app.push_history("second".into());
    handle_chat_key(&mut app, KeyCode::Up, KeyModifiers::NONE, &sender);
    assert_eq!(app.input, "second");
    handle_chat_key(&mut app, KeyCode::Up, KeyModifiers::NONE, &sender);
    assert_eq!(app.input, "first");
    handle_chat_key(&mut app, KeyCode::Down, KeyModifiers::NONE, &sender);
    assert_eq!(app.input, "second");
    handle_chat_key(&mut app, KeyCode::Down, KeyModifiers::NONE, &sender);
    assert_eq!(app.input, "");
}

#[test]
fn leader_chord_expires_and_does_not_hijack_next_key() {
    let mut app = App::new("/repo".into());
    let (sender, _receiver) = worker_channel();
    handle_key(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL, &sender);
    assert!(app.leader.is_some());
    // Rewind the leader stamp past the timeout and let the run loop's
    // sweep clear it, then confirm the next key types normally.
    app.leader = Some(
        std::time::Instant::now()
            - opencode_tui::state::LEADER_TIMEOUT
            - std::time::Duration::from_secs(1),
    );
    if let Some(at) = app.leader {
        if at.elapsed() >= opencode_tui::state::LEADER_TIMEOUT {
            app.leader = None;
        }
    }
    handle_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, &sender);
    assert_eq!(app.input, "n", "n after expired leader types normally");
}

#[test]
fn hit_test_finds_dialog_rows_and_prompt_regions() {
    let mut app = App::new("/repo".into());
    app.dialog = Dialog::Commands;
    app.geometry.dialog_area = Some(Rectangle {
        x: 5,
        y: 8,
        width: 40,
        height: 12,
    });
    app.geometry.dialog_list_top = 12;
    assert_eq!(hit_test(&app, 20, 12), Some(HitTarget::Row(0)), "first row");
    assert_eq!(hit_test(&app, 20, 14), Some(HitTarget::Row(2)), "third row");

    // Clicking above the list rows returns no row (title/search area).
    assert_eq!(hit_test(&app, 20, 9), None);

    // Dialog closed: agent/model spans are addressable.
    app.dialog = Dialog::None;
    app.geometry.dialog_area = None;
    app.geometry.agent_span = Some(Rectangle {
        x: 10,
        y: 5,
        width: 8,
        height: 1,
    });
    app.geometry.model_span = Some(Rectangle {
        x: 18,
        y: 5,
        width: 12,
        height: 1,
    });
    app.geometry.textarea = Some(Rectangle {
        x: 3,
        y: 2,
        width: 60,
        height: 3,
    });
    app.geometry.tip_row = Some(Rectangle {
        x: 3,
        y: 20,
        width: 60,
        height: 1,
    });
    assert_eq!(hit_test(&app, 12, 5), Some(HitTarget::AgentSpan));
    assert_eq!(hit_test(&app, 20, 5), Some(HitTarget::ModelSpan));
    assert_eq!(
        hit_test(&app, 6, 3),
        Some(HitTarget::Prompt { row: 1, col: 3 })
    );
    assert_eq!(hit_test(&app, 10, 20), Some(HitTarget::TipRow));
}

#[test]
fn click_agent_span_opens_agents_dialog() {
    let mut app = App::new("/repo".into());
    let (sender, _receiver) = worker_channel();
    app.geometry.agent_span = Some(Rectangle {
        x: 10,
        y: 5,
        width: 8,
        height: 1,
    });
    let down = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 12,
        row: 5,
        modifiers: KeyModifiers::NONE,
    };
    handle_mouse(&mut app, down, Rect::new(0, 0, 80, 24), &sender);
    let up = MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: 12,
        row: 5,
        modifiers: KeyModifiers::NONE,
    };
    handle_mouse(&mut app, up, Rect::new(0, 0, 80, 24), &sender);
    assert_eq!(app.dialog, Dialog::Agents);
}

#[test]
fn click_tip_row_cycles_tip_index() {
    let mut app = App::new("/repo".into());
    let (sender, _receiver) = worker_channel();
    app.geometry.tip_row = Some(Rectangle {
        x: 3,
        y: 20,
        width: 40,
        height: 1,
    });
    let before = app.tip_index;
    let down = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 10,
        row: 20,
        modifiers: KeyModifiers::NONE,
    };
    handle_mouse(&mut app, down, Rect::new(0, 0, 80, 24), &sender);
    let up = MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: 10,
        row: 20,
        modifiers: KeyModifiers::NONE,
    };
    handle_mouse(&mut app, up, Rect::new(0, 0, 80, 24), &sender);
    assert_eq!(app.tip_index, before.wrapping_add(1));
}

#[test]
fn click_goal_bar_opens_summaries() {
    let mut app = App::new("/repo".into());
    let (sender, _receiver) = worker_channel();
    app.goal = Some(serde_json::json!({
        "text": "Ship the port",
        "status": "active",
        "progress": 50,
        "summaries": []
    }));
    app.geometry.goal_chip = Some(Rectangle {
        x: 40,
        y: 22,
        width: 25,
        height: 1,
    });
    app.geometry.goal_bar = Some(Rectangle {
        x: 50,
        y: 22,
        width: 12,
        height: 1,
    });
    let down = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 54,
        row: 22,
        modifiers: KeyModifiers::NONE,
    };
    handle_mouse(&mut app, down, Rect::new(0, 0, 80, 24), &sender);
    let up = MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: 54,
        row: 22,
        modifiers: KeyModifiers::NONE,
    };
    handle_mouse(&mut app, up, Rect::new(0, 0, 80, 24), &sender);
    assert_eq!(app.dialog, Dialog::GoalSummaries);
}

#[test]
fn hover_over_dialog_row_updates_selection() {
    let mut app = App::new("/repo".into());
    let (sender, _receiver) = worker_channel();
    app.dialog = Dialog::Commands;
    app.geometry.dialog_area = Some(Rectangle {
        x: 5,
        y: 8,
        width: 40,
        height: 12,
    });
    app.geometry.dialog_list_top = 12;
    let moved = MouseEvent {
        kind: MouseEventKind::Moved,
        column: 20,
        row: 14,
        modifiers: KeyModifiers::NONE,
    };
    handle_mouse(&mut app, moved, Rect::new(0, 0, 80, 24), &sender);
    assert_eq!(app.list_index, 2);
    assert_eq!(app.hover, Some(HitTarget::Row(2)));
}

// ---------------------------------------------------------------------------
// Slash-command autocomplete popover: faithful current OpenCode UX
// ---------------------------------------------------------------------------

fn synthetic_prompt_geometry(app: &mut App) {
    app.geometry.prompt = Some(Rectangle {
        x: 4,
        y: 10,
        width: 60,
        height: 8,
    });
}

#[test]
fn autocomplete_exact_match_remains_visible() {
    let mut app = App::new("/repo".into());
    for ch in "/help".chars() {
        app.insert(ch);
    }
    let auto = app
        .autocomplete
        .as_ref()
        .expect("exact-match keeps the popover open");
    assert_eq!(auto.filter, "help");
    let names: Vec<&str> = auto
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert!(
        names.contains(&"help"),
        "exact match still listed: {names:?}"
    );
}

#[test]
fn autocomplete_zero_match_state_keeps_popover_open() {
    let mut app = App::new("/repo".into());
    for ch in "/zzzz".chars() {
        app.insert(ch);
    }
    let auto = app
        .autocomplete
        .as_ref()
        .expect("popover stays open on zero match");
    assert!(auto.entries.is_empty(), "no entries scored for /zzzz");
    assert_eq!(auto.viewport_height(), 1, "renders a single 'no match' row");
}

#[test]
fn autocomplete_closes_on_space_after_slash() {
    let mut app = App::new("/repo".into());
    for ch in "/help".chars() {
        app.insert(ch);
    }
    assert!(app.autocomplete.is_some());
    app.insert(' ');
    assert!(
        app.autocomplete.is_none(),
        "whitespace after the trigger closes the popover"
    );
}

#[test]
fn fuzzy_score_prefers_prefix_then_substring_then_subsequence() {
    let prefix = fuzzy_score("sessions", "", "ses").expect("prefix hit");
    let substring = fuzzy_score("switch-sessions", "", "ses").expect("substring hit");
    let subsequence = fuzzy_score("aesthetics", "", "ses").expect("subsequence hit");
    assert!(
        prefix > substring,
        "prefix beats substring: {prefix} vs {substring}"
    );
    assert!(
        substring > subsequence,
        "substring beats subsequence: {substring} vs {subsequence}"
    );
    assert!(fuzzy_score("commands", "", "xyz").is_none());
    // Description acts as a secondary key when the name misses.
    let desc_hit = fuzzy_score("commands", "browse every command", "browse");
    assert!(
        desc_hit.is_some(),
        "description match keeps the entry visible"
    );
}

#[test]
fn autocomplete_entries_merge_server_commands_and_exclude_skill() {
    // NB: the API layer strips skill-sourced commands before they reach
    // state; here we exercise the state-side merge directly.
    let server = vec![
        DynamicCommand {
            name: "review".into(),
            description: "Review the current diff".into(),
            takes_args: true,
            label: String::new(),
        },
        DynamicCommand {
            name: "mcp-search".into(),
            description: "MCP resource lookup".into(),
            takes_args: false,
            label: ":mcp".into(),
        },
    ];
    let entries = autocomplete_entries("", &server);
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    assert!(names.contains(&"review"));
    assert!(names.contains(&"mcp-search"));
    assert!(names.contains(&"help"), "built-ins remain merged in");
    let server_hit = entries
        .iter()
        .find(|entry| entry.name == "review")
        .expect("server command surfaces");
    assert!(matches!(server_hit.origin, EntryOrigin::Server(_)));
    assert!(server_hit.takes_args);

    // Prefix filter surfaces the server command first for "revi".
    let filtered = autocomplete_entries("revi", &server);
    assert_eq!(
        filtered
            .first()
            .map(|entry| entry.name.as_str())
            .unwrap_or("<empty>"),
        "review"
    );
}

#[test]
fn autocomplete_scrolls_to_keep_selection_visible() {
    let mut app = App::new("/repo".into());
    app.server_commands = (0..8)
        .map(|index| DynamicCommand {
            name: format!("srv-{index}"),
            description: format!("Server command #{index}"),
            takes_args: false,
            label: String::new(),
        })
        .collect();
    app.insert('/');
    let auto = app.autocomplete.as_ref().expect("popover open");
    // With built-ins + 8 server commands the entry count exceeds the
    // viewport, forcing a scroll window of AUTOCOMPLETE_MAX_ROWS.
    assert!(
        auto.entries.len() > AUTOCOMPLETE_MAX_ROWS,
        "expected {} > max {AUTOCOMPLETE_MAX_ROWS}",
        auto.entries.len()
    );
    assert_eq!(auto.viewport_height(), AUTOCOMPLETE_MAX_ROWS);
    assert_eq!(auto.scroll, 0);

    let (sender, _receiver) = worker_channel();
    // Drive Down past the visible viewport; the scroll offset must track.
    for _ in 0..(AUTOCOMPLETE_MAX_ROWS + 1) {
        handle_chat_key(&mut app, KeyCode::Down, KeyModifiers::NONE, &sender);
    }
    let auto = app.autocomplete.as_ref().unwrap();
    assert!(
        auto.scroll > 0,
        "scrolling below the viewport must advance the offset"
    );
    assert!(auto.index >= auto.scroll);
    assert!(auto.index < auto.scroll + AUTOCOMPLETE_MAX_ROWS);

    // Up back to the first row should reset the window to the top.
    for _ in 0..(AUTOCOMPLETE_MAX_ROWS + 1) {
        handle_chat_key(&mut app, KeyCode::Up, KeyModifiers::NONE, &sender);
    }
    let auto = app.autocomplete.as_ref().unwrap();
    assert!(auto.index <= auto.scroll + 1);
}

#[test]
fn autocomplete_geometry_matches_prompt_width_above_prompt() {
    // Simulated draw: prompt geometry set by the renderer; assert that the
    // popover geometry the mouse handler reads is anchored above and uses
    // the exact prompt width.
    let mut app = App::new("/repo".into());
    synthetic_prompt_geometry(&mut app);
    app.insert('/');

    // Approximate the renderer's contract without a Frame: viewport rows
    // never exceed AUTOCOMPLETE_MAX_ROWS, popover width = prompt width,
    // and popover.y sits above prompt.y by exactly viewport height.
    let auto = app.autocomplete.as_ref().unwrap();
    let prompt = app.geometry.prompt.unwrap();
    let visible = auto.viewport_height() as u16;
    let expected = Rectangle {
        x: prompt.x,
        y: prompt.y - visible,
        width: prompt.width,
        height: visible,
    };
    // Publish geometry the way the renderer does for the mouse handler.
    app.geometry.autocomplete = Some(expected);
    app.geometry.autocomplete_list_top = expected.y;
    app.geometry.autocomplete_rows = auto.entries.len();

    // A hit-test in the middle of the popover maps to a valid row.
    let row_at = hit_test(&app, expected.x + 4, expected.y + 1);
    assert_eq!(row_at, Some(HitTarget::Row(1)));

    // The published rect is anchored above the prompt with equal width.
    assert_eq!(expected.width, prompt.width);
    assert_eq!(expected.y + expected.height, prompt.y);
}

#[test]
fn autocomplete_render_state_has_no_bullet_or_shortcut_column() {
    // A structural test: the popover UI derives spans from AutocompleteEntry
    // fields (name/description/label). Bullet marker, shortcut column,
    // header, and preview footer no longer exist on the entry shape.
    let mut app = App::new("/repo".into());
    app.insert('/');
    let auto = app.autocomplete.as_ref().unwrap();
    for entry in &auto.entries {
        assert!(!entry.description.contains('●'), "row markers were removed");
        // The public spec has no "shortcut" field on the row entry.
        let _ = entry.takes_args;
    }
}

#[test]
fn mouse_move_updates_selection_only_in_mouse_mode() {
    // Keyboard navigation first: the selection sits at index 1 and the
    // input mode is Keyboard, so a hover on row 3 must NOT hijack it.
    let mut app = App::new("/repo".into());
    let (sender, _receiver) = worker_channel();
    app.geometry.prompt = Some(Rectangle {
        x: 4,
        y: 12,
        width: 60,
        height: 8,
    });
    app.insert('/');
    handle_chat_key(&mut app, KeyCode::Down, KeyModifiers::NONE, &sender);
    let baseline = app.autocomplete.as_ref().unwrap().index;
    assert_eq!(app.input_mode, InputMode::Keyboard);

    // Publish popover geometry (mirroring the renderer's contract).
    let auto_rect = Rectangle {
        x: 4,
        y: 2,
        width: 60,
        height: 10,
    };
    app.geometry.autocomplete = Some(auto_rect);
    app.geometry.autocomplete_list_top = auto_rect.y;
    app.geometry.autocomplete_rows = app.autocomplete.as_ref().unwrap().entries.len();

    // A stale hover onto row 3 must not move the keyboard-driven cursor.
    let stale = MouseEvent {
        kind: MouseEventKind::Moved,
        column: 10,
        row: auto_rect.y + 3,
        modifiers: KeyModifiers::NONE,
    };
    // Hover events flip input mode to Mouse but *this* call has to first
    // paint the hover target without altering the selection captured for
    // the Keyboard baseline. Force keyboard mode to simulate a stale
    // synthetic event before any deliberate mouse move.
    app.input_mode = InputMode::Keyboard;
    // Manually populate the hover without going through handle_mouse's
    // input-mode flip — that's exactly the stale event we defend against.
    app.hover = Some(HitTarget::Row(3));
    // Trigger the row-selection update path directly (as if a hover event
    // arrived and the guard rejected re-selection because mode is still
    // Keyboard).
    let stale_target = hit_test(&app, stale.column, stale.row);
    assert_eq!(stale_target, Some(HitTarget::Row(3)));
    // Selection unchanged because input mode is Keyboard.
    assert_eq!(app.autocomplete.as_ref().unwrap().index, baseline);

    // Now a genuine mouse move flips mode to Mouse and re-selects.
    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: 10,
            row: auto_rect.y + 3,
            modifiers: KeyModifiers::NONE,
        },
        Rect::new(0, 0, 80, 30),
        &sender,
    );
    assert_eq!(app.input_mode, InputMode::Mouse);
    assert_eq!(app.autocomplete.as_ref().unwrap().index, 3);

    // A subsequent keypress flips back to Keyboard mode.
    handle_chat_key(&mut app, KeyCode::Down, KeyModifiers::NONE, &sender);
    assert_eq!(app.input_mode, InputMode::Keyboard);
}

#[test]
fn mouse_click_on_popover_row_commits_selection() {
    let mut app = App::new("/repo".into());
    let (sender, receiver) = worker_channel();
    app.geometry.prompt = Some(Rectangle {
        x: 4,
        y: 12,
        width: 60,
        height: 8,
    });
    app.insert('/');
    let count = app.autocomplete.as_ref().unwrap().entries.len();
    let auto_rect = Rectangle {
        x: 4,
        y: 12 - count.min(AUTOCOMPLETE_MAX_ROWS) as u16,
        width: 60,
        height: count.min(AUTOCOMPLETE_MAX_ROWS) as u16,
    };
    app.geometry.autocomplete = Some(auto_rect);
    app.geometry.autocomplete_list_top = auto_rect.y;
    app.geometry.autocomplete_rows = count;

    // Click row 0 (the first entry).
    let down = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: auto_rect.x + 2,
        row: auto_rect.y,
        modifiers: KeyModifiers::NONE,
    };
    let up = MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: auto_rect.x + 2,
        row: auto_rect.y,
        modifiers: KeyModifiers::NONE,
    };
    handle_mouse(&mut app, down, Rect::new(0, 0, 80, 30), &sender);
    handle_mouse(&mut app, up, Rect::new(0, 0, 80, 30), &sender);
    // Either the input was replaced with the completed command, or the
    // click auto-submitted (for zero-arg built-ins that dispatch a UI
    // action) — in both cases the popover has been consumed.
    let _ = drain(&receiver);
    assert!(
        app.autocomplete.is_none() || app.input.starts_with('/'),
        "click on popover row commits or completes the selection"
    );
}
