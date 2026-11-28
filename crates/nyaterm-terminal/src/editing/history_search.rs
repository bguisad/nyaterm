//! Captured input boundaries for readline recovery. These observations grant no
//! editing permission; the caller must confirm the foreground editor and echo.

use nyaterm_core::terminal::input_tracker::{
    MAX_TRACKED_INPUT_BYTES, known_terminal_prompt_prefix,
};
use zeroize::Zeroizing;

use super::{InputMapping, chain, recover_input_at, resolve_input_at};
use crate::{ShellInputAnchor, TerminalSnapshot};

pub struct HistoryInputBoundary {
    pub anchor: ShellInputAnchor,
    prefix: Zeroizing<String>,
    shell_anchor: Option<ShellInputAnchor>,
    cols: usize,
    rows: usize,
}

/// Visible text can omit spaces to the right of the cursor. It is suitable for
/// retaining a mouse target, never for replacing the authoritative input buffer.
pub struct HistoryInputObservation {
    pub visible: Zeroizing<String>,
    pub cursor: usize,
    pub mapping: InputMapping,
}

fn row_prefix(snapshot: &TerminalSnapshot, row: usize, end: usize) -> Option<Zeroizing<String>> {
    let mut text = Zeroizing::new(String::new());
    for cell in snapshot.row(row)?.cells.iter().take(end) {
        if cell.width != 0 {
            text.push_str(if cell.text.is_empty() {
                " "
            } else {
                cell.text.as_ref()
            });
        }
        if text.len() > MAX_TRACKED_INPUT_BYTES {
            return None;
        }
    }
    Some(text)
}

pub fn is_readline_search_display(snapshot: &TerminalSnapshot) -> bool {
    let Some(range) = chain(snapshot) else {
        return false;
    };
    let mut prefix = Zeroizing::new(String::new());
    for row in range {
        let Some(row) = snapshot.row(row) else {
            return false;
        };
        for cell in row.cells.iter().filter(|cell| cell.width != 0) {
            prefix.push_str(if cell.text.is_empty() {
                " "
            } else {
                cell.text.as_ref()
            });
            if prefix.len() >= 26 {
                break;
            }
        }
        if prefix.len() >= 26 {
            break;
        }
    }
    prefix.starts_with("(reverse-i-search)`") || prefix.starts_with("(failed reverse-i-search)`")
}

impl HistoryInputBoundary {
    pub fn prompt(&self) -> &str {
        &self.prefix
    }
    pub fn capture(snapshot: &TerminalSnapshot, confirmed_prompt: Option<&str>) -> Option<Self> {
        if snapshot.history.alternate_screen {
            return None;
        }
        let range = chain(snapshot)?;
        let (row, col) = if let Some(anchor) = snapshot.shell_input_anchor {
            let row = range
                .clone()
                .find(|&row| snapshot.row(row).and_then(|r| r.line_id) == Some(anchor.line_id))?;
            (row, anchor.col)
        } else {
            let row = range.start;
            let text = row_prefix(snapshot, row, snapshot.cols)?;
            let prompt = confirmed_prompt
                .filter(|prompt| !prompt.is_empty() && text.starts_with(prompt))
                .or_else(|| known_terminal_prompt_prefix(&text))?;
            let mut bytes = 0;
            let mut col = 0;
            for cell in &snapshot.row(row)?.cells {
                if bytes == prompt.len() {
                    break;
                }
                if cell.width != 0 {
                    bytes += if cell.text.is_empty() {
                        1
                    } else {
                        cell.text.len()
                    };
                    col += usize::from(cell.width);
                }
            }
            if bytes != prompt.len() {
                return None;
            }
            (row, col)
        };
        let prefix = row_prefix(snapshot, row, col)?;
        if prefix.trim().is_empty() {
            return None;
        }
        Some(Self {
            anchor: ShellInputAnchor {
                line_id: snapshot.row(row)?.line_id?,
                col,
            },
            prefix,
            shell_anchor: snapshot.shell_input_anchor,
            cols: snapshot.cols,
            rows: snapshot.viewport_rows,
        })
    }

    pub fn same_geometry(&self, snapshot: &TerminalSnapshot) -> bool {
        !snapshot.history.alternate_screen
            && snapshot.cols == self.cols
            && snapshot.viewport_rows == self.rows
            && snapshot.shell_input_anchor == self.shell_anchor
            && snapshot
                .rows()
                .iter()
                .any(|row| row.line_id == Some(self.anchor.line_id))
    }

    pub fn is_search_display(&self, snapshot: &TerminalSnapshot) -> bool {
        self.same_geometry(snapshot) && is_readline_search_display(snapshot)
    }

