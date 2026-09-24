//! Terminal size to layout mode, pure. The mode follows the reported size;
//! a device type is never inferred.

/// The smallest terminal the client runs in.
pub const MIN_WIDTH: u16 = 24;
pub const MIN_HEIGHT: u16 = 6;

/// The presentation the terminal can hold. Docs/tui.md defines the modes and
/// their keys; this is the selection rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    /// Channels, timeline, and composer side by side.
    Wide,
    /// One-column timeline with the full-screen channel picker.
    Narrow,
    /// One column and compact rows.
    Minimal,
    /// Below the minimum: a size message stands in for the interface.
    TooSmall,
}

/// The richest layout whose minimum the terminal satisfies. A terminal that
/// fits none of them gets the size message.
pub fn mode(width: u16, height: u16) -> LayoutMode {
    if width >= 80 && height >= 12 {
        LayoutMode::Wide
    } else if width >= 40 && height >= 10 {
        LayoutMode::Narrow
    } else if width >= MIN_WIDTH && height >= MIN_HEIGHT {
        LayoutMode::Minimal
    } else {
        LayoutMode::TooSmall
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_mode_keeps_its_stated_minimum() {
        assert_eq!(mode(80, 12), LayoutMode::Wide);
        assert_eq!(mode(40, 10), LayoutMode::Narrow);
        assert_eq!(mode(24, 6), LayoutMode::Minimal);
    }

    #[test]
    fn a_tall_but_narrow_terminal_still_gets_one_column() {
        // Rows cannot buy back columns: narrow needs 40 of them.
        assert_eq!(mode(39, 60), LayoutMode::Minimal);
        assert_eq!(mode(79, 9), LayoutMode::Minimal);
        // Wide needs 12 rows; below that the widest layout that fits wins,
        // and the compact markers are for narrow terminals, not for this.
        assert_eq!(mode(120, 11), LayoutMode::Narrow);
    }

    #[test]
    fn the_too_small_corner_is_below_either_minimum() {
        assert_eq!(mode(23, 24), LayoutMode::TooSmall);
        assert_eq!(mode(120, 5), LayoutMode::TooSmall);
        assert_eq!(mode(0, 0), LayoutMode::TooSmall);
    }
}
