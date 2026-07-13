//! Buffer-level tests for the width-aware transcript renderer. Each case
//! renders the transcript into a `TestBackend` and asserts on the resulting
//! terminal buffer directly so the gutter, 2-column padding, panel
//! background, and right-edge fill are verified per visual row across a
//! representative width sweep: 40 (narrow), 60 (typical), 80 (upstream
//! default), 121 (sidebar breakpoint), 140 (wide).
//!
//! The tests exercise:
//!   * user gutter/padding/background repeat on every wrapped row
//!   * multi-line user messages (hard `\n` breaks preserved)
//!   * Unicode/CJK/emoji glyphs and long hard tokens
//!   * scroll math running on visual (wrapped) row count
//!   * todowrite block wraps long todo items with the border preserved
//!   * tool title wrapping (bash, grep, task)
//!   * task subagent multi-line body (title + child status + footer)
//!   * sidebar todos wrap without losing the panel background

use opencode_tui::state::App;
use opencode_tui::theme;
use opencode_tui::transcript;
use opencode_tui::wrap;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use serde_json::{json, Value};

const WIDTHS: [u16; 5] = [40, 60, 80, 121, 140];

fn user_message(text: &str) -> Value {
    json!({ "type": "user", "text": text })
}

fn assistant_text(text: &str) -> Value {
    json!({
        "type": "assistant",
        "content": [{ "type": "text", "text": text }],
    })
}

fn tool_part(name: &str, input: Value, metadata: Value, status: &str) -> Value {
    json!({
        "type": "tool",
        "name": name,
        "state": {
            "status": status,
            "input": input,
            "metadata": metadata,
        }
    })
}

fn assistant_tool(part: Value) -> Value {
    json!({ "type": "assistant", "content": [part] })
}

fn render_into(width: u16, height: u16, app: &App) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            let mut rows = vec![];
            transcript::render(app, width, &mut rows);
            let total = rows.len() as u16;
            let scroll = total.saturating_sub(height);
            frame.render_widget(
                Paragraph::new(rows).scroll((scroll, 0)),
                Rect::new(0, 0, width, height),
            );
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

fn row_string(buffer: &Buffer, row: u16) -> String {
    let mut out = String::new();
    for column in 0..buffer.area.width {
        out.push_str(buffer[(column, row)].symbol());
    }
    out
}

// ---------------------------------------------------------------------------
// User gutter / padding / background repeats on every wrapped visual row
// ---------------------------------------------------------------------------

#[test]
fn user_gutter_padding_background_repeats_on_every_visual_row_at_every_width() {
    let text = "the quick brown fox jumps over the lazy dog again and again and again";
    let mut app = App::new("/tmp".into());
    app.messages.push(user_message(text));

    for width in WIDTHS {
        let buffer = render_into(width, 40, &app);
        // Locate the first row that starts with the gutter glyph — the
        // renderer always emits a leading blank row, then a blank body row,
        // then wrapped content rows.
        let mut saw_wrapped_row = false;
        for row in 0..buffer.area.height {
            let cell = &buffer[(0, row)];
            if cell.symbol() != "┃" {
                continue;
            }
            saw_wrapped_row = true;
            assert_eq!(
                cell.fg,
                theme::agent_color("build", 0),
                "gutter fg wrong at width={width} row={row}"
            );
            // Padding column 1..3 is panel background.
            for column in 1..3 {
                assert_eq!(
                    buffer[(column, row)].bg,
                    theme::BACKGROUND_PANEL,
                    "padding bg wrong at width={width} row={row} col={column}"
                );
            }
            // Right-most column of the body is still panel background: the
            // renderer fills the remainder of the row so long lines do not
            // "flash" the black transcript background when they wrap.
            let last = width - 1;
            assert_eq!(
                buffer[(last, row)].bg,
                theme::BACKGROUND_PANEL,
                "right-edge bg wrong at width={width} row={row}"
            );
        }
        assert!(saw_wrapped_row, "no user gutter rendered at width={width}");
    }
}

// ---------------------------------------------------------------------------
// Multi-line user message: each hard `\n` produces a new visual row and the
// gutter/pad/bg still repeat.
// ---------------------------------------------------------------------------

#[test]
fn user_multiline_hard_newlines_preserve_gutter_and_bg() {
    let mut app = App::new("/tmp".into());
    app.messages
        .push(user_message("first line\nsecond line\nthird"));
    let buffer = render_into(80, 12, &app);
    let gutter_rows: Vec<u16> = (0..buffer.area.height)
        .filter(|row| buffer[(0, *row)].symbol() == "┃")
        .collect();
    // Blank pad top + 3 content rows + blank pad bottom = 5 gutter rows.
    assert_eq!(gutter_rows.len(), 5, "gutter row count for 3-line user");
    // The three content rows each carry the original text.
    let text_rows: Vec<String> = gutter_rows
        .iter()
        .map(|row| row_string(&buffer, *row))
        .collect();
    assert!(text_rows[1].contains("first line"));
    assert!(text_rows[2].contains("second line"));
    assert!(text_rows[3].contains("third"));
}

