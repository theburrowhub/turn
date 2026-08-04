//! Terminal dimensions on the wire.

use serde::{Deserialize, Serialize};

/// Rows and columns of a pty.
///
/// A protocol type of its own rather than `turn_pty::ScreenSize`, because the
/// contract between daemon and UI must not depend on which pty implementation the
/// daemon happens to use — that is precisely the coupling this crate exists to
/// prevent. The two are structurally identical and clamp the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PtySize {
    pub rows: u16,
    pub cols: u16,
}

impl Default for PtySize {
    fn default() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

impl PtySize {
    /// Builds a size, clamping away the degenerate cases.
    ///
    /// Zero is clamped rather than rejected: a UI reports a size while a pane is
    /// still being laid out and will legitimately measure zero for a frame, and
    /// failing that request would be worse than starting one row tall and being
    /// corrected a moment later. A zero passed through to the kernel or to the
    /// terminal parser is a panic.
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            rows: rows.max(1),
            cols: cols.max(1),
        }
    }

    /// Whether this size came in with a dimension that had to be clamped.
    pub fn was_degenerate(rows: u16, cols: u16) -> bool {
        rows == 0 || cols == 0
    }

    /// Total cells, for sizing a replay buffer.
    pub fn cells(&self) -> u32 {
        self.rows as u32 * self.cols as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_dimension_is_clamped_rather_than_rejected() {
        assert_eq!(PtySize::new(0, 0), PtySize { rows: 1, cols: 1 });
        assert_eq!(PtySize::new(0, 80), PtySize { rows: 1, cols: 80 });
        assert!(PtySize::was_degenerate(0, 80));
        assert!(!PtySize::was_degenerate(24, 80));
    }

    #[test]
    fn the_default_is_the_conventional_terminal() {
        assert_eq!(PtySize::default(), PtySize { rows: 24, cols: 80 });
        assert_eq!(PtySize::default().cells(), 1_920);
    }

    #[test]
    fn a_size_round_trips_as_two_plain_numbers() {
        let size = PtySize::new(48, 160);
        let json = serde_json::to_string(&size).unwrap();
        assert_eq!(json, "{\"rows\":48,\"cols\":160}");
        assert_eq!(serde_json::from_str::<PtySize>(&json).unwrap(), size);
    }
}
