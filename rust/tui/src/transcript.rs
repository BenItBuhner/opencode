//! Prewrapped, width-aware transcript renderer.
//!
//! Every visible message is exploded into a flat list of `Line`s already
//! wrapped to the transcript's inner width, with the appropriate gutter,
//! two-column body padding, panel background, and right-edge fill re-emitted
//! on every visual row. Scroll math operates on the total visual row count
//! so shift+PageUp behaves the way it does upstream.
//!
//! The tool renderers here match the OpenCode TS TUI's
//! `packages/tui/src/routes/session/index.tsx` renderers (Shell/Task/Edit/
//! Read/Write/Glob/Grep/WebFetch/WebSearch/ApplyPatch/Question/TodoWrite/
//! Skill/Execute/Generic) as closely as ratatui allows. Inline tools use
//! the icon + title layout and, for tool subagents, the multi-line
//! "background label", "child status", "toolcount / duration" body. Block
//! tools use the `┃` border + `panel` background and repeat the border and
//! background on every wrapped visual row so long titles do not lose the
//! block edge.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::state::App;
use crate::theme;
use crate::wrap::{columns, RowChrome};

/// Emit all rows for the transcript into `rows`. Every element in `rows`
/// is already width `<= inner_width`, ready to be pushed into a `Paragraph`
/// with no wrap. The caller is responsible for the scroll math.
pub fn render(app: &App, inner_width: u16, rows: &mut Vec<Line<'static>>) {
    rows.push(blank_row(inner_width));
    let last_assistant = app
        .messages
        .iter()
        .rposition(|message| message.get("type").and_then(Value::as_str) == Some("assistant"));
    for (index, message) in app.messages.iter().enumerate() {
        push_message(
            app,
            message,
            inner_width,
            rows,
            index == 0,
            Some(index) == last_assistant,
        );
    }
}

fn blank_row(width: u16) -> Line<'static> {
    Line::from(vec![Span::styled(
        " ".repeat(width as usize),
        Style::default(),
    )])
}

fn push_message(
    app: &App,
    message: &Value,
    width: u16,
    out: &mut Vec<Line<'static>>,
    first: bool,
    is_last_assistant: bool,
) {
    match message.get("type").and_then(Value::as_str).unwrap_or("") {
        "user" => push_user(app, message, width, out, first),
        "assistant" => push_assistant(app, message, width, out, is_last_assistant),
        "agent-switched" => {
            let agent = message.get("agent").and_then(Value::as_str).unwrap_or("?");
            out.push(blank_row(width));
            push_plain(
                &format!("   ▣ Switched to {} agent", titlecase(agent)),
                Style::default().fg(theme::agent_color(agent, app.agent_index)),
                width,
                out,
            );
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
            out.push(blank_row(width));
            push_plain(
                &format!("   ▣ Switched to {model}"),
                Style::default().fg(theme::TEXT_MUTED),
                width,
                out,
            );
        }
        _ => {}
    }
}

fn push_user(app: &App, message: &Value, width: u16, out: &mut Vec<Line<'static>>, first: bool) {
    let text = message
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // Background task completion is model-facing synthetic context. The
    // upstream TUI filters synthetic user parts, so never dump its XML into
    // the visible transcript.
    if text.trim_start().starts_with("<task ") {
        return;
    }
    if !first {
        out.push(blank_row(width));
    }
    let gutter = Style::default().fg(app.agent_color()).bg(theme::BACKGROUND);
    let body = Style::default().fg(theme::TEXT).bg(theme::BACKGROUND_PANEL);
    let chrome = RowChrome {
        prefix: vec![Span::styled("┃", gutter), Span::styled("  ", body)],
        fill_style: body,
        body_width: width.saturating_sub(3) as usize,
    };
    // Blank pad row above content (background continues under gutter).
    chrome.emit(&[(String::new(), body)], out);
    for hard_line in text.split('\n') {
        chrome.emit(&[(hard_line.to_string(), body)], out);
    }
    chrome.emit(&[(String::new(), body)], out);
}

