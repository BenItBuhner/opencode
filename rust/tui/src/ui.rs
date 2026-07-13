//! Faithful rendering of the OpenCode TUI (packages/tui), reproduced with
//! ratatui: the home route (block logo + centered prompt + tips + footer),
//! the session route (message list with `┃` gutters, per-tool icon rows,
//! thought headers, metadata footers), the prompt editor (left `┃` accent
//! border, agent row, connector, status row with the block spinner), and
//! centered DialogSelect panels with search and `●` markers.

use crate::state::{
    fuzzy, App, Autocomplete, Dialog, Geometry, HitTarget, Rectangle, COMMANDS as PALETTE,
};
use crate::theme;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use ratatui::Frame;
use serde_json::Value;

pub fn draw(frame: &mut Frame, app: &mut App) {
    app.geometry = Geometry::default();
    frame.render_widget(
        Block::default().style(Style::default().bg(theme::BACKGROUND)),
        frame.area(),
    );
    if app.is_home() {
        draw_home(frame, app);
    } else {
        draw_session(frame, app);
    }
    if app.autocomplete.is_some() {
        draw_autocomplete(frame, app);
    }
    match app.dialog {
        Dialog::None => {}
        Dialog::Help => draw_help(frame, app),
        Dialog::GoalDetails => draw_goal_details(frame, app),
        Dialog::GoalSummaries => draw_goal_summaries(frame, app),
        _ => draw_dialog_select(frame, app),
    }
}

fn rect_of(area: Rect) -> Rectangle {
    Rectangle {
        x: area.x,
        y: area.y,
        width: area.width,
        height: area.height,
    }
}

// ---------------------------------------------------------------------------
// Home route: centered logo, prompt, tip, footer
// ---------------------------------------------------------------------------

fn draw_home(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let prompt_width = (area.width * 7 / 10)
        .clamp(40, 75)
        .min(area.width.saturating_sub(2));
    let prompt = prompt_height(app);
    // Logo(4) + gap(1) + prompt block + tip row.
    let content_height = 4 + 1 + prompt + 2;
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
    let prompt_area = Rect::new(prompt_x, area.y + top + 5, prompt_width, prompt);
    draw_prompt(frame, app, prompt_area);

    // Tip row (feature-plugins/home/tips-view.tsx): "● Tip  <text>".
    let tip = TIPS[app.tip_index % TIPS.len()];
    let tip_area = Rect::new(
        prompt_x,
        prompt_area.y + prompt_area.height + 1,
        prompt_width,
        1,
    );
    let is_hovered = app.hover == Some(HitTarget::TipRow);
    let is_pressed = app.pressed == Some(HitTarget::TipRow);
    let bg = if is_pressed {
        theme::BACKGROUND_ELEMENT
    } else {
        theme::BACKGROUND
    };
    let bold = if is_hovered || is_pressed {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };
    let mut spans = vec![Span::styled(
        "● Tip ",
        Style::default()
            .fg(theme::WARNING)
            .bg(bg)
            .add_modifier(bold),
    )];
    spans.extend(tip_spans(tip, bg));
    frame.render_widget(Paragraph::new(Line::from(spans)), tip_area);
    app.geometry.tip_row = Some(rect_of(tip_area));

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

fn tip_spans(tip: &str, bg: Color) -> Vec<Span<'static>> {
    let mut spans = vec![];
    let mut rest = tip;
    while let Some(start) = rest.find('{') {
        if !rest[..start].is_empty() {
            spans.push(Span::styled(
                rest[..start].to_string(),
                Style::default().fg(theme::TEXT_MUTED).bg(bg),
            ));
        }
        let Some(end) = rest[start..].find('}') else {
            break;
        };
        spans.push(Span::styled(
            rest[start + 1..start + end].to_string(),
            Style::default().fg(theme::TEXT).bg(bg),
        ));
        rest = &rest[start + end + 1..];
    }
    if !rest.is_empty() {
        spans.push(Span::styled(
            rest.to_string(),
            Style::default().fg(theme::TEXT_MUTED).bg(bg),
        ));
    }
    spans
}

// ---------------------------------------------------------------------------
// Session route: transcript + prompt
// ---------------------------------------------------------------------------