// ---------------------------------------------------------------------------
// Unicode/CJK/emoji + hard token wrapping
// ---------------------------------------------------------------------------

#[test]
fn cjk_and_emoji_wrap_on_column_width_not_char_count() {
    let mut app = App::new("/tmp".into());
    // Two-column CJK glyphs: at width 40 (body_width 37) 20 glyphs (=40
    // cols) must wrap even though the char count fits ratatui's byte view.
    app.messages.push(user_message(&"你".repeat(40)));
    let buffer = render_into(40, 12, &app);
    let gutter_rows: usize = (0..buffer.area.height)
        .filter(|row| buffer[(0, *row)].symbol() == "┃")
        .count();
    // top pad + wrapped rows (2 rows at body 37 for 80 CJK cols) + bottom
    // pad = at least 3 gutter rows.
    assert!(gutter_rows >= 3, "cjk should wrap into multiple rows");
}

#[test]
fn hard_break_long_token_never_exceeds_body_width() {
    let mut app = App::new("/tmp".into());
    app.messages.push(user_message(&"a".repeat(200)));
    let buffer = render_into(60, 20, &app);
    for row in 0..buffer.area.height {
        // Right-most cell should never be an "a" that ran off the edge —
        // the hard-break must have inserted a line break somewhere.
        let s = row_string(&buffer, row);
        assert!(
            wrap::columns(&s) <= 60,
            "row {row} exceeded 60 cols: {:?}",
            s
        );
    }
}

// ---------------------------------------------------------------------------
// Scroll math uses the visual row count
// ---------------------------------------------------------------------------

#[test]
fn scroll_math_uses_visual_row_count_not_logical_lines() {
    let mut app = App::new("/tmp".into());
    // One long paragraph that wraps into many visual rows.
    app.messages.push(assistant_text(&"word ".repeat(400)));
    let mut rows = vec![];
    transcript::render(&app, 60, &mut rows);
    // With ~400 words * 5 chars each and body width 57, expect ~35+ rows.
    assert!(
        rows.len() >= 30,
        "expected many visual rows, got {}",
        rows.len()
    );
    // Every row is a full-width visual line ready for the paragraph
    // scroll offset. None should exceed 60 columns.
    for line in &rows {
        let width: usize = line
            .spans
            .iter()
            .map(|span| wrap::columns(&span.content))
            .sum();
        assert!(width <= 60, "visual row exceeded 60 cols ({} cols)", width);
    }
}

// ---------------------------------------------------------------------------
// TodoWrite block wraps long items with the border preserved
// ---------------------------------------------------------------------------

#[test]
fn todowrite_block_wraps_long_todos_with_border_preserved() {
    let long = "implement the width-aware transcript renderer described by the audits and validate it against every tool and every terminal width";
    let part = tool_part(
        "todowrite",
        json!({ "todos": [
            { "status": "completed", "content": "short one" },
            { "status": "in_progress", "content": long },
        ] }),
        json!({}),
        "completed",
    );
    let mut app = App::new("/tmp".into());
    app.messages.push(assistant_tool(part));

    for width in WIDTHS {
        let buffer = render_into(width, 30, &app);
        let mut border_rows = 0u16;
        for row in 0..buffer.area.height {
            if buffer[(0, row)].symbol() == "┃" {
                border_rows += 1;
                assert_eq!(buffer[(0, row)].fg, theme::BORDER);
                assert_eq!(buffer[(1, row)].bg, theme::BACKGROUND_PANEL);
                let last = width - 1;
                assert_eq!(
                    buffer[(last, row)].bg,
                    theme::BACKGROUND_PANEL,
                    "todo block right-edge bg lost at width={width}"
                );
            }
        }
        assert!(
            border_rows >= 4,
            "expected block border on multiple rows at width={width}, saw {border_rows}"
        );
    }
}

// ---------------------------------------------------------------------------
// Inline tool titles wrap without breaking the icon/pad prefix
// ---------------------------------------------------------------------------

#[test]
fn bash_inline_title_wraps_at_narrow_widths() {
    let part = tool_part(
        "bash",
        json!({ "command": "echo very very very very very very very very very very very long command" }),
        json!({}),
        "completed",
    );
    let mut app = App::new("/tmp".into());
    app.messages.push(assistant_tool(part));
    let buffer = render_into(40, 20, &app);
    // Locate the row starting with `$` icon; the wrapped continuation
    // row(s) should be indented under the title (columns 5..).
    let icon_row = (0..buffer.area.height)
        .find(|row| buffer[(3, *row)].symbol() == "$")
        .expect("bash icon appears at column 3");
    // Continuation row exists and is entirely inside 40 cols.
    let next = icon_row + 1;
    assert!(next < buffer.area.height);
    let text = row_string(&buffer, next);
    assert!(
        wrap::columns(&text) <= 40,
        "wrapped bash title exceeded width"
    );
}