fn push_assistant(
    app: &App,
    message: &Value,
    width: u16,
    out: &mut Vec<Line<'static>>,
    is_last_assistant: bool,
) {
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
                out.push(blank_row(width));
                let title: String = text
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(80)
                    .collect();
                let duration = part_duration(&part);
                push_indented_paragraph(
                    &format!("Thought: {title}{duration}"),
                    Style::default().fg(theme::thinking()),
                    width,
                    out,
                );
            }
            "text" => {
                let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
                if text.is_empty() {
                    continue;
                }
                out.push(blank_row(width));
                for hard_line in text.split('\n') {
                    push_indented_paragraph(
                        hard_line,
                        Style::default().fg(theme::TEXT),
                        width,
                        out,
                    );
                }
            }
            "tool" => push_tool(app, &part, width, out),
            _ => {}
        }
    }
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
        out.push(blank_row(width));
        let chrome = RowChrome {
            prefix: vec![Span::raw("   ")],
            fill_style: Style::default(),
            body_width: width.saturating_sub(3) as usize,
        };
        chrome.emit(
            &[
                (
                    format!("▣ {}", titlecase(agent)),
                    Style::default().fg(theme::agent_color(agent, app.agent_index)),
                ),
                (
                    format!(" · {model}{duration}"),
                    Style::default().fg(theme::TEXT_MUTED),
                ),
            ],
            out,
        );
    }
}

/// Push a paragraph body indented by 3 columns (matching the TS TUI's
/// `paddingLeft={3}` on assistant text and inline tools). Wraps to the
/// remaining width and keeps the leading pad on continuation rows.
fn push_indented_paragraph(text: &str, style: Style, width: u16, out: &mut Vec<Line<'static>>) {
    let chrome = RowChrome {
        prefix: vec![Span::raw("   ")],
        fill_style: Style::default(),
        body_width: width.saturating_sub(3) as usize,
    };
    chrome.emit(&[(text.to_string(), style)], out);
}

fn push_plain(text: &str, style: Style, width: u16, out: &mut Vec<Line<'static>>) {
    let chrome = RowChrome {
        prefix: vec![],
        fill_style: Style::default(),
        body_width: width as usize,
    };
    chrome.emit(&[(text.to_string(), style)], out);
}

// ---------------------------------------------------------------------------
// Tool renderers (mirror packages/tui/src/routes/session/index.tsx)
// ---------------------------------------------------------------------------

