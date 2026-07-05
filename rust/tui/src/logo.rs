//! The OpenCode block wordmark, ported from packages/tui/src/logo.ts and
//! component/logo.tsx: 4 rows, left half muted / right half bold, with the
//! shadow-character mapping (`_` space-on-shadow, `^` upper-block-on-shadow,
//! `~` shadow upper block, `,` shadow lower block).

use crate::theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

const LEFT: [&str; 4] = [
    "                   ",
    "█▀▀█ █▀▀█ █▀▀█ █▀▀▄",
    "█__█ █__█ █^^^ █__█",
    "▀▀▀▀ █▀▀▀ ▀▀▀▀ ▀~~▀",
];
const RIGHT: [&str; 4] = [
    "             ▄     ",
    "█▀▀▀ █▀▀█ █▀▀█ █▀▀█",
    "█___ █__█ █__█ █^^^",
    "▀▀▀▀ ▀▀▀▀ ▀▀▀▀ ▀▀▀▀",
];

fn half(source: &str, color: ratatui::style::Color, bold: bool) -> Vec<Span<'static>> {
    let shadow = theme::tint(theme::BACKGROUND, color, 0.25);
    let base = if bold {
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(color)
    };
    source
        .chars()
        .map(|ch| match ch {
            '_' => Span::styled(" ", base.bg(shadow)),
            '^' => Span::styled("▀", base.bg(shadow)),
            '~' => Span::styled("▀", Style::default().fg(shadow)),
            ',' => Span::styled("▄", Style::default().fg(shadow)),
            other => Span::styled(other.to_string(), base),
        })
        .collect()
}

pub fn lines() -> Vec<Line<'static>> {
    (0..4)
        .map(|row| {
            let mut spans = half(LEFT[row], theme::TEXT_MUTED, false);
            spans.push(Span::raw(" "));
            spans.extend(half(RIGHT[row], theme::TEXT, true));
            Line::from(spans)
        })
        .collect()
}

pub const WIDTH: u16 = 39;