    pub fn observe(
        &self,
        snapshot: &TerminalSnapshot,
        at_end: bool,
    ) -> Option<HistoryInputObservation> {
        if !self.same_geometry(snapshot) {
            return None;
        }
        let row = snapshot
            .rows()
            .iter()
            .position(|row| row.line_id == Some(self.anchor.line_id))?;
        if row_prefix(snapshot, row, self.anchor.col)?.as_str() != self.prefix.as_str() {
            return None;
        }
        let (value, cursor) =
            recover_input_at(snapshot, self.anchor, MAX_TRACKED_INPUT_BYTES, at_end)?;
        let visible = Zeroizing::new(value);
        let mapping = resolve_input_at(snapshot, &visible, cursor, Some(self.anchor))?;
        Some(HistoryInputObservation {
            visible,
            cursor,
            mapping,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::HistoryInputBoundary;
    use crate::TerminalScreen;
    use crate::editing::recover_marked_input;

    #[test]
    fn readline_exit_requires_original_prompt_and_calibration_preserves_tail_spaces() {
        for marked in [false, true] {
            let mut screen = TerminalScreen::new(80, 6);
            screen.advance(b"$ ");
            if marked {
                screen.advance(b"\x1b]133;B\x07");
            }
            let boundary = HistoryInputBoundary::capture(&screen.snapshot(), None).unwrap();
            screen.advance(b"\r(reverse-i-search)`test': echo test tail  \x1b[K");
            assert!(boundary.is_search_display(&screen.snapshot()));
            assert!(boundary.observe(&screen.snapshot(), false).is_none());
            assert!(recover_marked_input(&screen.snapshot(), 4096).is_none());
            screen.advance(b"\r$ echo test tail  \x1b[K\r\x1b[7C");
            let observed = boundary.observe(&screen.snapshot(), false).unwrap();
            assert_eq!(observed.visible.as_str(), "echo test tail");
            assert_eq!(observed.cursor, 5);
            assert!(boundary.observe(&screen.snapshot(), true).is_none());
            screen.advance(b"\r\x1b[2C");
            assert_eq!(
                boundary.observe(&screen.snapshot(), false).unwrap().cursor,
                0
            );
            screen.advance(b"\x1b[16C");
            let recovered = boundary.observe(&screen.snapshot(), true).unwrap();
            assert_eq!(recovered.visible.as_str(), "echo test tail  ");
            assert_eq!(recovered.cursor, recovered.visible.len());
        }
    }

    #[test]
    fn observed_custom_prompt_can_recover_but_arbitrary_output_cannot_create_a_boundary() {
        let mut screen = TerminalScreen::new(80, 6);
        screen.advance(b"custom> ");
        assert!(HistoryInputBoundary::capture(&screen.snapshot(), None).is_none());
        let boundary = HistoryInputBoundary::capture(&screen.snapshot(), Some("custom> ")).unwrap();
        screen.advance(b"echo test");
        assert_eq!(
            boundary
                .observe(&screen.snapshot(), true)
                .unwrap()
                .visible
                .as_str(),
            "echo test"
        );
        screen.advance(b"\rPassword: secret\x1b[K");
        assert!(boundary.observe(&screen.snapshot(), true).is_none());
    }

    #[test]
    fn soft_wrap_and_wide_characters_keep_cell_and_byte_boundaries() {
        let mut screen = TerminalScreen::new(12, 8);
        screen.advance(b"$ ");
        let boundary = HistoryInputBoundary::capture(&screen.snapshot(), None).unwrap();
        screen.advance("echo 界abc xyz  ".as_bytes());
        let recovered = boundary.observe(&screen.snapshot(), true).unwrap();
        assert_eq!(recovered.visible.as_str(), "echo 界abc xyz  ");
        assert!(recovered.mapping.cells.iter().any(|cell| cell.row > 0));
        assert_eq!(recovered.mapping.byte_at(0, 8), Some(5));
        let mut search = TerminalScreen::new(12, 8);
        search.advance(b"(failed reverse-i-search)`test': echo test");
        assert!(super::is_readline_search_display(&search.snapshot()));
    }

    #[test]
    fn stale_suffix_resize_alternate_screen_and_hard_newlines_reject_recovery() {
        let mut screen = TerminalScreen::new(80, 6);
        screen.advance(b"$ ");
        let boundary = HistoryInputBoundary::capture(&screen.snapshot(), None).unwrap();
        screen.advance(b"echo test\x1b[70GOLD\r\x1b[11C");
        assert!(boundary.observe(&screen.snapshot(), true).is_none());
        screen.advance(b"\x1b[K\r\ncontinuation");
        assert!(boundary.observe(&screen.snapshot(), true).is_none());
        screen.advance(b"\x1b[?1049h$ echo test");
        assert!(boundary.observe(&screen.snapshot(), true).is_none());
        screen.advance(b"\x1b[?1049l");
        screen.resize(60, 6);
        assert!(!boundary.same_geometry(&screen.snapshot()));
    }
}
