//! Focused unit tests for the Rust TUI: Home vs Session routing, agent
//! cycling on Home not sending server changes, first-prompt session
//! creation, slash-command autocomplete filtering and selection, and the
//! deterministic mouse hit-test used by the mouse handler.

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use opencode_tui::state::{App, Dialog, HitTarget, Rectangle, Route, COMMANDS};
use opencode_tui::{
    handle_chat_key, handle_dialog_key, handle_key, handle_mouse, hit_test, submit_prompt, Cmd,
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
    assert_eq!(auto.matches.len(), COMMANDS.len());

    app.insert('s');
    let auto = app.autocomplete.as_ref().expect("still open");
    assert!(!auto.matches.is_empty());
    // "sessions" must be one of the /s matches; "goal" must not be.
    let names: Vec<&str> = auto
        .matches
        .iter()
        .map(|position| COMMANDS[*position].name)
        .collect();
    assert!(names.contains(&"sessions"));
    assert!(!names.contains(&"goal"));

    app.insert('e');
    let auto = app.autocomplete.as_ref().expect("still open");
    let names: Vec<&str> = auto
        .matches
        .iter()
        .map(|position| COMMANDS[*position].name)
        .collect();
    assert!(names.contains(&"sessions"));
    assert!(!names.contains(&"models"));
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
