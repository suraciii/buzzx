//! `KeyEvent` to `Action`, pure. The key set is the contract in
//! docs/tui-use.md. Navigation mode handles the timeline and channels;
//! composer mode handles text input.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// How many rows one PgUp or PgDn moves.
pub const PAGE_ROWS: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    NextChannel,
    PrevChannel,
    /// Jump to the channel with this 1-based index.
    Channel(usize),
    /// `g` or Home: focus the oldest loaded row.
    Top,
    /// `G` or End: focus the newest row.
    Bottom,
    PageUp,
    PageDown,
    /// `i` or Tab: compose a new message.
    ComposeNew,
    /// Enter: compose a reply to the focused row, or a new message when the
    /// timeline is empty.
    ComposeReply,
    React,
    EditRow,
    DeleteRow,
    ToggleHelp,
    Quit,

    ComposerInput(char),
    ComposerBackspace,
    ComposerNewline,
    ComposerCursorUp,
    ComposerCursorDown,
    ComposerCursorLeft,
    ComposerCursorRight,
    ComposerHome,
    ComposerEnd,
    /// Esc clears the reply or edit target first, and only leaves the
    /// composer on the second press.
    ComposerEscape,
    ComposerSend,
    /// A key the current mode does not define.
    Ignored,
}

/// Map a key press to an action in navigation mode.
pub fn map_navigation(key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') => Action::Quit,
            _ => Action::Ignored,
        };
    }
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => Action::NextChannel,
        KeyCode::Char('k') | KeyCode::Up => Action::PrevChannel,
        KeyCode::Char('g') | KeyCode::Home => Action::Top,
        KeyCode::Char('G') | KeyCode::End => Action::Bottom,
        KeyCode::PageUp => Action::PageUp,
        KeyCode::PageDown => Action::PageDown,
        KeyCode::Char('i') | KeyCode::Tab => Action::ComposeNew,
        KeyCode::Enter => Action::ComposeReply,
        KeyCode::Char('r') => Action::React,
        KeyCode::Char('e') => Action::EditRow,
        KeyCode::Char('d') => Action::DeleteRow,
        KeyCode::Char('?') => Action::ToggleHelp,
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char(d @ '1'..='9') => Action::Channel(d as usize - '0' as usize),
        _ => Action::Ignored,
    }
}

/// Map a key press to an action in composer mode. Character input must not
/// carry Ctrl or Alt: those combinations belong to commands, not text.
pub fn map_composer(key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') => Action::Quit,
            _ => Action::Ignored,
        };
    }
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Enter if alt => Action::ComposerNewline,
        KeyCode::Enter => Action::ComposerSend,
        KeyCode::Esc => Action::ComposerEscape,
        KeyCode::Backspace => Action::ComposerBackspace,
        KeyCode::Up => Action::ComposerCursorUp,
        KeyCode::Down => Action::ComposerCursorDown,
        KeyCode::Left => Action::ComposerCursorLeft,
        KeyCode::Right => Action::ComposerCursorRight,
        KeyCode::Home => Action::ComposerHome,
        KeyCode::End => Action::ComposerEnd,
        KeyCode::Char(c) if !alt => Action::ComposerInput(c),
        _ => Action::Ignored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventState;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: crossterm::event::KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn navigation_moves_channels_with_j_k_and_digits() {
        assert_eq!(
            map_navigation(key(KeyCode::Char('j'), KeyModifiers::NONE)),
            Action::NextChannel
        );
        assert_eq!(
            map_navigation(key(KeyCode::Down, KeyModifiers::NONE)),
            Action::NextChannel
        );
        assert_eq!(
            map_navigation(key(KeyCode::Char('k'), KeyModifiers::NONE)),
            Action::PrevChannel
        );
        assert_eq!(
            map_navigation(key(KeyCode::Up, KeyModifiers::NONE)),
            Action::PrevChannel
        );
        assert_eq!(
            map_navigation(key(KeyCode::Char('3'), KeyModifiers::NONE)),
            Action::Channel(3)
        );
    }

    #[test]
    fn navigation_scrolls_and_targets_rows() {
        assert_eq!(
            map_navigation(key(KeyCode::Char('g'), KeyModifiers::NONE)),
            Action::Top
        );
        assert_eq!(
            map_navigation(key(KeyCode::Home, KeyModifiers::NONE)),
            Action::Top
        );
        assert_eq!(
            map_navigation(key(KeyCode::Char('G'), KeyModifiers::SHIFT)),
            Action::Bottom
        );
        assert_eq!(
            map_navigation(key(KeyCode::End, KeyModifiers::NONE)),
            Action::Bottom
        );
        assert_eq!(
            map_navigation(key(KeyCode::PageUp, KeyModifiers::NONE)),
            Action::PageUp
        );
        assert_eq!(
            map_navigation(key(KeyCode::PageDown, KeyModifiers::NONE)),
            Action::PageDown
        );
        assert_eq!(
            map_navigation(key(KeyCode::Char('r'), KeyModifiers::NONE)),
            Action::React
        );
        assert_eq!(
            map_navigation(key(KeyCode::Char('e'), KeyModifiers::NONE)),
            Action::EditRow
        );
        assert_eq!(
            map_navigation(key(KeyCode::Char('d'), KeyModifiers::NONE)),
            Action::DeleteRow
        );
        assert_eq!(
            map_navigation(key(KeyCode::Enter, KeyModifiers::NONE)),
            Action::ComposeReply
        );
    }

    #[test]
    fn ctrl_c_quits_from_either_mode() {
        assert_eq!(
            map_navigation(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Quit
        );
        assert_eq!(
            map_composer(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Quit
        );
    }

    #[test]
    fn composer_text_needs_no_modifiers() {
        assert_eq!(
            map_composer(key(KeyCode::Char('x'), KeyModifiers::NONE)),
            Action::ComposerInput('x')
        );
        // Alt+x is not text; terminals use it for commands.
        assert_eq!(
            map_composer(key(KeyCode::Char('x'), KeyModifiers::ALT)),
            Action::Ignored
        );
    }

    #[test]
    fn alt_enter_inserts_a_newline_and_enter_sends() {
        assert_eq!(
            map_composer(key(KeyCode::Enter, KeyModifiers::ALT)),
            Action::ComposerNewline
        );
        assert_eq!(
            map_composer(key(KeyCode::Enter, KeyModifiers::NONE)),
            Action::ComposerSend
        );
    }

    #[test]
    fn composer_editing_keys_map_to_cursor_actions() {
        assert_eq!(
            map_composer(key(KeyCode::Backspace, KeyModifiers::NONE)),
            Action::ComposerBackspace
        );
        assert_eq!(
            map_composer(key(KeyCode::Left, KeyModifiers::NONE)),
            Action::ComposerCursorLeft
        );
        assert_eq!(
            map_composer(key(KeyCode::Right, KeyModifiers::NONE)),
            Action::ComposerCursorRight
        );
        assert_eq!(
            map_composer(key(KeyCode::Home, KeyModifiers::NONE)),
            Action::ComposerHome
        );
        assert_eq!(
            map_composer(key(KeyCode::End, KeyModifiers::NONE)),
            Action::ComposerEnd
        );
    }
}
