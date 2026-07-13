//! Width-aware grapheme/word wrapping shared by the transcript, sidebar,
//! todowrite block, and every tool renderer. All layout runs through this
//! module so the gutter, 2-column padding, panel background, and right-edge
//! fill can be re-emitted on every visual row rather than depending on
//! ratatui `Paragraph` auto-wrap (which strips background per row and
//! ignores prefixes).
//!
//! Column widths come from `unicode-width`; grapheme boundaries from
//! `unicode-segmentation`. Emoji, CJK, and combining marks all count as
//! their true terminal column width; ASCII control characters count as 0.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// A single-grapheme cell tagged with a foreground/background style so we
/// can rebuild wrapped spans without losing per-run styles.
#[derive(Clone, Debug)]
struct Cell {
    grapheme: String,
    style: Style,
    width: usize,
}

/// Terminal column width of a display string, honouring CJK + emoji widths.
pub fn columns(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Explode a list of styled segments into per-grapheme cells so a wrapper
/// can measure real terminal columns instead of char counts.
fn cells_of(segments: &[(String, Style)]) -> Vec<Cell> {
    let mut cells = Vec::with_capacity(segments.iter().map(|s| s.0.len()).sum());
    for (text, style) in segments {
        for grapheme in text.as_str().graphemes(true) {
            let width = UnicodeWidthStr::width(grapheme);
            cells.push(Cell {
                grapheme: grapheme.to_string(),
                style: *style,
                width,
            });
        }
    }
    cells
}

fn is_whitespace(cell: &Cell) -> bool {
    cell.grapheme.chars().all(char::is_whitespace)
}

/// Word-wrap a single logical line (no `\n` inside) worth of styled
/// segments into a list of rows, each row a list of cells with total
/// display width `<= width`. A token longer than `width` is hard-broken at
/// grapheme boundaries. Trailing whitespace after a wrap point is dropped.
fn wrap_cells(cells: &[Cell], width: usize) -> Vec<Vec<Cell>> {
    if width == 0 {
        return vec![vec![]];
    }
    let mut rows: Vec<Vec<Cell>> = vec![vec![]];
    let mut row_width = 0usize;
    let mut index = 0usize;
    while index < cells.len() {
        if is_whitespace(&cells[index]) {
            if row_width + cells[index].width <= width && row_width > 0 {
                let w = cells[index].width;
                rows.last_mut().unwrap().push(cells[index].clone());
                row_width += w;
            }
            index += 1;
            continue;
        }
        let word_start = index;
        while index < cells.len() && !is_whitespace(&cells[index]) {
            index += 1;
        }
        let word_end = index;
        let word_width: usize = cells[word_start..word_end].iter().map(|c| c.width).sum();
        let fits = row_width + word_width <= width;
        if !fits && row_width > 0 {
            trim_trailing_whitespace(rows.last_mut().unwrap(), &mut row_width);
            rows.push(vec![]);
            row_width = 0;
        }
        if word_width <= width {
            for cell in cells[word_start..word_end].iter() {
                rows.last_mut().unwrap().push(cell.clone());
            }
            row_width += word_width;
            continue;
        }
        for cell in cells[word_start..word_end].iter() {
            if row_width + cell.width > width && row_width > 0 {
                rows.push(vec![]);
                row_width = 0;
            }
            let w = cell.width;
            rows.last_mut().unwrap().push(cell.clone());
            row_width += w;
        }
    }
    trim_trailing_whitespace(rows.last_mut().unwrap(), &mut row_width);
    rows
}

fn trim_trailing_whitespace(row: &mut Vec<Cell>, row_width: &mut usize) {
    while let Some(last) = row.last() {
        if is_whitespace(last) {
            *row_width = row_width.saturating_sub(last.width);
            row.pop();
        } else {
            break;
        }
    }
}

/// Coalesce a row of per-grapheme cells back into styled `Span`s. Adjacent
/// cells with the same style are merged so the resulting `Line` uses the
/// minimal number of spans (nicer for tests and Buffer diffing).
fn spans_of(row: &[Cell]) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = vec![];
    for cell in row {
        if let Some(last) = spans.last_mut() {
            if last.style == cell.style {
                let mut owned = last.content.to_string();
                owned.push_str(&cell.grapheme);
                *last = Span::styled(owned, cell.style);
                continue;
            }
        }
        spans.push(Span::styled(cell.grapheme.clone(), cell.style));
    }
    spans
}

