//! Faithful rendering of the OpenCode TUI (packages/tui), reproduced with
//! ratatui: the home route (block logo + centered prompt + tips + footer),
//! the session route (message list with `┃` gutters, per-tool icon rows,
//! thought headers, metadata footers), the prompt editor (left `┃` accent
//! border, agent row, connector, status row with the block spinner), and
//! centered DialogSelect panels with search and `●` markers.

use crate::state::{fuzzy, App, Dialog};
use crate::theme;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use ratatui::Frame;
use serde_json::Value;

pub fn draw(frame: &mut Frame, app: &App) {
    frame.render_widget(
        Block::default().style(Style::default().bg(theme::BACKGROUND)),
        frame.area(),
    );
    if app.is_home() {
        draw_home(frame, app);
    } else {
        draw_session(frame, app);
    }
    match app.dialog {
        Dialog::None => {}
        Dialog::Help => draw_help(frame, app),
        _ => draw_dialog_select(frame, app),
    }
}

// ---------------------------------------------------------------------------
// Home route: centered logo, prompt, tip, footer
// ---------------------------------------------------------------------------

fn draw_home(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let prompt_width = (area.width * 7 / 10)
        .clamp(40, 75)
        .min(area.width.saturating_sub(2));
    let editor = editor_height(app);
    // Logo(4) + gap(1) + prompt block + tip row.
    let content_height = 4 + 1 + editor + 3 + 2;
    let top = area.height.saturating_sub(content_height + 2) / 2;

    let logo_x = area.x + area.width.saturating_sub(crate::logo::WIDTH) / 2;
    for (row, line) in crate::logo::lines().into_iter().enumerate() {
        let rect = Rect::new(
            logo_x,
            area.y + top + row as u16,
            crate::logo::WIDTH.min(area.width),
            1,
        );
        frame.render_widget(Paragraph::new(line), rect);
    }

    let prompt_x = area.x + area.width.saturating_sub(prompt_width) / 2;
    let prompt_area = Rect::new(prompt_x, area.y + top + 5, prompt_width, editor + 3);
    draw_prompt(frame, app, prompt_area);

    // Tip row (feature-plugins/home/tips-view.tsx): "● Tip  <text>".
    let tip = TIPS[app.tip_index % TIPS.len()];
    let tip_area = Rect::new(
        prompt_x,
        prompt_area.y + prompt_area.height + 1,
        prompt_width,
        1,
    );
    let mut spans = vec![Span::styled("● Tip ", Style::default().fg(theme::WARNING))];
    spans.extend(tip_spans(tip));
    frame.render_widget(Paragraph::new(Line::from(spans)), tip_area);

    // Home footer: directory left, version right (both muted).
    let footer = Rect::new(
        area.x + 2,
        area.y + area.height.saturating_sub(2),
        area.width.saturating_sub(4),
        1,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            app.directory.clone(),
            Style::default().fg(theme::TEXT_MUTED),
        )),
        footer,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            format!("opencode-rust v{}", app.version),
            Style::default().fg(theme::TEXT_MUTED),
        ))
        .alignment(Alignment::Right),
        footer,
    );
}

/// Tip strings from tips-view.tsx TIPS, with {highlight} spans -> theme.text.
const TIPS: [&str; 6] = [
    "Start a message with {!} to run shell commands directly (e.g., {!ls -la})",
    "Use {/models} to see and switch between available AI models",
    "Use {/new} to start a fresh conversation session",
    "Use {/sessions} to list, pin, and continue sessions",
    "The leader key is {ctrl+x}; combine with other keys for quick actions",
    "Press {ctrl+p} to see all available actions and commands",
];

