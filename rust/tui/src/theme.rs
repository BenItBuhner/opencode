//! The default "opencode" theme (dark mode), resolved from
//! packages/tui/src/theme/assets/opencode.json. Every color below is the
//! exact hex the TS TUI renders with.

use ratatui::style::Color;

pub const PRIMARY: Color = Color::Rgb(0xfa, 0xb2, 0x83);
pub const SECONDARY: Color = Color::Rgb(0x5c, 0x9c, 0xf5);
pub const ACCENT: Color = Color::Rgb(0x9d, 0x7c, 0xd8);
pub const ERROR: Color = Color::Rgb(0xe0, 0x6c, 0x75);
pub const WARNING: Color = Color::Rgb(0xf5, 0xa7, 0x42);
pub const SUCCESS: Color = Color::Rgb(0x7f, 0xd8, 0x8f);
pub const INFO: Color = Color::Rgb(0x56, 0xb6, 0xc2);
pub const TEXT: Color = Color::Rgb(0xee, 0xee, 0xee);
pub const TEXT_MUTED: Color = Color::Rgb(0x80, 0x80, 0x80);
pub const BACKGROUND: Color = Color::Rgb(0x0a, 0x0a, 0x0a);
pub const BACKGROUND_PANEL: Color = Color::Rgb(0x14, 0x14, 0x14);
pub const BACKGROUND_ELEMENT: Color = Color::Rgb(0x1e, 0x1e, 0x1e);
pub const BORDER: Color = Color::Rgb(0x48, 0x48, 0x48);
/// selectedListItemText defaults to the background color.
pub const SELECTED_FG: Color = BACKGROUND;

/// tint(background, fg, amount) from the TS theme engine.
pub fn tint(from: Color, to: Color, amount: f32) -> Color {
    let (Color::Rgb(fr, fg_, fb), Color::Rgb(tr, tg, tb)) = (from, to) else {
        return from;
    };
    let blend = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount) as u8;
    Color::Rgb(blend(fr, tr), blend(fg_, tg), blend(fb, tb))
}

/// The warning header color for collapsed "Thought:" rows (warning @ 60%).
pub fn thinking() -> Color {
    tint(BACKGROUND, WARNING, 0.6)
}

/// Agents without an explicit color cycle through theme tokens in order
/// (packages/tui/src/context/local.tsx); plan/goal carry explicit colors.
pub fn agent_color(agent: &str, index: usize) -> Color {
    match agent {
        "plan" => WARNING,
        "goal" => ACCENT,
        _ => {
            const CYCLE: [Color; 7] = [SECONDARY, ACCENT, SUCCESS, WARNING, PRIMARY, ERROR, INFO];
            CYCLE[index % CYCLE.len()]
        }
    }
}