fn draw_session(frame: &mut Frame, app: &mut App) {
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
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(prompt_height(app)),
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

/// Total prompt block height: paddingTop row, textarea rows, agent-row
/// paddingTop row, agent row, connector, status.
pub fn prompt_height(app: &App) -> u16 {
    editor_height(app) + 5
}

fn draw_prompt(frame: &mut Frame, app: &mut App, area: Rect) {
    let editor = editor_height(app);
    let accent = app.agent_color();
    let shell_mode = app.input.starts_with('!');
    let border_color = if app.leader.is_some() {
        theme::BORDER
    } else if shell_mode {
        theme::PRIMARY
    } else {
        theme::tint(theme::BORDER, accent, 0.7)
    };
    let element = Style::default().bg(theme::BACKGROUND_ELEMENT);
    let blank_row = |frame: &mut Frame, rect: Rect| {
        frame.render_widget(
            Paragraph::new(Span::styled("┃", Style::default().fg(border_color))).style(element),
            rect,
        );
    };

    app.geometry.prompt = Some(rect_of(area));

    // Prompt box paddingTop=1: an element-background row above the textarea.
    blank_row(frame, Rect::new(area.x, area.y, area.width, 1));

    // Textarea rows with left `┃` border and element background.
    let textarea_rect = Rect::new(area.x + 3, area.y + 1, area.width.saturating_sub(3), editor);
    app.geometry.textarea = Some(rect_of(textarea_rect));
    for row in 0..editor {
        let rect = Rect::new(area.x, area.y + 1 + row, area.width, 1);
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

    // Agent row paddingTop=1: blank element row between textarea and label.
    blank_row(frame, Rect::new(area.x, area.y + 1 + editor, area.width, 1));

    // Agent row: {Agent} · {model} {provider}
    let agent_row = Rect::new(area.x, area.y + 2 + editor, area.width, 1);
    let label = if shell_mode {
        "Shell".to_string()
    } else {
        titlecase(&app.agent)
    };
    let agent_hovered = matches!(app.hover, Some(HitTarget::AgentSpan))
        || matches!(app.pressed, Some(HitTarget::AgentSpan));
    let model_hovered = matches!(app.hover, Some(HitTarget::ModelSpan))
        || matches!(app.pressed, Some(HitTarget::ModelSpan));
    let agent_bg = if matches!(app.pressed, Some(HitTarget::AgentSpan)) {
        theme::BACKGROUND_PANEL
    } else {
        theme::BACKGROUND_ELEMENT
    };
    let model_bg = if matches!(app.pressed, Some(HitTarget::ModelSpan)) {
        theme::BACKGROUND_PANEL
    } else {
        theme::BACKGROUND_ELEMENT
    };
    let label_text = format!("  {label}");
    let model_text = format!(" · {} {}", app.model, app.provider);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("┃", Style::default().fg(border_color)),
            Span::styled(
                label_text.clone(),
                Style::default()
                    .fg(theme::tint(
                        theme::BACKGROUND_ELEMENT,
                        if shell_mode { theme::PRIMARY } else { accent },
                        if agent_hovered { 1.0 } else { 0.8 },
                    ))
                    .bg(agent_bg)
                    .add_modifier(if agent_hovered {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(
                model_text.clone(),
                Style::default()
                    .fg(if model_hovered {
                        theme::TEXT
                    } else {
                        theme::TEXT_MUTED
                    })
                    .bg(model_bg)
                    .add_modifier(if model_hovered {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
        ]))
        .style(element),
        agent_row,
    );
    let agent_width = label_text.chars().count() as u16;
    let model_width = model_text.chars().count() as u16;
    app.geometry.agent_span = Some(Rectangle {
        x: agent_row.x + 1,
        y: agent_row.y,
        width: agent_width,
        height: 1,
    });
    app.geometry.model_span = Some(Rectangle {
        x: agent_row.x + 1 + agent_width,
        y: agent_row.y,
        width: model_width,
        height: 1,
    });

    // Connector row: `╹` + `▀` bottom edge of the element background.
    let connector = Rect::new(area.x, area.y + editor + 3, area.width, 1);
    let mut edge = String::from("╹");
    edge.push_str(&"▀".repeat(area.width.saturating_sub(1) as usize));
    frame.render_widget(
        Paragraph::new(Span::styled(
            edge,
            Style::default().fg(theme::BACKGROUND_ELEMENT),
        )),
        connector,
    );
    let status = Rect::new(area.x, area.y + editor + 4, area.width, 1);
    draw_prompt_status(frame, app, status);

    // Terminal cursor inside the textarea (below the padding row).
    if app.dialog == Dialog::None && app.autocomplete.is_none() {
        let before: String = app.input.chars().take(app.cursor).collect();
        let row = before.matches('\n').count() as u16;
        let column = before.split('\n').next_back().unwrap_or("").chars().count() as u16;
        frame.set_cursor_position((
            area.x + 3 + column.min(area.width.saturating_sub(4)),
            area.y + 1 + row.min(editor.saturating_sub(1)),
        ));
    }
}

fn draw_prompt_status(frame: &mut Frame, app: &mut App, area: Rect) {
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

    let hint = if app.leader.is_some() {
        Line::from(Span::styled(
            "leader · q quit  n new  l sessions  a agents  m models  ? help",
            Style::default().fg(theme::WARNING),
        ))
    } else if let Some(goal) = app.display_goal() {
        // Fork goal chip: `goal {N}% ━━━────── {elapsed}` — click "goal" for
        // details, click the bar for summaries (ctrl+x g / ctrl+x s too).
        let progress = goal
            .get("progress")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .clamp(0, 100);
        let status = goal
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("active");
        let filled = (progress as usize * 12) / 100;
        let bar: String = "━".repeat(filled) + &"─".repeat(12 - filled);
        let elapsed = goal_elapsed(goal);
        let modifier = if matches!(app.hover, Some(HitTarget::GoalChip))
            || matches!(app.pressed, Some(HitTarget::GoalChip))
        {
            Modifier::BOLD
        } else {
            Modifier::empty()
        };
        let head = format!(
            "goal{} ",
            match status {
                "paused" => " (paused)",
                "completed" => " (completed)",
                _ => "",
            }
        );
        let percent = format!("{progress}% ");
        let elapsed_text = elapsed.as_deref().map(|item| format!(" {item}"));
        let mut chip_width = head.chars().count() + percent.chars().count() + bar.chars().count();
        if let Some(text) = &elapsed_text {
            chip_width += text.chars().count();
        }
        let mut spans = vec![
            Span::styled(
                head,
                Style::default().fg(theme::ACCENT).add_modifier(modifier),
            ),
            Span::styled(
                percent,
                Style::default().fg(theme::ACCENT).add_modifier(modifier),
            ),
            Span::styled(
                bar,
                Style::default().fg(theme::ACCENT).add_modifier(modifier),
            ),
        ];
        if let Some(text) = elapsed_text {
            spans.push(Span::styled(
                text,
                Style::default()
                    .fg(theme::TEXT_MUTED)
                    .add_modifier(modifier),
            ));
        }
        let chip_x = area
            .x
            .saturating_add(area.width.saturating_sub(chip_width as u16));
        app.geometry.goal_chip = Some(Rectangle {
            x: chip_x,
            y: area.y,
            width: chip_width as u16,
            height: 1,
        });
        Line::from(spans)
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
// Slash-command autocomplete popover
// ---------------------------------------------------------------------------

fn draw_autocomplete(frame: &mut Frame, app: &mut App) {
    let Some(auto) = app.autocomplete.as_ref() else {
        return;
    };
    let Some(prompt) = app.geometry.prompt else {
        return;
    };
    let screen = frame.area();
    let matches = auto.matches.len();
    if matches == 0 {
        return;
    }
    let width = 60u16.min(screen.width.saturating_sub(4));
    // Card height: 1 heading + `matches` rows + 2 preview.
    let preview_lines = 2u16;
    let rows = matches as u16;
    let height = (2 + rows + preview_lines).min(screen.height.saturating_sub(4));
    let y = prompt.y.saturating_sub(height);
    let x = prompt.x;
    let area = Rect::new(x, y, width, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().bg(theme::BACKGROUND_PANEL)),
        area,
    );
    let panel = Style::default().bg(theme::BACKGROUND_PANEL);
    // Header row: "Commands" left; "tab · enter" right hint.
    let header_row = Rect::new(area.x + 2, area.y, area.width.saturating_sub(4), 1);
    frame.render_widget(
        Paragraph::new(Span::styled(
            "Commands",
            Style::default()
                .fg(theme::TEXT)
                .bg(theme::BACKGROUND_PANEL)
                .add_modifier(Modifier::BOLD),
        ))
        .style(panel),
        header_row,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            "tab · enter",
            Style::default()
                .fg(theme::TEXT_MUTED)
                .bg(theme::BACKGROUND_PANEL),
        ))
        .alignment(Alignment::Right)
        .style(panel),
        header_row,
    );

    app.geometry.autocomplete = Some(rect_of(area));
    app.geometry.autocomplete_list_top = area.y + 1;
    app.geometry.autocomplete_rows = matches;

    // Rows: marker + `/name`, shortcut column right-aligned.
    for (row, position) in auto.matches.iter().enumerate() {
        let spec = &PALETTE[*position];
        let is_selected = row == auto.index;
        let hovered = matches!(app.hover, Some(HitTarget::Row(hovered_row)) if hovered_row == row);
        let bg = if is_selected {
            theme::PRIMARY
        } else if hovered {
            theme::BACKGROUND_ELEMENT
        } else {
            theme::BACKGROUND_PANEL
        };
        let fg = if is_selected {
            theme::SELECTED_FG
        } else {
            theme::TEXT
        };
        let marker = if is_selected { " ●" } else { "  " };
        let title_text = format!(" /{}", spec.name);
        let shortcut = spec.shortcut;
        let used = marker.chars().count() + title_text.chars().count() + shortcut.chars().count();
        let padding = (area.width as usize).saturating_sub(used + 4).max(2);
        let row_rect = Rect::new(area.x, area.y + 1 + row as u16, area.width, 1);
        let bold = if is_selected {
            Modifier::BOLD
        } else {
            Modifier::empty()
        };
        let row_spans = vec![
            Span::styled(
                marker.to_string(),
                Style::default().fg(fg).bg(bg).add_modifier(bold),
            ),
            Span::styled(
                title_text,
                Style::default().fg(fg).bg(bg).add_modifier(bold),
            ),
            Span::styled(" ".repeat(padding), Style::default().bg(bg)),
            Span::styled(
                format!("{shortcut}  "),
                Style::default()
                    .fg(if is_selected {
                        theme::SELECTED_FG
                    } else {
                        theme::TEXT_MUTED
                    })
                    .bg(bg),
            ),
        ];
        frame.render_widget(Paragraph::new(Line::from(row_spans)), row_rect);
    }

    // Preview footer: description for the currently-selected match.
    let selected_spec = PALETTE
        .get(auto.matches[auto.index.min(matches - 1)])
        .expect("index bounded above");
    let preview_rect = Rect::new(
        area.x + 2,
        area.y + 1 + rows,
        area.width.saturating_sub(4),
        preview_lines,
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                selected_spec.description.to_string(),
                Style::default()
                    .fg(theme::TEXT_MUTED)
                    .bg(theme::BACKGROUND_PANEL),
            )),
            Line::from(Span::styled(
                format!("{}    press enter to run", selected_spec.shortcut),
                Style::default()
                    .fg(theme::TEXT_MUTED)
                    .bg(theme::BACKGROUND_PANEL)
                    .add_modifier(Modifier::DIM),
            )),
        ])
        .wrap(Wrap { trim: true })
        .style(Style::default().bg(theme::BACKGROUND_PANEL)),
        preview_rect,
    );
}