fn tip_spans(tip: &str) -> Vec<Span<'static>> {
    let mut spans = vec![];
    let mut rest = tip;
    while let Some(start) = rest.find('{') {
        if !rest[..start].is_empty() {
            spans.push(Span::styled(
                rest[..start].to_string(),
                Style::default().fg(theme::TEXT_MUTED),
            ));
        }
        let Some(end) = rest[start..].find('}') else {
            break;
        };
        spans.push(Span::styled(
            rest[start + 1..start + end].to_string(),
            Style::default().fg(theme::TEXT),
        ));
        rest = &rest[start + end + 1..];
    }
    if !rest.is_empty() {
        spans.push(Span::styled(
            rest.to_string(),
            Style::default().fg(theme::TEXT_MUTED),
        ));
    }
    spans
}

// ---------------------------------------------------------------------------
// Session route: transcript + prompt
// ---------------------------------------------------------------------------

fn draw_session(frame: &mut Frame, app: &App) {
    let area = frame.area();
    // Sidebar (width 42) appears automatically on terminals wider than 120.
    let (main, sidebar) = if area.width > 120 {
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(10), Constraint::Length(42)])
            .split(area);
        (split[0], Some(split[1]))
    } else {
        (area, None)
    };
    let editor = editor_height(app);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(editor + 3),
            Constraint::Length(1),
        ])
        .split(Rect::new(
            main.x + 2,
            main.y,
            main.width.saturating_sub(4),
            main.height,
        ));

    draw_transcript(frame, app, rows[0]);
    draw_prompt(frame, app, rows[1]);
    if let Some(sidebar) = sidebar {
        draw_sidebar(frame, app, sidebar);
    }
    if let Some(toast) = &app.toast {
        draw_toast(frame, toast, area);
    }
}

fn draw_transcript(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = vec![Line::default()];
    let last_assistant = app
        .messages
        .iter()
        .rposition(|message| message.get("type").and_then(Value::as_str) == Some("assistant"));
    for (index, message) in app.messages.iter().enumerate() {
        message_lines(
            app,
            message,
            &mut lines,
            index == 0,
            Some(index) == last_assistant,
        );
    }
    if app.busy {
        lines.push(Line::default());
        lines.push(Line::from(vec![
            Span::styled(
                block_spinner(app.frame),
                Style::default().fg(app.agent_color()),
            ),
            Span::styled("  ", Style::default()),
            Span::styled(
                if app.interrupts > 0 {
                    "esc again to interrupt"
                } else {
                    "esc interrupt"
                },
                Style::default().fg(if app.interrupts > 0 {
                    theme::PRIMARY
                } else {
                    theme::TEXT_MUTED
                }),
            ),
        ]));
    }
    let total = lines.len() as u16;
    let offset = total.saturating_sub(area.height).saturating_sub(app.scroll);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((offset, 0)),
        area,
    );
}

/// The prompt-row Knight Rider block spinner (ui/spinner.ts "blocks").
fn block_spinner(frame: usize) -> String {
    const WIDTH: usize = 8;
    let cycle = WIDTH * 2 - 2;
    let position = frame % cycle;
    let active = if position < WIDTH {
        position
    } else {
        cycle - position
    };
    (0..WIDTH)
        .map(|index| if index == active { '■' } else { '⬝' })
        .collect()
}

