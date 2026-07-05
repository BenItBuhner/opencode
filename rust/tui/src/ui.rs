//! Rendering, mirroring the TS TUI layout: header, transcript, prompt editor,
//! status bar, and centered modal list dialogs.

use crate::state::{App, Screen};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use serde_json::Value;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(frame: &mut Frame, app: &App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(input_height(app)),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_header(frame, app, outer[0]);
    draw_transcript(frame, app, outer[1]);
    draw_input(frame, app, outer[2]);
    draw_status(frame, app, outer[3]);

    match app.screen {
        Screen::SessionList => draw_list_dialog(
            frame,
            "Sessions  (enter select · esc close)",
            &session_items(app),
            app.list_index,
        ),
        Screen::AgentList => draw_list_dialog(
            frame,
            "Agents  (enter select · esc close)",
            &agent_items(app),
            app.list_index,
        ),
        Screen::ModelList => draw_list_dialog(
            frame,
            "Models  (enter select · esc close)",
            &model_items(app),
            app.list_index,
        ),
        Screen::Help => draw_help(frame),
        Screen::Chat => {}
    }
}

fn input_height(app: &App) -> u16 {
    let lines = app.input.split('\n').count().clamp(1, 6) as u16;
    lines + 2
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let title = if app.session_title.is_empty() {
        "new session".to_string()
    } else {
        app.session_title.clone()
    };
    let left = Line::from(vec![
        Span::styled(
            " opencode ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
    ]);
    frame.render_widget(Paragraph::new(left), area);
    let right = Line::from(vec![
        Span::styled(app.agent.clone(), Style::default().fg(Color::Magenta)),
        Span::raw(" · "),
        Span::styled(app.model.clone(), Style::default().fg(Color::Blue)),
        Span::raw(" "),
    ]);
    frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
}

fn draw_transcript(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = vec![];
    for message in &app.messages {
        message_lines(message, &mut lines, area.width.saturating_sub(2));
    }
    if app.busy {
        lines.push(Line::from(Span::styled(
            format!("{} working…", SPINNER[app.spinner_frame % SPINNER.len()]),
            Style::default().fg(Color::Yellow),
        )));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "Ask anything to get started.",
            Style::default().fg(Color::DarkGray),
        )));
    }
    let total = lines.len() as u16;
    let visible = area.height;
    let bottom_start = total.saturating_sub(visible);
    let offset = bottom_start.saturating_sub(app.scroll);
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((offset, 0));
    frame.render_widget(paragraph, area);
}