fn push_tool(app: &App, part: &Value, width: u16, out: &mut Vec<Line<'static>>) {
    let name = part.get("name").and_then(Value::as_str).unwrap_or("tool");
    let state = part.get("state").cloned().unwrap_or(Value::Null);
    let status = state
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    let input = state.get("input").cloned().unwrap_or(Value::Null);
    let structured = state.get("structured").cloned().unwrap_or(Value::Null);
    let metadata = state
        .get("metadata")
        .or_else(|| structured.get("metadata"))
        .cloned()
        .unwrap_or(Value::Null);
    let output = state
        .get("output")
        .or_else(|| structured.get("output"))
        .and_then(Value::as_str)
        .unwrap_or("");

    match tool_display(name) {
        "bash" => push_bash(&input, &metadata, status, width, out),
        "edit" => push_edit(&input, &metadata, status, width, out),
        "write" => push_write(&input, &metadata, status, width, out),
        "read" => push_read(&input, &metadata, status, width, out),
        "glob" => push_glob(&input, &metadata, status, width, out),
        "grep" => push_grep(&input, &metadata, status, width, out),
        "webfetch" => push_webfetch(&input, status, width, out),
        "websearch" => push_websearch(&input, &metadata, status, width, out),
        "task" => push_task(app, &input, &metadata, part, status, width, out),
        "apply_patch" => push_apply_patch(&metadata, status, width, out),
        "question" => push_question(&input, &metadata, status, width, out),
        "todowrite" => push_todowrite(&input, status, width, out),
        "skill" => push_skill(&input, status, width, out),
        "execute" => push_execute(&metadata, output, status, width, out),
        _ => push_generic(name, &input, output, status, width, out),
    }
    if status == "error" {
        let error = state
            .get("error")
            .and_then(Value::as_str)
            .or_else(|| {
                state
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("failed");
        let chrome = RowChrome {
            prefix: vec![Span::raw("     ")],
            fill_style: Style::default(),
            body_width: width.saturating_sub(5) as usize,
        };
        chrome.emit(
            &[(error.to_string(), Style::default().fg(theme::ERROR))],
            out,
        );
    }
}

fn tool_display(name: &str) -> &'static str {
    match name {
        "bash" => "bash",
        "glob" => "glob",
        "read" => "read",
        "grep" => "grep",
        "webfetch" => "webfetch",
        "websearch" => "websearch",
        "write" => "write",
        "edit" => "edit",
        "task" => "task",
        "apply_patch" => "apply_patch",
        "todowrite" => "todowrite",
        "question" => "question",
        "skill" => "skill",
        "execute" => "execute",
        _ => "generic",
    }
}

fn text_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn number_field(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

/// Inline tool row rendering: `<3-col pad><icon>  <title>` wrapped to the
/// available width with continuation rows re-indented to line up under the
/// title. The upstream TS TUI renders this as a `paddingLeft={3}` box with
/// the icon inside a fixed-width span.
fn push_inline(
    icon: &str,
    icon_style: Style,
    title_segments: Vec<(String, Style)>,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let icon_width = columns(icon).max(1);
    let prefix_width = 3 + icon_width + 1;
    if prefix_width as u16 >= width {
        push_plain(icon, icon_style, width, out);
        for (text, style) in title_segments {
            push_plain(&text, style, width, out);
        }
        return;
    }
    let chrome = RowChrome {
        prefix: vec![
            Span::raw("   "),
            Span::styled(icon.to_string(), icon_style),
            Span::raw(" "),
        ],
        fill_style: Style::default(),
        body_width: width as usize - prefix_width,
    };
    chrome.emit(&title_segments, out);
}

/// A pending inline tool renders `~ <pending>` in place of the icon/title
/// (mirrors the TS `<Show fallback={<text>~ pending</text>}>` branch).
fn push_inline_pending(pending: &str, style: Style, width: u16, out: &mut Vec<Line<'static>>) {
    push_indented_paragraph(&format!("~ {pending}"), style, width, out);
}

/// A block tool renders a `┃` border with a two-column padded body on a
/// panel background. Every visual row re-emits the border and background so
/// long titles / wrapped bodies keep the block edge intact.
struct BlockRenderer {
    body_width: usize,
}

impl BlockRenderer {
    fn new(width: u16) -> Self {
        BlockRenderer {
            body_width: width.saturating_sub(3) as usize,
        }
    }

    fn chrome(&self, panel: Style) -> RowChrome {
        RowChrome {
            prefix: vec![
                Span::styled(
                    "┃",
                    Style::default().fg(theme::BORDER).bg(theme::BACKGROUND),
                ),
                Span::styled("  ", panel),
            ],
            fill_style: panel,
            body_width: self.body_width,
        }
    }

    fn push_blank(&self, out: &mut Vec<Line<'static>>, panel: Style) {
        self.chrome(panel).emit(&[(String::new(), panel)], out);
    }

    fn open(&self, out: &mut Vec<Line<'static>>) {
        out.push(blank_row(self.body_width as u16 + 3));
        self.push_blank(out, Style::default().bg(theme::BACKGROUND_PANEL));
    }

    fn close(&self, out: &mut Vec<Line<'static>>) {
        self.push_blank(out, Style::default().bg(theme::BACKGROUND_PANEL));
    }

    fn push_title(&self, title: &str, out: &mut Vec<Line<'static>>) {
        let panel = Style::default()
            .fg(theme::TEXT_MUTED)
            .bg(theme::BACKGROUND_PANEL);
        self.chrome(panel).emit(&[(title.to_string(), panel)], out);
    }

    fn push_body(&self, text: &str, style: Style, out: &mut Vec<Line<'static>>) {
        let panel = style.bg(theme::BACKGROUND_PANEL);
        self.chrome(panel).emit(&[(text.to_string(), panel)], out);
    }
}

// ---- individual tools -----------------------------------------------------

fn push_bash(
    input: &Value,
    metadata: &Value,
    status: &str,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let command = text_field(input, "command");
    let bash_output = metadata
        .get("output")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(output) = bash_output {
        let block = BlockRenderer::new(width);
        block.open(out);
        let workdir = text_field(input, "workdir");
        let title = if !workdir.is_empty() && workdir != "." {
            format!("# Running in {workdir}")
        } else {
            String::new()
        };
        if !title.is_empty() {
            block.push_title(&title, out);
            block.push_blank(out, Style::default().bg(theme::BACKGROUND_PANEL));
        }
        block.push_body(
            &format!("$ {command}"),
            Style::default().fg(theme::TEXT),
            out,
        );
        if !output.is_empty() {
            block.push_blank(out, Style::default().bg(theme::BACKGROUND_PANEL));
            for hard_line in output.split('\n') {
                block.push_body(hard_line, Style::default().fg(theme::TEXT), out);
            }
        }
        block.close(out);
        return;
    }
    let icon_style = tool_icon_style(status);
    let title_style = tool_title_style(status);
    if command.is_empty() && status == "pending" {
        push_inline_pending("Writing command...", title_style, width, out);
        return;
    }
    push_inline("$", icon_style, vec![(command, title_style)], width, out);
}

fn push_edit(
    input: &Value,
    metadata: &Value,
    status: &str,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let path = text_field(input, "filePath");
    let diff = metadata.get("diff").and_then(Value::as_str);
    if let Some(diff) = diff {
        let block = BlockRenderer::new(width);
        block.open(out);
        block.push_title(&format!("← Edit {path}"), out);
        block.push_blank(out, Style::default().bg(theme::BACKGROUND_PANEL));
        for hard_line in diff.split('\n') {
            let fg = if let Some(stripped) = hard_line.strip_prefix('+') {
                let _ = stripped;
                Style::default().fg(theme::SUCCESS)
            } else if hard_line.starts_with('-') {
                Style::default().fg(theme::ERROR)
            } else {
                Style::default().fg(theme::TEXT_MUTED)
            };
            block.push_body(hard_line, fg, out);
        }
        block.close(out);
        return;
    }
    let icon_style = tool_icon_style(status);
    let title_style = tool_title_style(status);
    if path.is_empty() && status == "pending" {
        push_inline_pending("Preparing edit...", title_style, width, out);
        return;
    }
    let mut title = format!("Edit {path}");
    let extras = extras_of(input, &["filePath", "oldString", "newString"]);
    if !extras.is_empty() {
        title.push(' ');
        title.push_str(&extras);
    }
    push_inline("←", icon_style, vec![(title, title_style)], width, out);
}

fn push_write(
    input: &Value,
    metadata: &Value,
    status: &str,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let path = text_field(input, "filePath");
    let has_diagnostics = metadata.get("diagnostics").is_some();
    if has_diagnostics {
        let block = BlockRenderer::new(width);
        block.open(out);
        block.push_title(&format!("# Wrote {path}"), out);
        let content = text_field(input, "content");
        if !content.is_empty() {
            block.push_blank(out, Style::default().bg(theme::BACKGROUND_PANEL));
            for (idx, hard_line) in content.split('\n').enumerate() {
                let numbered = format!("{:>3} {hard_line}", idx + 1);
                block.push_body(&numbered, Style::default().fg(theme::TEXT), out);
            }
        }
        block.close(out);
        return;
    }
    let icon_style = tool_icon_style(status);
    let title_style = tool_title_style(status);
    if path.is_empty() && status == "pending" {
        push_inline_pending("Preparing write...", title_style, width, out);
        return;
    }
    push_inline(
        "←",
        icon_style,
        vec![(format!("Write {path}"), title_style)],
        width,
        out,
    );
}

fn push_read(
    input: &Value,
    metadata: &Value,
    status: &str,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let path = text_field(input, "filePath");
    let icon_style = tool_icon_style(status);
    let title_style = tool_title_style(status);
    if path.is_empty() && status == "pending" {
        push_inline_pending("Reading file...", title_style, width, out);
        return;
    }
    let mut title = format!("Read {path}");
    let extras = extras_of(input, &["filePath"]);
    if !extras.is_empty() {
        title.push(' ');
        title.push_str(&extras);
    }
    push_inline("→", icon_style, vec![(title, title_style)], width, out);
    if let Some(loaded) = metadata.get("loaded").and_then(Value::as_array) {
        for entry in loaded {
            if let Some(name) = entry.as_str() {
                let text = format!("↳ Loaded {name}");
                push_indented_continuation(
                    &text,
                    Style::default().fg(theme::TEXT_MUTED),
                    width,
                    out,
                );
            }
        }
    }
}

fn push_indented_continuation(text: &str, style: Style, width: u16, out: &mut Vec<Line<'static>>) {
    let chrome = RowChrome {
        prefix: vec![Span::raw("      ")],
        fill_style: Style::default(),
        body_width: width.saturating_sub(6) as usize,
    };
    chrome.emit(&[(text.to_string(), style)], out);
}

fn push_glob(
    input: &Value,
    metadata: &Value,
    status: &str,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let pattern = text_field(input, "pattern");
    let path = text_field(input, "path");
    let count = number_field(metadata, "count")
        .or_else(|| number_field(metadata, "matches"))
        .unwrap_or(0);
    let icon_style = tool_icon_style(status);
    let title_style = tool_title_style(status);
    if pattern.is_empty() && status == "pending" {
        push_inline_pending("Finding files...", title_style, width, out);
        return;
    }
    let mut title = format!("Glob \"{pattern}\"");
    if !path.is_empty() {
        title.push_str(&format!(" in {path}"));
    }
    if count > 0 {
        title.push_str(&format!(
            " ({count} match{})",
            if count == 1 { "" } else { "es" }
        ));
    }
    push_inline("✱", icon_style, vec![(title, title_style)], width, out);
}

fn push_grep(
    input: &Value,
    metadata: &Value,
    status: &str,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let pattern = text_field(input, "pattern");
    let path = text_field(input, "path");
    let count = number_field(metadata, "matches")
        .or_else(|| number_field(metadata, "count"))
        .unwrap_or(0);
    let icon_style = tool_icon_style(status);
    let title_style = tool_title_style(status);
    if pattern.is_empty() && status == "pending" {
        push_inline_pending("Searching content...", title_style, width, out);
        return;
    }
    let mut title = format!("Grep \"{pattern}\"");
    if !path.is_empty() {
        title.push_str(&format!(" in {path}"));
    }
    if count > 0 {
        title.push_str(&format!(
            " ({count} match{})",
            if count == 1 { "" } else { "es" }
        ));
    }
    push_inline("✱", icon_style, vec![(title, title_style)], width, out);
}

fn push_webfetch(input: &Value, status: &str, width: u16, out: &mut Vec<Line<'static>>) {
    let url = text_field(input, "url");
    let icon_style = tool_icon_style(status);
    let title_style = tool_title_style(status);
    if url.is_empty() && status == "pending" {
        push_inline_pending("Fetching from the web...", title_style, width, out);
        return;
    }
    push_inline(
        "%",
        icon_style,
        vec![(format!("WebFetch {url}"), title_style)],
        width,
        out,
    );
}

fn push_websearch(
    input: &Value,
    metadata: &Value,
    status: &str,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let query = text_field(input, "query");
    let provider = metadata
        .get("provider")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "WebSearch".into());
    let results = number_field(metadata, "numResults").unwrap_or(0);
    let icon_style = tool_icon_style(status);
    let title_style = tool_title_style(status);
    if query.is_empty() && status == "pending" {
        push_inline_pending("Searching web...", title_style, width, out);
        return;
    }
    let mut title = format!("{provider} \"{query}\"");
    if results > 0 {
        title.push_str(&format!(" ({results} results)"));
    }
    push_inline("◈", icon_style, vec![(title, title_style)], width, out);
}

/// Task tool = subagent delegation. The TS TUI stacks a title line, an
/// (optional) background label, and a child status/toolcount/duration
/// footer. We reproduce the three-line body exactly (via `\n` in the body
/// segment so the wrapper can re-indent continuation lines correctly).
fn push_task(
    app: &App,
    input: &Value,
    metadata: &Value,
    part: &Value,
    status: &str,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let description = text_field(input, "description");
    let subagent = text_field(input, "subagent_type");
    let subagent = if subagent.is_empty() {
        "General".to_string()
    } else {
        titlecase(&subagent)
    };
    let background = metadata
        .get("background")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let child_id = metadata.get("sessionId").and_then(Value::as_str);
    let child_status = child_id
        .and_then(|id| app.session_status.get(id))
        .and_then(|status| status.get("type"))
        .and_then(Value::as_str);
    let running =
        status == "running" || (background && child_status.is_some_and(|value| value != "idle"));
    let title_style = tool_title_style(status);

    if description.is_empty() && status == "pending" {
        push_inline_pending("Delegating...", title_style, width, out);
        return;
    }
    let icon = if running {
        "│"
    } else if status == "completed" {
        "✓"
    } else if status == "error" {
        "✗"
    } else {
        "│"
    };
    let icon_style = if status == "error" {
        Style::default().fg(theme::ERROR)
    } else if !running && status == "completed" {
        Style::default().fg(theme::SUCCESS)
    } else {
        Style::default().fg(theme::WARNING)
    };
    let mut body = format!(
        "{subagent} Task{} — {description}",
        if background { " (background)" } else { "" }
    );
    let toolcount = metadata
        .get("toolCount")
        .and_then(Value::as_i64)
        .or_else(|| {
            metadata
                .get("toolcalls")
                .and_then(Value::as_array)
                .map(|value| value.len() as i64)
        })
        .unwrap_or(0);
    let child_title = metadata
        .get("currentToolTitle")
        .and_then(Value::as_str)
        .map(str::to_string);
    let child_tool = metadata
        .get("currentTool")
        .and_then(Value::as_str)
        .map(str::to_string);
    if running {
        if let (Some(tool), Some(title)) = (child_tool, child_title) {
            body.push('\n');
            body.push_str(&format!("↳ {} {title}", titlecase(&tool)));
        } else if toolcount > 0 {
            body.push('\n');
            body.push_str(&format!(
                "↳ {toolcount} toolcall{}",
                if toolcount == 1 { "" } else { "s" }
            ));
        }
    }
    if !running && status == "completed" {
        let duration = part_duration(part);
        body.push('\n');
        if toolcount == 0 {
            body.push_str(&format!("↳ Completed{duration}"));
        } else {
            body.push_str(&format!(
                "↳ {toolcount} toolcall{}{duration}",
                if toolcount == 1 { "" } else { "s" }
            ));
        }
    }
    let segments = vec![(body, title_style)];
    push_inline(icon, icon_style, segments, width, out);
}

fn push_apply_patch(metadata: &Value, status: &str, width: u16, out: &mut Vec<Line<'static>>) {
    let files = metadata
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if files.is_empty() {
        let title_style = tool_title_style(status);
        if status == "pending" || status == "running" {
            push_inline_pending("Preparing patch...", title_style, width, out);
            return;
        }
        push_inline(
            "%",
            Style::default().fg(theme::ERROR),
            vec![(
                "Patch failed".to_string(),
                Style::default().fg(theme::ERROR),
            )],
            width,
            out,
        );
        return;
    }
    for file in files {
        let type_ = text_field(&file, "type");
        let relative = text_field(&file, "relativePath");
        let file_path = text_field(&file, "filePath");
        let patch = text_field(&file, "patch");
        let deletions = number_field(&file, "deletions").unwrap_or(0);
        let title = match type_.as_str() {
            "delete" => format!("# Deleted {relative}"),
            "add" => format!("# Created {relative}"),
            "move" => format!("# Moved {file_path} → {relative}"),
            _ => format!("← Patched {relative}"),
        };
        let block = BlockRenderer::new(width);
        block.open(out);
        block.push_title(&title, out);
        if type_ == "delete" {
            block.push_blank(out, Style::default().bg(theme::BACKGROUND_PANEL));
            block.push_body(
                &format!("-{deletions} line{}", if deletions == 1 { "" } else { "s" }),
                Style::default().fg(theme::ERROR),
                out,
            );
        } else if !patch.is_empty() {
            block.push_blank(out, Style::default().bg(theme::BACKGROUND_PANEL));
            for hard_line in patch.split('\n') {
                let style = if hard_line.starts_with('+') {
                    Style::default().fg(theme::SUCCESS)
                } else if hard_line.starts_with('-') {
                    Style::default().fg(theme::ERROR)
                } else {
                    Style::default().fg(theme::TEXT_MUTED)
                };
                block.push_body(hard_line, style, out);
            }
        }
        block.close(out);
    }
}

fn push_question(
    input: &Value,
    metadata: &Value,
    status: &str,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let questions = input
        .get("questions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let answers = metadata.get("answers").and_then(Value::as_array).cloned();
    let title_style = tool_title_style(status);
    let icon_style = tool_icon_style(status);
    if answers.is_none() {
        let count = questions.len();
        if count == 0 && status == "pending" {
            push_inline_pending("Asking questions...", title_style, width, out);
            return;
        }
        push_inline(
            "→",
            icon_style,
            vec![(
                format!(
                    "Asked {count} question{}",
                    if count == 1 { "" } else { "s" }
                ),
                title_style,
            )],
            width,
            out,
        );
        return;
    }
    let answers = answers.unwrap();
    let block = BlockRenderer::new(width);
    block.open(out);
    block.push_title("# Questions", out);
    for (index, question) in questions.iter().enumerate() {
        let text = question
            .get("question")
            .and_then(Value::as_str)
            .unwrap_or_default();
        block.push_blank(out, Style::default().bg(theme::BACKGROUND_PANEL));
        block.push_body(text, Style::default().fg(theme::TEXT_MUTED), out);
        let answer_text = match answers.get(index) {
            Some(entry) => entry
                .as_array()
                .map(|array| {
                    array
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "(no answer)".to_string()),
            None => "(no answer)".to_string(),
        };
        block.push_body(&answer_text, Style::default().fg(theme::TEXT), out);
    }
    block.close(out);
}

fn push_todowrite(input: &Value, _status: &str, width: u16, out: &mut Vec<Line<'static>>) {
    let todos = input
        .get("todos")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if todos.is_empty() {
        let title_style = Style::default().fg(theme::TEXT);
        push_inline_pending("Updating todos...", title_style, width, out);
        return;
    }
    let block = BlockRenderer::new(width);
    block.open(out);
    block.push_title("# Todos", out);
    for todo in todos {
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
        } else if status == "completed" {
            theme::SUCCESS
        } else {
            theme::TEXT_MUTED
        };
        let content = todo
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        block.push_body(
            &format!("[{mark}] {content}"),
            Style::default().fg(color),
            out,
        );
    }
    block.close(out);
}

fn push_skill(input: &Value, status: &str, width: u16, out: &mut Vec<Line<'static>>) {
    let name = text_field(input, "name");
    let icon_style = tool_icon_style(status);
    let title_style = tool_title_style(status);
    if name.is_empty() && status == "pending" {
        push_inline_pending("Loading skill...", title_style, width, out);
        return;
    }
    push_inline(
        "→",
        icon_style,
        vec![(format!("Skill \"{name}\""), title_style)],
        width,
        out,
    );
}

fn push_execute(
    metadata: &Value,
    output: &str,
    status: &str,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let calls = metadata
        .get("toolCalls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let runtime_error = metadata
        .get("error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let icon = if runtime_error {
        "✗"
    } else if status == "completed" {
        "✓"
    } else {
        "│"
    };
    let icon_style = if runtime_error {
        Style::default().fg(theme::ERROR)
    } else if status == "completed" {
        Style::default().fg(theme::SUCCESS)
    } else {
        Style::default().fg(theme::WARNING)
    };
    let title_style = tool_title_style(status);
    let mut body = String::from("execute");
    for call in &calls {
        let tool = call.get("tool").and_then(Value::as_str).unwrap_or_default();
        let call_status = call
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let call_input = call.get("input").cloned().unwrap_or(Value::Null);
        let extras = extras_of(&call_input, &[]);
        let mut line = format!("↳ {tool}");
        if !extras.is_empty() {
            line.push(' ');
            line.push_str(&extras);
        }
        if call_status == "error" {
            line.push_str(" (failed)");
        }
        body.push('\n');
        body.push_str(&line);
    }
    push_inline(icon, icon_style, vec![(body, title_style)], width, out);
    if runtime_error && !output.trim().is_empty() {
        let block_chrome = RowChrome {
            prefix: vec![Span::raw("      ")],
            fill_style: Style::default(),
            body_width: width.saturating_sub(6) as usize,
        };
        for (index, hard_line) in output.split('\n').take(4).enumerate() {
            let prefix = if index == 0 { "↳ " } else { "  " };
            block_chrome.emit(
                &[(
                    format!("{prefix}{hard_line}"),
                    Style::default().fg(theme::ERROR),
                )],
                out,
            );
        }
    }
}

fn push_generic(
    name: &str,
    input: &Value,
    output: &str,
    status: &str,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let icon_style = tool_icon_style(status);
    let title_style = tool_title_style(status);
    let extras = extras_of(input, &[]);
    let has_output = !output.trim().is_empty();
    let title = if extras.is_empty() {
        name.to_string()
    } else {
        format!("{name} {extras}")
    };
    if !has_output {
        push_inline("⚙", icon_style, vec![(title, title_style)], width, out);
        return;
    }
    let block = BlockRenderer::new(width);
    block.open(out);
    block.push_title(&format!("# {title}"), out);
    block.push_blank(out, Style::default().bg(theme::BACKGROUND_PANEL));
    for hard_line in output.split('\n').take(3) {
        block.push_body(hard_line, Style::default().fg(theme::TEXT), out);
    }
    block.close(out);
}

fn tool_icon_style(status: &str) -> Style {
    match status {
        "error" => Style::default().fg(theme::ERROR),
        "completed" => Style::default().fg(theme::TEXT_MUTED),
        _ => Style::default().fg(theme::TEXT),
    }
}

fn tool_title_style(status: &str) -> Style {
    match status {
        "error" => Style::default().fg(theme::ERROR),
        "completed" => Style::default().fg(theme::TEXT_MUTED),
        _ => Style::default().fg(theme::TEXT),
    }
}

fn extras_of(input: &Value, omit: &[&str]) -> String {
    let object = match input.as_object() {
        Some(object) => object,
        None => return String::new(),
    };
    let items: Vec<String> = object
        .iter()
        .filter(|(key, value)| {
            !omit.contains(&key.as_str())
                && (value.is_string() || value.is_boolean() || value.is_number())
        })
        .map(|(key, value)| match value {
            Value::String(text) => format!("{key}={text}"),
            other => format!("{key}={other}"),
        })
        .collect();
    if items.is_empty() {
        String::new()
    } else {
        format!("[{}]", items.join(", "))
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

/// Render the sidebar title + optional todos into a flat list of prewrapped
/// visual rows, each already the full inner width with the panel
/// background. Sidebar-only helper because the sidebar has a different
/// chrome (no gutter, panel bg only).
pub fn render_sidebar_body(app: &App, inner_width: u16, out: &mut Vec<Line<'static>>) {
    let panel_bg = Style::default().bg(theme::BACKGROUND_PANEL);
    let width = inner_width as usize;
    push_sidebar_row(
        &app.session_title,
        Style::default()
            .fg(theme::TEXT)
            .bg(theme::BACKGROUND_PANEL)
            .add_modifier(Modifier::BOLD),
        width,
        out,
    );
    if !app.todos.is_empty() {
        out.push(Line::from(vec![Span::styled(" ".repeat(width), panel_bg)]));
        push_sidebar_row(
            "Todos",
            Style::default()
                .fg(theme::ACCENT)
                .bg(theme::BACKGROUND_PANEL)
                .add_modifier(Modifier::BOLD),
            width,
            out,
        );
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
            } else if status == "completed" {
                theme::SUCCESS
            } else {
                theme::TEXT_MUTED
            };
            let content = todo
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            push_sidebar_row(
                &format!("[{mark}] {content}"),
                Style::default().fg(color).bg(theme::BACKGROUND_PANEL),
                width,
                out,
            );
        }
    }
}

fn push_sidebar_row(text: &str, style: Style, width: usize, out: &mut Vec<Line<'static>>) {
    for wrapped in crate::wrap::wrap_text(text, width) {
        let text_width = columns(&wrapped);
        let mut spans = vec![Span::styled(wrapped, style)];
        if text_width < width {
            spans.push(Span::styled(" ".repeat(width - text_width), style));
        }
        out.push(Line::from(spans));
    }
}