fn message_lines(
    app: &App,
    message: &Value,
    lines: &mut Vec<Line>,
    first: bool,
    is_last_assistant: bool,
) {
    match message.get("type").and_then(Value::as_str).unwrap_or("") {
        "user" => {
            if !first {
                lines.push(Line::default());
            }
            // User block: `┃` gutter in agent color, panel background.
            let gutter = Style::default().fg(app.agent_color());
            let body = Style::default().fg(theme::TEXT).bg(theme::BACKGROUND_PANEL);
            let text = message
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            lines.push(Line::from(vec![
                Span::styled("┃", gutter),
                Span::styled("  ", body),
            ]));
            for row in text.split('\n') {
                lines.push(Line::from(vec![
                    Span::styled("┃", gutter),
                    Span::styled(format!("  {row}"), body),
                ]));
            }
            lines.push(Line::from(vec![
                Span::styled("┃", gutter),
                Span::styled("  ", body),
            ]));
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
                        if text.is_empty() {
                            continue;
                        }
                        lines.push(Line::default());
                        let title: String = text
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .chars()
                            .take(80)
                            .collect();
                        let duration = part_duration(&part);
                        lines.push(Line::from(Span::styled(
                            format!("   Thought: {title}{duration}"),
                            Style::default().fg(theme::thinking()),
                        )));
                    }
                    "text" => {
                        let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
                        if text.is_empty() {
                            continue;
                        }
                        lines.push(Line::default());
                        for row in text.split('\n') {
                            lines.push(Line::from(Span::styled(
                                format!("   {row}"),
                                Style::default().fg(theme::TEXT),
                            )));
                        }
                    }
                    "tool" => tool_lines(&part, lines),
                    _ => {}
                }
            }
            // Metadata footer for the final assistant message:
            // ▣ {Agent} · {model} · {duration}
            if is_last_assistant
                && message
                    .get("time")
                    .and_then(|t| t.get("completed"))
                    .is_some()
            {
                let model = message
                    .get("model")
                    .and_then(|model| model.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("?");
                let agent = message
                    .get("agent")
                    .and_then(Value::as_str)
                    .unwrap_or("build");
                let duration = message_duration(message);
                lines.push(Line::default());
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("   ▣ {}", titlecase(agent)),
                        Style::default().fg(theme::agent_color(agent, app.agent_index)),
                    ),
                    Span::styled(
                        format!(" · {model}{duration}"),
                        Style::default().fg(theme::TEXT_MUTED),
                    ),
                ]));
            }
        }
        "agent-switched" => {
            let agent = message.get("agent").and_then(Value::as_str).unwrap_or("?");
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                format!("   ▣ Switched to {} agent", titlecase(agent)),
                Style::default().fg(theme::agent_color(agent, app.agent_index)),
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
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                format!("   ▣ Switched to {model}"),
                Style::default().fg(theme::TEXT_MUTED),
            )));
        }
        _ => {}
    }
}

/// Inline tool rows: 2-char icon + title, per the TS tool registry.
fn tool_lines(part: &Value, lines: &mut Vec<Line>) {
    let name = part.get("name").and_then(Value::as_str).unwrap_or("tool");
    let state = part.get("state").cloned().unwrap_or(Value::Null);
    let status = state
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    let input = state.get("input").cloned().unwrap_or(Value::Null);
    let text = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let matches = || {
        state
            .get("structured")
            .and_then(Value::as_array)
            .map(|items| items.len())
            .unwrap_or(0)
    };

    // Todowrite renders as a block: `┃  # Todos` + checkbox items.
    if name == "todowrite" {
        lines.push(Line::default());
        let border = Style::default().fg(theme::BORDER);
        let panel = Style::default()
            .fg(theme::TEXT_MUTED)
            .bg(theme::BACKGROUND_PANEL);
        lines.push(Line::from(vec![
            Span::styled("┃", border),
            Span::styled("  # Todos", panel),
        ]));
        for todo in input
            .get("todos")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let todo_status = todo
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending");
            let mark = match todo_status {
                "completed" => "✓",
                "in_progress" => "•",
                _ => " ",
            };
            let color = if todo_status == "in_progress" {
                theme::WARNING
            } else {
                theme::TEXT_MUTED
            };
            lines.push(Line::from(vec![
                Span::styled("┃", border),
                Span::styled(
                    format!(
                        "  [{mark}] {}",
                        todo.get("content")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                    ),
                    Style::default().fg(color).bg(theme::BACKGROUND_PANEL),
                ),
            ]));
        }
        return;
    }

    let (icon, title) = match name {
        "bash" => ("$", text("command")),
        "read" => ("→", format!("Read {}", text("path"))),
        "write" => ("←", format!("Write {}", text("path"))),
        "edit" => ("←", format!("Edit {}", text("path"))),
        "glob" => (
            "✱",
            format!(
                "Glob \"{}\" ({} match{})",
                text("pattern"),
                matches(),
                if matches() == 1 { "" } else { "es" }
            ),
        ),
        "grep" => (
            "✱",
            format!(
                "Grep \"{}\" ({} match{})",
                text("pattern"),
                matches(),
                if matches() == 1 { "" } else { "es" }
            ),
        ),
        "webfetch" => ("%", format!("WebFetch {}", text("url"))),
        "skill" => ("→", format!("Skill \"{}\"", text("name"))),
        _ => ("⚙", name.to_string()),
    };
    let (icon_style, title_style) = match status {
        "error" => (
            Style::default().fg(theme::ERROR),
            Style::default().fg(theme::ERROR),
        ),
        "completed" => (
            Style::default().fg(theme::TEXT_MUTED),
            Style::default().fg(theme::TEXT_MUTED),
        ),
        _ => (
            Style::default().fg(theme::TEXT),
            Style::default().fg(theme::TEXT),
        ),
    };
    lines.push(Line::from(vec![
        Span::raw("   "),
        Span::styled(format!("{icon} "), icon_style),
        Span::styled(title, title_style),
    ]));
    if status == "error" {
        let message = state
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("failed");
        lines.push(Line::from(Span::styled(
            format!("     {message}"),
            Style::default().fg(theme::ERROR),
        )));
    }
}