/// A rendering "chrome" that repeats on every visual row: the gutter/prefix
/// spans, the style used to pad the right edge, and the body width in
/// columns. `emit_wrapped` word-wraps the supplied styled body segments,
/// then for each wrapped row emits `prefix + body + right-pad(fill_style)`
/// so the panel background reaches to the right edge on every row.
pub struct RowChrome {
    pub prefix: Vec<Span<'static>>,
    pub fill_style: Style,
    pub body_width: usize,
}

impl RowChrome {
    /// Push wrapped visual rows for one logical block of body segments into
    /// `out`. `body` may contain `\n` graphemes; each hard-break introduces
    /// a new visual row before word-wrap re-applies.
    pub fn emit(&self, body: &[(String, Style)], out: &mut Vec<Line<'static>>) {
        let mut segments: Vec<(String, Style)> = vec![];
        for (text, style) in body {
            let mut first = true;
            for chunk in text.split('\n') {
                if !first {
                    self.push_line(&segments, out);
                    segments.clear();
                }
                if !chunk.is_empty() {
                    segments.push((chunk.to_string(), *style));
                }
                first = false;
            }
        }
        self.push_line(&segments, out);
    }

    fn push_line(&self, segments: &[(String, Style)], out: &mut Vec<Line<'static>>) {
        if self.body_width == 0 {
            out.push(Line::from(self.prefix.clone()));
            return;
        }
        let cells = cells_of(segments);
        let rows = if cells.is_empty() {
            vec![vec![]]
        } else {
            wrap_cells(&cells, self.body_width)
        };
        for row in rows {
            let mut spans = self.prefix.clone();
            let used: usize = row.iter().map(|cell| cell.width).sum();
            spans.extend(spans_of(&row));
            if used < self.body_width {
                spans.push(Span::styled(
                    " ".repeat(self.body_width - used),
                    self.fill_style,
                ));
            }
            out.push(Line::from(spans));
        }
    }
}

/// Wrap `text` at grapheme boundaries into rows of `width` columns using
/// word-wrap (fall back to a hard grapheme break on oversized tokens).
/// Used for the sidebar todo lines and any pure-text wrapping site.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut rows = vec![];
    for hard_line in text.split('\n') {
        let cells = cells_of(&[(hard_line.to_string(), Style::default())]);
        let wrapped = if cells.is_empty() {
            vec![vec![]]
        } else {
            wrap_cells(&cells, width)
        };
        for row in wrapped {
            let text: String = row.iter().map(|cell| cell.grapheme.as_str()).collect();
            rows.push(text);
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_simple_ascii_words() {
        let rows = wrap_text("the quick brown fox jumps", 10);
        assert_eq!(rows, vec!["the quick", "brown fox", "jumps"]);
    }

    #[test]
    fn hard_breaks_long_token() {
        let rows = wrap_text("aaaaaaaaaaaaaaa", 5);
        assert_eq!(rows, vec!["aaaaa", "aaaaa", "aaaaa"]);
    }

    #[test]
    fn cjk_column_widths_are_two() {
        let rows = wrap_text("你好世界", 4);
        assert_eq!(rows, vec!["你好", "世界"]);
    }

    #[test]
    fn preserves_hard_newlines() {
        let rows = wrap_text("foo\nbar", 10);
        assert_eq!(rows, vec!["foo", "bar"]);
    }

    #[test]
    fn emoji_counts_two_columns() {
        // 🙂 is width 2 per unicode-width.
        let rows = wrap_text("🙂🙂🙂", 4);
        assert_eq!(rows, vec!["🙂🙂", "🙂"]);
    }

    #[test]
    fn styled_wrap_preserves_prefix_and_right_fill() {
        let chrome = RowChrome {
            prefix: vec![Span::styled("|", Style::default()), Span::raw("  ")],
            fill_style: Style::default(),
            body_width: 6,
        };
        let mut out = vec![];
        chrome.emit(&[("hello world".into(), Style::default())], &mut out);
        assert_eq!(out.len(), 2);
        let widths: Vec<usize> = out
            .iter()
            .map(|line| line.spans.iter().map(|s| columns(&s.content)).sum())
            .collect();
        // Prefix width = 3, body width = 6 → total 9 per row.
        assert_eq!(widths, vec![9, 9]);
    }
}
