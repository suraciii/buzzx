//! Terminal size to layout mode, pure. The mode follows the reported size;
//! a device type is never inferred.

/// The smallest terminal the client runs in.
pub const MIN_WIDTH: u16 = 24;
pub const MIN_HEIGHT: u16 = 6;

/// Includes the divider, so hiding the sidebar returns every column.
pub const SIDEBAR_WIDTH: u16 = 24;

/// The presentation the terminal can hold. Docs/tui.md defines the modes and
/// their keys; this is the selection rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    /// Optional conversation sidebar beside the full channel surface.
    Desk,
    /// Full-width timeline and composer.
    Wide,
    /// One-column timeline with the full-screen channel switcher.
    Narrow,
    /// One column and compact rows.
    Minimal,
    /// Below the minimum: a size message stands in for the interface.
    TooSmall,
}

/// The richest layout whose minimum the terminal satisfies. A terminal that
/// fits none of them gets the size message.
pub fn mode(width: u16, height: u16) -> LayoutMode {
    if width >= 112 && height >= 16 {
        LayoutMode::Desk
    } else if width >= 80 && height >= 12 {
        LayoutMode::Wide
    } else if width >= 40 && height >= 10 {
        LayoutMode::Narrow
    } else if width >= MIN_WIDTH && height >= MIN_HEIGHT {
        LayoutMode::Minimal
    } else {
        LayoutMode::TooSmall
    }
}

/// Sidebar and main column budgets; overlays always pass `false`.
pub fn column_widths(width: u16, height: u16, sidebar_open: bool) -> (u16, u16) {
    let sidebar = if sidebar_open && mode(width, height) == LayoutMode::Desk {
        SIDEBAR_WIDTH
    } else {
        0
    };
    (sidebar, width.saturating_sub(sidebar))
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
    fn desk_yields_every_column_when_hidden_or_below_its_boundary() {
        assert_eq!(mode(112, 16), LayoutMode::Desk);
        assert_eq!(column_widths(112, 16, true), (24, 88));
        assert_eq!(column_widths(120, 30, false), (0, 120));
        assert_eq!(column_widths(111, 16, true), (0, 111));
        assert_eq!(column_widths(112, 15, true), (0, 112));
        assert_eq!(mode(111, 16), LayoutMode::Wide);
        assert_eq!(mode(112, 15), LayoutMode::Wide);
        // Main inset and focus gutter still leave more than 72 body cells.
        assert!(column_widths(112, 16, true).1 - 6 >= 72);
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