fn part_duration(part: &Value) -> String {
    let time = part.get("time").cloned().unwrap_or(Value::Null);
    match (
        time.get("created").and_then(Value::as_i64),
        time.get("completed").and_then(Value::as_i64),
    ) {
        (Some(created), Some(completed)) if completed > created => {
            format!(" · {}", duration_label(completed - created))
        }
        _ => String::new(),
    }
}

fn message_duration(message: &Value) -> String {
    let time = message.get("time").cloned().unwrap_or(Value::Null);
    match (
        time.get("created").and_then(Value::as_i64),
        time.get("completed").and_then(Value::as_i64),
    ) {
        (Some(created), Some(completed)) if completed > created => {
            format!(" · {}", duration_label(completed - created))
        }
        _ => String::new(),
    }
}

fn duration_label(millis: i64) -> String {
    if millis < 1000 {
        return format!("{millis}ms");
    }
    if millis < 60_000 {
        return format!("{:.1}s", millis as f64 / 1000.0);
    }
    format!("{}m {}s", millis / 60_000, millis % 60_000 / 1000)
}

fn titlecase(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Prompt editor: `┃` accent border, textarea, agent row, connector, status
// ---------------------------------------------------------------------------

fn editor_height(app: &App) -> u16 {
    app.input.split('\n').count().clamp(1, 6) as u16
}

fn draw_prompt(frame: &mut Frame, app: &App, area: Rect) {
    let editor = editor_height(app);
    let accent = app.agent_color();
    let shell_mode = app.input.starts_with('!');
    let border_color = if app.leader {
        theme::BORDER
    } else if shell_mode {
        theme::PRIMARY
    } else {
        theme::tint(theme::BORDER, accent, 0.7)
    };
    let element = Style::default().bg(theme::BACKGROUND_ELEMENT);

    // Row 0..editor: textarea with left `┃` border and element background.
    for row in 0..editor {
        let rect = Rect::new(area.x, area.y + row, area.width, 1);
        let content: String = app
            .input
            .split('\n')
            .nth(row as usize)
            .unwrap_or_default()
            .to_string();
        let mut spans = vec![Span::styled("┃", Style::default().fg(border_color))];
        if row == 0 && app.input.is_empty() {
            spans.push(Span::styled(
                "  Ask anything... \"Fix a TODO in the codebase\"",
                Style::default()
                    .fg(theme::TEXT_MUTED)
                    .bg(theme::BACKGROUND_ELEMENT),
            ));
        } else {
            spans.push(Span::styled(
                format!("  {content}"),
                Style::default()
                    .fg(theme::TEXT)
                    .bg(theme::BACKGROUND_ELEMENT),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)).style(element), rect);
    }

    // Agent row: {Agent} · {model} {provider}
    let agent_row = Rect::new(area.x, area.y + editor, area.width, 1);
    let label = if shell_mode {
        "Shell".to_string()
    } else {
        titlecase(&app.agent)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("┃", Style::default().fg(border_color)),
            Span::styled(
                format!("  {label}"),
                Style::default()
                    .fg(theme::tint(
                        theme::BACKGROUND_ELEMENT,
                        if shell_mode { theme::PRIMARY } else { accent },
                        0.8,
                    ))
                    .bg(theme::BACKGROUND_ELEMENT),
            ),
            Span::styled(
                format!(" · {} {}", app.model, app.provider),
                Style::default()
                    .fg(theme::TEXT_MUTED)
                    .bg(theme::BACKGROUND_ELEMENT),
            ),
        ]))
        .style(element),
        agent_row,
    );

    // Connector row: `╹` + `▀` bottom edge of the element background.
    let connector = Rect::new(area.x, area.y + editor + 1, area.width, 1);
    let mut edge = String::from("╹");
    edge.push_str(&"▀".repeat(area.width.saturating_sub(1) as usize));
    frame.render_widget(
        Paragraph::new(Span::styled(
            edge,
            Style::default().fg(theme::BACKGROUND_ELEMENT),
        )),
        connector,
    );
    let status = Rect::new(area.x, area.y + editor + 2, area.width, 1);
    draw_prompt_status(frame, app, status);

    // Terminal cursor inside the textarea.
    if app.dialog == Dialog::None {
        let before: String = app.input.chars().take(app.cursor).collect();
        let row = before.matches('\n').count() as u16;
        let column = before.split('\n').next_back().unwrap_or("").chars().count() as u16;
        frame.set_cursor_position((
            area.x + 3 + column.min(area.width.saturating_sub(4)),
            area.y + row.min(editor.saturating_sub(1)),
        ));
    }
}