/// Silences an "unused import" lint for the re-export used elsewhere.
#[allow(dead_code)]
fn _hold_autocomplete(_: &Autocomplete) {}

// ---------------------------------------------------------------------------
// DialogSelect: centered panel, search input, categories, ● markers
// ---------------------------------------------------------------------------

pub struct DialogOption {
    pub category: Option<String>,
    pub title: String,
    pub description: String,
    pub footer: String,
    /// The active value (current session/agent/model) gets the `●` gutter.
    pub current: bool,
}

pub fn dialog_options(app: &App) -> Vec<DialogOption> {
    let filter =
        |title: &str, description: &str| fuzzy(&format!("{title} {description}"), &app.search);
    match app.dialog {
        Dialog::Commands => PALETTE
            .iter()
            .filter(|spec| filter(spec.title, spec.description))
            .map(|spec| DialogOption {
                category: None,
                title: spec.title.to_string(),
                description: spec.description.to_string(),
                footer: spec.shortcut.to_string(),
                current: false,
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
                current: session.get("id").and_then(Value::as_str) == app.session_id.as_deref(),
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
            .map(|agent| {
                let name = agent
                    .get("id")
                    .or_else(|| agent.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("agent");
                DialogOption {
                    category: None,
                    title: titlecase(name),
                    description: agent
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .chars()
                        .take(60)
                        .collect(),
                    footer: String::new(),
                    current: name == app.agent,
                }
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
                current: *provider == app.provider && *model == app.model,
            })
            .collect(),
        _ => vec![],
    }
}

/// Dialog panel geometry, shared by the renderer and the mouse handler.
pub fn dialog_rect(screen: Rect, dialog: Dialog) -> Rect {
    let width = match dialog {
        Dialog::Sessions | Dialog::Models => 88u16,
        _ => 60u16,
    }
    .min(screen.width.saturating_sub(2));
    let list_height = (screen.height / 2).saturating_sub(6).max(4);
    let height = (list_height + 4).min(screen.height.saturating_sub(4));
    Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    )
}

/// Maps a clicked terminal row inside the dialog to an option index,
/// accounting for category header lines.
pub fn dialog_row_at(app: &App, area: Rect, row: u16) -> Option<usize> {
    let list_top = area.y + 4;
    if row < list_top {
        return None;
    }
    dialog_row_at_offset(app, (row - list_top) as usize)
}

/// Maps a zero-based rendered row offset (into the dialog list body) back to
/// the underlying option index. Shared with the mouse hit-test.
pub fn dialog_row_at_offset(app: &App, offset: usize) -> Option<usize> {
    let options = dialog_options(app);
    let mut line = 0usize;
    let mut last_category: Option<String> = None;
    for (index, option) in options.iter().enumerate() {
        if option.category != last_category {
            if option.category.is_some() {
                line += 1;
            }
            last_category = option.category.clone();
        }
        if line == offset {
            return Some(index);
        }
        line += 1;
    }
    None
}

fn dialog_title(dialog: Dialog) -> &'static str {
    match dialog {
        Dialog::Commands => "Commands",
        Dialog::Sessions => "Switch session",
        Dialog::Agents => "Select agent",
        Dialog::Models => "Select model",
        _ => "",
    }
}

fn draw_dialog_select(frame: &mut Frame, app: &mut App) {
    let screen = frame.area();
    let options = dialog_options(app);
    let area = dialog_rect(screen, app.dialog);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().bg(theme::BACKGROUND_PANEL)),
        area,
    );
    app.geometry.dialog_area = Some(rect_of(area));
    app.geometry.dialog_list_top = area.y + 4;

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
        let hovered =
            matches!(app.hover, Some(HitTarget::Row(hovered)) if hovered == index) && !active;
        let (fg, muted, bg) = if active {
            (theme::SELECTED_FG, theme::SELECTED_FG, theme::PRIMARY)
        } else if hovered {
            (theme::TEXT, theme::TEXT_MUTED, theme::BACKGROUND_ELEMENT)
        } else {
            (theme::TEXT, theme::TEXT_MUTED, theme::BACKGROUND_PANEL)
        };
        // `●` marks the current value (active session/agent/model), like the
        // DialogSelect gutter; the highlight bar tracks the cursor.
        let marker = if option.current { " ● " } else { "   " };
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
        ("enter", "send prompt / execute autocomplete"),
        ("alt+enter", "insert newline"),
        ("esc", "interrupt session / close dialog"),
        ("pageup / pagedown", "scroll messages"),
        ("ctrl+p", "command palette"),
        ("ctrl+a / ctrl+e", "beginning / end of line"),
        ("ctrl+b / ctrl+f", "move left / right"),
        ("ctrl+u / ctrl+k", "delete to start / end"),
        ("ctrl+w", "delete word back"),
        ("alt+b / alt+f", "word left / right"),
        ("ctrl+d / delete", "delete forward"),
        ("up / down", "prompt history / scroll"),
        ("tab", "autocomplete / cycle agent"),
        ("ctrl+x n", "new session (home)"),
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