fn message_lines(message: &Value, lines: &mut Vec<Line>, _width: u16) {
    match message.get("type").and_then(Value::as_str).unwrap_or("") {
        "user" => {
            lines.push(Line::default());
            for (index, part) in message
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .split('\n')
                .enumerate()
            {
                lines.push(Line::from(vec![
                    Span::styled(
                        if index == 0 { "❯ " } else { "  " },
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        part.to_string(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ]));
            }
        }
        "assistant" => {
            for part in message
                .get("content")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                match part.get("type").and_then(Value::as_str).unwrap_or("") {
                    "reasoning" => {
                        let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
                        if !text.is_empty() {
                            let preview: String = text.chars().take(120).collect();
                            lines.push(Line::from(Span::styled(
                                format!(
                                    "✱ {}{}",
                                    preview.replace('\n', " "),
                                    if text.chars().count() > 120 {
                                        "…"
                                    } else {
                                        ""
                                    }
                                ),
                                Style::default()
                                    .fg(Color::DarkGray)
                                    .add_modifier(Modifier::ITALIC),
                            )));
                        }
                    }
                    "text" => {
                        for row in part
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .split('\n')
                        {
                            lines.push(Line::from(Span::raw(row.to_string())));
                        }
                    }
                    "tool" => {
                        let name = part.get("name").and_then(Value::as_str).unwrap_or("tool");
                        let status = part
                            .get("state")
                            .and_then(|state| state.get("status"))
                            .and_then(Value::as_str)
                            .unwrap_or("pending");
                        let color = match status {
                            "completed" => Color::Green,
                            "error" => Color::Red,
                            _ => Color::Yellow,
                        };
                        let detail = tool_detail(&part);
                        lines.push(Line::from(vec![
                            Span::styled("⚙ ", Style::default().fg(color)),
                            Span::styled(
                                name.to_string(),
                                Style::default().fg(color).add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                format!(" {detail}"),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]));
                    }
                    _ => {}
                }
            }
        }
        "agent-switched" => {
            lines.push(Line::from(Span::styled(
                format!(
                    "· agent → {}",
                    message.get("agent").and_then(Value::as_str).unwrap_or("?")
                ),
                Style::default().fg(Color::Magenta),
            )));
        }
        "model-switched" => {
            let model = message
                .get("model")
                .map(|model| {
                    format!(
                        "{}/{}",
                        model
                            .get("providerID")
                            .and_then(Value::as_str)
                            .unwrap_or("?"),
                        model.get("id").and_then(Value::as_str).unwrap_or("?")
                    )
                })
                .unwrap_or_default();
            lines.push(Line::from(Span::styled(
                format!("· model → {model}"),
                Style::default().fg(Color::Blue),
            )));
        }
        "system" | "synthetic" => {
            lines.push(Line::from(Span::styled(
                format!(
                    "· {}",
                    message
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .lines()
                        .next()
                        .unwrap_or_default()
                ),
                Style::default().fg(Color::DarkGray),
            )));
        }
        _ => {}
    }
}

fn tool_detail(part: &Value) -> String {
    let input = part
        .get("state")
        .and_then(|state| state.get("input"))
        .cloned()
        .unwrap_or(Value::Null);
    let text = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let name = part.get("name").and_then(Value::as_str).unwrap_or("");
    let detail = match name {
        "bash" => text("command"),
        "read" | "edit" | "write" => text("path"),
        "glob" | "grep" => text("pattern"),
        "webfetch" => text("url"),
        "skill" => text("name"),
        _ => String::new(),
    };
    detail.chars().take(60).collect()
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if app.busy { Color::Yellow } else { Color::Cyan }));
    let content = if app.input.is_empty() {
        Paragraph::new(Span::styled(
            "Ask anything…  (enter send · alt+enter newline · ctrl+x leader)",
            Style::default().fg(Color::DarkGray),
        ))
        .block(block)
    } else {
        Paragraph::new(app.input.clone())
            .wrap(Wrap { trim: false })
            .block(block)
    };
    frame.render_widget(content, area);
    // Place the terminal cursor at the logical position inside the editor.
    let before: String = app.input.chars().take(app.cursor).collect();
    let row = before.matches('\n').count() as u16;
    let column = before.split('\n').next_back().unwrap_or("").chars().count() as u16;
    frame.set_cursor_position((
        area.x + 1 + column.min(area.width.saturating_sub(3)),
        area.y + 1 + row.min(area.height.saturating_sub(3)),
    ));
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let left = match &app.toast {
        Some(toast) => Line::from(Span::styled(
            format!(" {toast}"),
            Style::default().fg(Color::Yellow),
        )),
        None => Line::from(Span::styled(
            format!(" {}", app.directory),
            Style::default().fg(Color::DarkGray),
        )),
    };
    frame.render_widget(Paragraph::new(left), area);
    let hint = if app.leader {
        "leader: q quit · n new · l sessions · a agents · m models · ? help"
    } else {
        "ctrl+x leader · esc interrupt · pgup/pgdn scroll · ctrl+c quit"
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{hint} "),
            Style::default().fg(if app.leader {
                Color::Yellow
            } else {
                Color::DarkGray
            }),
        )))
        .alignment(Alignment::Right),
        area,
    );
}

fn session_items(app: &App) -> Vec<String> {
    app.sessions
        .iter()
        .map(|session| {
            let title = session
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("untitled");
            let directory = session
                .get("location")
                .and_then(|location| location.get("directory"))
                .and_then(Value::as_str)
                .unwrap_or("");
            format!("{title}  ({directory})")
        })
        .collect()
}

fn agent_items(app: &App) -> Vec<String> {
    app.agents
        .iter()
        .map(|agent| {
            let name = agent
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_else(|| agent.get("name").and_then(Value::as_str).unwrap_or("agent"));
            let description = agent
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            format!(
                "{name}  {}",
                description.chars().take(70).collect::<String>()
            )
        })
        .collect()
}

fn model_items(app: &App) -> Vec<String> {
    app.models
        .iter()
        .map(|(provider, model)| format!("{provider}/{model}"))
        .collect()
}

fn draw_list_dialog(frame: &mut Frame, title: &str, items: &[String], selected: usize) {
    let area = centered(frame.area(), 70, 60);
    frame.render_widget(Clear, area);
    let list = List::new(
        items
            .iter()
            .map(|item| ListItem::new(item.clone()))
            .collect::<Vec<_>>(),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(title.to_string())
            .border_style(Style::default().fg(Color::Cyan)),
    )
    .highlight_style(
        Style::default()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    );
    let mut state = ListState::default();
    state.select(Some(selected.min(items.len().saturating_sub(1))));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_help(frame: &mut Frame) {
    let area = centered(frame.area(), 64, 60);
    frame.render_widget(Clear, area);
    let bindings = [
        ("enter", "send prompt"),
        ("alt+enter", "insert newline"),
        ("esc", "interrupt running session / close dialog"),
        ("pageup / pagedown", "scroll transcript"),
        ("ctrl+x q", "quit"),
        ("ctrl+x n", "new session"),
        ("ctrl+x l", "list sessions"),
        ("ctrl+x a", "list agents"),
        ("ctrl+x m", "list models"),
        ("ctrl+c", "quit"),
    ];
    let lines: Vec<Line> = bindings
        .iter()
        .map(|(key, action)| {
            Line::from(vec![
                Span::styled(
                    format!("  {key:<20}"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw((*action).to_string()),
            ])
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Help  (esc close)")
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        area,
    );
}

fn centered(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}