fn draw_prompt_status(frame: &mut Frame, app: &App, area: Rect) {
    let left = if app.busy {
        Line::from(vec![
            Span::styled(
                block_spinner(app.frame),
                Style::default().fg(app.agent_color()),
            ),
            Span::styled(
                if app.interrupts > 0 {
                    "  esc again to interrupt"
                } else {
                    "  esc interrupt"
                },
                Style::default().fg(if app.interrupts > 0 {
                    theme::PRIMARY
                } else {
                    theme::TEXT_MUTED
                }),
            ),
        ])
    } else if let Some(toast) = &app.toast {
        Line::from(Span::styled(
            toast.clone(),
            Style::default().fg(theme::WARNING),
        ))
    } else {
        Line::from(Span::styled(
            app.directory.clone(),
            Style::default().fg(theme::TEXT_MUTED),
        ))
    };
    frame.render_widget(Paragraph::new(left), area);

    let hint = if app.leader {
        Line::from(Span::styled(
            "leader · q quit  n new  l sessions  a agents  m models  ? help",
            Style::default().fg(theme::WARNING),
        ))
    } else {
        Line::from(vec![
            Span::styled("ctrl+x a", Style::default().fg(theme::TEXT)),
            Span::styled(" agents  ", Style::default().fg(theme::TEXT_MUTED)),
            Span::styled("ctrl+p", Style::default().fg(theme::TEXT)),
            Span::styled(" commands", Style::default().fg(theme::TEXT_MUTED)),
        ])
    };
    frame.render_widget(Paragraph::new(hint).alignment(Alignment::Right), area);
}

// ---------------------------------------------------------------------------
// Sidebar (width 42, backgroundPanel): title, todos, footer
// ---------------------------------------------------------------------------