/// Fork "Goal Details" alert: Status / Running duration / goal text.
fn draw_goal_details(frame: &mut Frame, app: &App) {
    let Some(goal) = app.display_goal() else {
        return;
    };
    let screen = frame.area();
    let text = goal.get("text").and_then(Value::as_str).unwrap_or_default();
    let status = goal
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("active");
    let elapsed = goal_elapsed(goal);
    let width = 60u16.min(screen.width.saturating_sub(2));
    let height = 8u16.min(screen.height.saturating_sub(4));
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
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                "Goal Details",
                Style::default()
                    .fg(theme::TEXT)
                    .bg(theme::BACKGROUND_PANEL)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:>width$}", "esc", width = width as usize - 14),
                Style::default()
                    .fg(theme::TEXT_MUTED)
                    .bg(theme::BACKGROUND_PANEL),
            ),
        ]),
        Line::default(),
        Line::from(Span::styled(
            format!("Status: {status}"),
            Style::default()
                .fg(theme::TEXT_MUTED)
                .bg(theme::BACKGROUND_PANEL),
        )),
    ];
    if let Some(elapsed) = elapsed {
        lines.push(Line::from(Span::styled(
            format!("Running: {elapsed}"),
            Style::default()
                .fg(theme::TEXT_MUTED)
                .bg(theme::BACKGROUND_PANEL),
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(theme::TEXT).bg(theme::BACKGROUND_PANEL),
    )));
    frame.render_widget(
        Paragraph::new(lines)
            .style(panel)
            .wrap(Wrap { trim: false }),
        Rect::new(
            area.x + 2,
            area.y + 1,
            area.width.saturating_sub(4),
            area.height.saturating_sub(2),
        ),
    );
}