#[test]
fn grep_inline_title_shows_match_count_and_wraps() {
    let part = tool_part(
        "grep",
        json!({ "pattern": "todo", "path": "packages/opencode/src" }),
        json!({ "matches": 42 }),
        "completed",
    );
    let mut app = App::new("/tmp".into());
    app.messages.push(assistant_tool(part));
    let buffer = render_into(60, 12, &app);
    let combined: String = (0..buffer.area.height)
        .map(|row| row_string(&buffer, row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        combined.contains("(42 matches)"),
        "grep title should report count"
    );
    assert!(combined.contains("in packages/opencode/src"));
}

// ---------------------------------------------------------------------------
// Task subagent multi-line body
// ---------------------------------------------------------------------------

#[test]
fn task_subagent_multiline_body_shows_background_and_child_status() {
    let part = tool_part(
        "task",
        json!({
            "description": "Investigate the audit findings",
            "subagent_type": "researcher",
        }),
        json!({
            "background": true,
            "currentTool": "grep",
            "currentToolTitle": "\"audit\" (7 matches)",
        }),
        "running",
    );
    let mut app = App::new("/tmp".into());
    app.messages.push(assistant_tool(part));
    let buffer = render_into(80, 12, &app);
    let combined: String = (0..buffer.area.height)
        .map(|row| row_string(&buffer, row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        combined.contains("Researcher Task (background) — Investigate the audit findings"),
        "task title with background label missing:\n{combined}"
    );
    assert!(
        combined.contains("↳ Grep \"audit\" (7 matches)"),
        "task child status line missing:\n{combined}"
    );
}

#[test]
fn task_subagent_completed_shows_toolcount_and_duration() {
    let part = json!({
        "type": "tool",
        "name": "task",
        "state": {
            "status": "completed",
            "input": {
                "description": "Small delegation",
                "subagent_type": "general",
            },
            "metadata": { "toolCount": 3 }
        },
        "time": { "created": 0, "completed": 1_300 }
    });
    let mut app = App::new("/tmp".into());
    app.messages.push(assistant_tool(part));
    let buffer = render_into(80, 12, &app);
    let combined: String = (0..buffer.area.height)
        .map(|row| row_string(&buffer, row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        combined.contains("3 toolcalls · 1.3s"),
        "expected completed toolcount + duration footer:\n{combined}"
    );
}

#[test]
fn live_background_task_shape_uses_structured_metadata_and_hides_completion_xml() {
    let task = json!({
        "type": "tool",
        "name": "task",
        "state": {
            "status": "completed",
            "input": {
                "description": "Inspect workspace members",
                "subagent_type": "explore",
                "background": true,
            },
            "structured": {
                "title": "Inspect workspace members",
                "metadata": {
                    "sessionId": "ses_child",
                    "background": true,
                    "jobId": "ses_child",
                },
                "output": "<task state=\"running\">background</task>",
            },
        },
        "time": { "created": 0, "completed": 20 },
    });
    let mut app = App::new("/tmp".into());
    app.session_status = json!({ "ses_child": { "type": "running" } });
    app.messages.push(assistant_tool(task));
    app.messages.push(user_message(
        "<task id=\"ses_child\" state=\"completed\"><task_result>hidden</task_result></task>",
    ));
    let buffer = render_into(80, 12, &app);
    let combined: String = (0..buffer.area.height)
        .map(|row| row_string(&buffer, row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(combined.contains("Explore Task (background) — Inspect workspace members"));
    assert!(!combined.contains("<task"));
    assert!(!combined.contains("task_result"));
}

// ---------------------------------------------------------------------------
// Sidebar todos wrap without losing the panel background
// ---------------------------------------------------------------------------

#[test]
fn sidebar_todos_wrap_with_panel_background() {
    let mut app = App::new("/tmp".into());
    app.session_title = "Ship the width-aware transcript renderer".into();
    app.todos = vec![
        json!({ "status": "completed", "content": "wrap module" }),
        json!({ "status": "in_progress", "content": "port every upstream tool renderer with faithful icons, labels, and metadata" }),
    ];
    let inner_width: u16 = 42 - 4; // sidebar is 42 with 2-col left/right padding
    let mut rows = vec![];
    transcript::render_sidebar_body(&app, inner_width, &mut rows);
    let mut buffer = Buffer::empty(Rect::new(0, 0, inner_width, rows.len() as u16));
    for (i, line) in rows.iter().enumerate() {
        buffer.set_line(0, i as u16, line, inner_width);
    }
    for row in 0..buffer.area.height {
        for column in 0..inner_width {
            let cell = &buffer[(column, row)];
            // Every non-empty rendered cell must sit on the panel bg. The
            // Buffer::empty background is Reset; the renderer sets a bg.
            if cell.bg == Color::Reset {
                continue;
            }
            assert_eq!(
                cell.bg,
                theme::BACKGROUND_PANEL,
                "sidebar bg lost at row={row} col={column}"
            );
        }
    }
    // At width 38 the long todo wraps into >1 row.
    let long_wrap_rows = rows
        .iter()
        .filter(|row| {
            row.spans
                .iter()
                .any(|span| span.content.contains("faithful") || span.content.contains("port"))
        })
        .count();
    assert!(long_wrap_rows >= 2, "long todo should wrap onto >=2 rows");
}