fn draw_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    frame.render_widget(
        Block::default().style(Style::default().bg(theme::BACKGROUND_PANEL)),
        area,
    );
    let inner = Rect::new(
        area.x + 2,
        area.y + 1,
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
    );
    let mut lines = vec![Line::from(Span::styled(
        app.session_title.clone(),
        Style::default()
            .fg(theme::TEXT)
            .bg(theme::BACKGROUND_PANEL)
            .add_modifier(Modifier::BOLD),
    ))];
    if !app.todos.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "Todos",
            Style::default()
                .fg(theme::ACCENT)
                .bg(theme::BACKGROUND_PANEL)
                .add_modifier(Modifier::BOLD),
        )));
        for todo in &app.todos {
            let status = todo
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending");
            let mark = match status {
                "completed" => "✓",
                "in_progress" => "•",
                _ => " ",
            };
            let color = if status == "in_progress" {
                theme::WARNING
            } else {
                theme::TEXT_MUTED
            };
            lines.push(Line::from(Span::styled(
                format!(
                    "[{mark}] {}",
                    todo.get("content")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                ),
                Style::default().fg(color).bg(theme::BACKGROUND_PANEL),
            )));
        }
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);

    // Sidebar footer: `• OpenCode {version}`.
    let footer = Rect::new(
        inner.x,
        area.y + area.height.saturating_sub(2),
        inner.width,
        1,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "• ",
                Style::default()
                    .fg(theme::SUCCESS)
                    .bg(theme::BACKGROUND_PANEL),
            ),
            Span::styled(
                "Open",
                Style::default()
                    .fg(theme::TEXT_MUTED)
                    .bg(theme::BACKGROUND_PANEL),
            ),
            Span::styled(
                "Code",
                Style::default()
                    .fg(theme::TEXT)
                    .bg(theme::BACKGROUND_PANEL)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" v{}", app.version),
                Style::default()
                    .fg(theme::TEXT_MUTED)
                    .bg(theme::BACKGROUND_PANEL),
            ),
        ])),
        footer,
    );
}

// ---------------------------------------------------------------------------
// DialogSelect: centered panel, search input, categories, ● markers
// ---------------------------------------------------------------------------

pub struct DialogOption {
    pub category: Option<String>,
    pub title: String,
    pub description: String,
    pub footer: String,
}