/// Fork DialogGoalSummaries: per-summary cards with `[###－]` progress bars
/// and parsed `##` sections rendered as bullets, newest first.
fn draw_goal_summaries(frame: &mut Frame, app: &App) {
    let Some(goal) = app.display_goal() else {
        return;
    };
    let screen = frame.area();
    let width = 88u16.min(screen.width.saturating_sub(2));
    let height = (screen.height * 3 / 4)
        .max(10)
        .min(screen.height.saturating_sub(4));
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
    let bar = |progress: i64| {
        let filled = (progress.clamp(0, 100) as usize).div_ceil(10).min(10);
        format!("[{}{}]", "#".repeat(filled), "-".repeat(10 - filled))
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "Goal Summaries",
            Style::default()
                .fg(theme::TEXT)
                .bg(theme::BACKGROUND_PANEL)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:>width$}", "esc", width = width as usize - 16),
            Style::default()
                .fg(theme::TEXT_MUTED)
                .bg(theme::BACKGROUND_PANEL),
        ),
    ])];
    lines.push(Line::from(Span::styled(
        goal.get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        Style::default()
            .fg(theme::TEXT_MUTED)
            .bg(theme::BACKGROUND_PANEL),
    )));
    let summaries = goal
        .get("summaries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if summaries.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "No state summaries recorded yet.",
            Style::default()
                .fg(theme::TEXT_MUTED)
                .bg(theme::BACKGROUND_PANEL),
        )));
    }
    for (index, summary) in summaries.iter().rev().enumerate() {
        let latest = index == 0;
        let progress = summary.get("progress").and_then(Value::as_i64).unwrap_or(0);
        let title = if latest {
            "Latest State".to_string()
        } else {
            summary
                .get("headline")
                .and_then(Value::as_str)
                .unwrap_or("Previous State")
                .to_string()
        };
        lines.push(Line::default());
        lines.push(Line::from(vec![
            Span::styled(
                title,
                Style::default()
                    .fg(if latest { theme::ACCENT } else { theme::TEXT })
                    .bg(theme::BACKGROUND_PANEL)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {progress}% {}", bar(progress)),
                Style::default()
                    .fg(theme::TEXT_MUTED)
                    .bg(theme::BACKGROUND_PANEL),
            ),
        ]));
        for raw in summary
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .lines()
        {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(section) = line.strip_prefix("## ") {
                lines.push(Line::from(Span::styled(
                    section.to_string(),
                    Style::default().fg(theme::TEXT).bg(theme::BACKGROUND_PANEL),
                )));
                continue;
            }
            if let Some(bullet) = line.strip_prefix("- ") {
                lines.push(Line::from(Span::styled(
                    format!("  • {bullet}"),
                    Style::default()
                        .fg(theme::TEXT_MUTED)
                        .bg(theme::BACKGROUND_PANEL),
                )));
            }
        }
    }
    let offset = (lines.len() as u16)
        .saturating_sub(area.height.saturating_sub(2))
        .min(app.scroll);
    frame.render_widget(
        Paragraph::new(lines)
            .style(panel)
            .wrap(Wrap { trim: false })
            .scroll((offset, 0)),
        Rect::new(
            area.x + 2,
            area.y + 1,
            area.width.saturating_sub(4),
            area.height.saturating_sub(2),
        ),
    );
}

/// Elapsed label for active goals (fork tracks running duration).
pub fn goal_elapsed(goal: &Value) -> Option<String> {
    if goal.get("status").and_then(Value::as_str) != Some("active") {
        return None;
    }
    let created = goal.get("created").and_then(Value::as_i64)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as i64;
    let seconds = ((now - created) / 1000).max(0);
    Some(match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
    })
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
