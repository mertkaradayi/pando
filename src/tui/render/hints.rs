//! The key hints a click presses, read off the frame as it was painted.
//!
//! A hint is a key drawn bold in the palette's `hint_key` — orange to the
//! eye, and a colour nothing else on screen has, whatever the theme gives
//! orange — and the words after it: `esc cancel`, `⏎ starts it`. Reading
//! them off the frame rather than out of each dialog's layout keeps one
//! rule for every place a hint is drawn, however it is laid out. A key
//! with no words after it is no hint: every hint says what its key does.

use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use crate::theme::hint_key;

/// Each hint painted in `area`: the cells of its key and of the words
/// after it, and the key as it is written.
pub(super) fn key_hints(buf: &Buffer, area: Rect) -> Vec<(Rect, String)> {
    let area = area.intersection(buf.area);
    let key_colour = hint_key();
    let is_key = |cell: &Cell| cell.fg == key_colour && cell.modifier.contains(Modifier::BOLD);
    let mut hints = Vec::new();
    for y in area.top()..area.bottom() {
        let cell = |x: u16| &buf[(x, y)];
        let mut x = area.left();
        while x < area.right() {
            if !is_key(cell(x)) {
                x += 1;
                continue;
            }
            let start = x;
            let mut key = String::new();
            while x < area.right() && is_key(cell(x)) {
                key.push_str(cell(x).symbol());
                x += 1;
            }
            // The words after it run to the next key, a divider, or two
            // blanks: one blank is the gap inside `starts it`.
            let mut end = x;
            let mut blanks = 0;
            while end < area.right() {
                let next = cell(end);
                if is_key(next) || matches!(next.symbol(), "·" | "│") {
                    break;
                }
                blanks = if next.symbol() == " " { blanks + 1 } else { 0 };
                if blanks == 2 {
                    break;
                }
                end += 1;
            }
            while end > x && cell(end - 1).symbol() == " " {
                end -= 1;
            }
            let key = key.trim();
            if !key.is_empty() && end > x {
                hints.push((Rect::new(start, y, end - start, 1), key.to_string()));
            }
            x = end;
        }
    }
    hints
}