pub fn dialog_options(app: &App) -> Vec<DialogOption> {
    let filter =
        |title: &str, description: &str| fuzzy(&format!("{title} {description}"), &app.search);
    match app.dialog {
        Dialog::Commands => COMMANDS
            .iter()
            .filter(|(title, description, _)| filter(title, description))
            .map(|(title, description, footer)| DialogOption {
                category: None,
                title: (*title).to_string(),
                description: (*description).to_string(),
                footer: (*footer).to_string(),
            })
            .collect(),
        Dialog::Sessions => app
            .sessions
            .iter()
            .filter(|session| {
                filter(
                    session.get("title").and_then(Value::as_str).unwrap_or(""),
                    "",
                )
            })
            .map(|session| DialogOption {
                category: None,
                title: session
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("untitled")
                    .to_string(),
                description: session
                    .get("location")
                    .and_then(|location| location.get("directory"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                footer: String::new(),
            })
            .collect(),
        Dialog::Agents => app
            .agents
            .iter()
            .filter(|agent| {
                filter(
                    agent
                        .get("id")
                        .or_else(|| agent.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                    agent
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                )
            })
            .map(|agent| DialogOption {
                category: None,
                title: titlecase(
                    agent
                        .get("id")
                        .or_else(|| agent.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("agent"),
                ),
                description: agent
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars()
                    .take(60)
                    .collect(),
                footer: String::new(),
            })
            .collect(),
        Dialog::Models => app
            .models
            .iter()
            .filter(|(provider, model)| filter(&format!("{provider}/{model}"), ""))
            .map(|(provider, model)| DialogOption {
                category: Some(titlecase(provider)),
                title: model.clone(),
                description: String::new(),
                footer: if provider == "opencode" {
                    "Free".into()
                } else {
                    String::new()
                },
            })
            .collect(),
        _ => vec![],
    }
}

pub const COMMANDS: [(&str, &str, &str); 6] = [
    (
        "New session",
        "Start a fresh conversation session",
        "ctrl+x n",
    ),
    ("Switch session", "List and continue sessions", "ctrl+x l"),
    ("Switch agent", "Choose the active agent", "ctrl+x a"),
    (
        "Switch model",
        "See and switch between available AI models",
        "ctrl+x m",
    ),
    ("Help", "Show keybindings", "ctrl+x ?"),
    ("Exit the application", "", "ctrl+c"),
];

fn dialog_title(dialog: Dialog) -> &'static str {
    match dialog {
        Dialog::Commands => "Commands",
        Dialog::Sessions => "Switch session",
        Dialog::Agents => "Select agent",
        Dialog::Models => "Select model",
        _ => "",
    }
}

fn draw_dialog_select(frame: &mut Frame, app: &App) {
    let screen = frame.area();
    let width = match app.dialog {
        Dialog::Sessions | Dialog::Models => 88u16,
        _ => 60u16,
    }
    .min(screen.width.saturating_sub(2));
    let options = dialog_options(app);
    let list_height = (screen.height / 2).saturating_sub(6).max(4);
    let height = (list_height + 4).min(screen.height.saturating_sub(4));
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().bg(theme::BACKGROUND_PANEL)),
        area,
    );

    let panel = Style::default().bg(theme::BACKGROUND_PANEL);
    // Title row: bold title left, "esc" right.
    let title_row = Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(4), 1);
    frame.render_widget(
        Paragraph::new(Span::styled(
            dialog_title(app.dialog),
            Style::default()
                .fg(theme::TEXT)
                .bg(theme::BACKGROUND_PANEL)
                .add_modifier(Modifier::BOLD),
        ))
        .style(panel),
        title_row,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            "esc",
            Style::default()
                .fg(theme::TEXT_MUTED)
                .bg(theme::BACKGROUND_PANEL),
        ))
        .alignment(Alignment::Right)
        .style(panel),
        title_row,
    );

    // Search input.
    let search_row = Rect::new(area.x + 2, area.y + 2, area.width.saturating_sub(4), 1);
    let search_display = if app.search.is_empty() {
        Span::styled(
            "Search",
            Style::default()
                .fg(theme::TEXT_MUTED)
                .bg(theme::BACKGROUND_PANEL),
        )
    } else {
        Span::styled(
            app.search.clone(),
            Style::default().fg(theme::TEXT).bg(theme::BACKGROUND_PANEL),
        )
    };
    frame.render_widget(Paragraph::new(search_display).style(panel), search_row);
    frame.set_cursor_position((
        search_row.x + app.search.chars().count() as u16,
        search_row.y,
    ));

    // Options with category headers, `●` marker, active bg=primary.
    let list_area = Rect::new(
        area.x,
        area.y + 4,
        area.width,
        area.height.saturating_sub(4),
    );
    let mut lines: Vec<Line> = vec![];
    let mut last_category: Option<String> = None;
    let mut selected_line = 0usize;
    for (index, option) in options.iter().enumerate() {
        if option.category != last_category {
            if let Some(category) = &option.category {
                lines.push(Line::from(Span::styled(
                    format!("   {category}"),
                    Style::default()
                        .fg(theme::ACCENT)
                        .bg(theme::BACKGROUND_PANEL)
                        .add_modifier(Modifier::BOLD),
                )));
            }
            last_category = option.category.clone();
        }
        let active = index == app.list_index.min(options.len().saturating_sub(1));
        if active {
            selected_line = lines.len();
        }
        let (fg, muted, bg) = if active {
            (theme::SELECTED_FG, theme::SELECTED_FG, theme::PRIMARY)
        } else {
            (theme::TEXT, theme::TEXT_MUTED, theme::BACKGROUND_PANEL)
        };
        let marker = if active { " ● " } else { "   " };
        let mut spans = vec![
            Span::styled(marker, Style::default().fg(fg).bg(bg)),
            Span::styled(
                option.title.clone(),
                Style::default().fg(fg).bg(bg).add_modifier(if active {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
            ),
        ];
        if !option.description.is_empty() {
            spans.push(Span::styled(
                format!("  {}", option.description),
                Style::default().fg(muted).bg(bg),
            ));
        }
        let used: usize = spans.iter().map(|span| span.content.chars().count()).sum();
        let padding = (area.width as usize)
            .saturating_sub(used + option.footer.chars().count() + 2)
            .max(2);
        spans.push(Span::styled(" ".repeat(padding), Style::default().bg(bg)));
        if !option.footer.is_empty() {
            spans.push(Span::styled(
                format!("{}  ", option.footer),
                Style::default().fg(muted).bg(bg),
            ));
        }
        lines.push(Line::from(spans));
    }
    if options.is_empty() {
        lines.push(Line::from(Span::styled(
            "   No results found",
            Style::default()
                .fg(theme::TEXT_MUTED)
                .bg(theme::BACKGROUND_PANEL),
        )));
    }
    let offset = (selected_line as u16).saturating_sub(list_area.height.saturating_sub(1));
    frame.render_widget(
        Paragraph::new(lines).style(panel).scroll((offset, 0)),
        list_area,
    );
}

fn draw_help(frame: &mut Frame, _app: &App) {
    let screen = frame.area();
    let width = 60u16.min(screen.width.saturating_sub(2));
    let bindings = [
        ("enter", "send prompt"),
        ("alt+enter", "insert newline"),
        ("esc", "interrupt session / close dialog"),
        ("pageup / pagedown", "scroll messages"),
        ("ctrl+p", "command palette"),
        ("ctrl+x n", "new session"),
        ("ctrl+x l", "switch session"),
        ("ctrl+x a", "select agent"),
        ("ctrl+x m", "select model"),
        ("ctrl+x q / ctrl+c", "exit the application"),
    ];
    let height = (bindings.len() as u16 + 4).min(screen.height.saturating_sub(4));
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().bg(theme::BACKGROUND_PANEL)),
        area,
    );
    let panel = Style::default().bg(theme::BACKGROUND_PANEL);
    let title_row = Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(4), 1);
    frame.render_widget(
        Paragraph::new(Span::styled(
            "Help",
            Style::default()
                .fg(theme::TEXT)
                .bg(theme::BACKGROUND_PANEL)
                .add_modifier(Modifier::BOLD),
        ))
        .style(panel),
        title_row,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            "esc",
            Style::default()
                .fg(theme::TEXT_MUTED)
                .bg(theme::BACKGROUND_PANEL),
        ))
        .alignment(Alignment::Right)
        .style(panel),
        title_row,
    );
    let mut lines = vec![];
    for (key, action) in bindings {
        lines.push(Line::from(vec![
            Span::styled(
                format!("   {key:<20}"),
                Style::default()
                    .fg(theme::TEXT)
                    .bg(theme::BACKGROUND_PANEL)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                action.to_string(),
                Style::default()
                    .fg(theme::TEXT_MUTED)
                    .bg(theme::BACKGROUND_PANEL),
            ),
        ]));
    }
    frame.render_widget(
        Paragraph::new(lines).style(panel),
        Rect::new(
            area.x,
            area.y + 3,
            area.width,
            area.height.saturating_sub(3),
        ),
    );
}

fn draw_toast(frame: &mut Frame, toast: &str, screen: Rect) {
    let width = (toast.chars().count() as u16 + 6)
        .min(60)
        .min(screen.width.saturating_sub(6));
    let area = Rect::new(
        screen.x + screen.width.saturating_sub(width + 2),
        screen.y + 2,
        width,
        1,
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "┃ ",
                Style::default()
                    .fg(theme::WARNING)
                    .bg(theme::BACKGROUND_PANEL),
            ),
            Span::styled(
                toast.to_string(),
                Style::default().fg(theme::TEXT).bg(theme::BACKGROUND_PANEL),
            ),
            Span::styled(
                " ┃",
                Style::default()
                    .fg(theme::WARNING)
                    .bg(theme::BACKGROUND_PANEL),
            ),
        ])),
        area,
    );
}
