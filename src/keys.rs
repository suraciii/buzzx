//! `KeyEvent` to `Action`, pure. The key set is the contract in
//! docs/tui-use.md and docs/tui.md. Navigation mode handles the timeline and
//! channels; composer mode handles text input. The layout decides whether
//! `j` and `k` move the channel list or the timeline.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::layout::LayoutMode;

/// How many rows one PgUp or PgDn moves.
pub const PAGE_ROWS: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    NextChannel,
    PrevChannel,
    /// The next or previous timeline row. One-column layouts move the
    /// timeline with `j` and `k`, because it is the list they show.
    NextRow,
    PrevRow,
    /// Jump to the channel with this 1-based index.
    Channel(usize),
    /// `c`: open the channel picker, or close it when it is open.
    TogglePicker,
    PickerNext,
    PickerPrev,
    PickerConfirm,
    /// `f`: the next Inbox filter.
    FilterNext,
    /// Scroll the help text by this many lines. At the minimum size the text
    /// is taller than the screen, and the whole of it must stay reachable.
    HelpScroll(isize),
    /// Esc: close the topmost overlay. The composer keeps its own escape.
    Dismiss,
    /// `g` or Home: focus the oldest loaded row.
    Top,
    /// `G` or End: focus the newest row.
    Bottom,
    PageUp,
    PageDown,
    /// `i`: compose a new message.
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

/// Map a key press to an action in navigation mode. `layout` selects the
/// list that `j` and `k` move; `picker` routes keys to the open conversation
/// picker, and `help` to the help text drawn over everything else. Each
/// overlay isolates the rest of the navigation keys.
pub fn map_navigation(key: KeyEvent, layout: LayoutMode, picker: bool, help: bool) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') => Action::Quit,
            _ => Action::Ignored,
        };
    }
    if layout == LayoutMode::TooSmall {
        // The interface is a size message; only leaving it makes sense.
        return match key.code {
            KeyCode::Char('q') => Action::Quit,
            _ => Action::Ignored,
        };
    }
    if help {
        return match key.code {
            KeyCode::Char('j') | KeyCode::Down => Action::HelpScroll(1),
            KeyCode::Char('k') | KeyCode::Up => Action::HelpScroll(-1),
            KeyCode::PageDown => Action::HelpScroll(PAGE_ROWS as isize),
            KeyCode::PageUp => Action::HelpScroll(-(PAGE_ROWS as isize)),
            KeyCode::Esc | KeyCode::Char('?') => Action::Dismiss,
            KeyCode::Char('q') => Action::Quit,
            _ => Action::Ignored,
        };
    }
    if picker {
        return match key.code {
            KeyCode::Char('j') | KeyCode::Down => Action::PickerNext,
            KeyCode::Char('k') | KeyCode::Up => Action::PickerPrev,
            KeyCode::Char('f') | KeyCode::Tab => Action::FilterNext,
            KeyCode::Enter => Action::PickerConfirm,
            KeyCode::Esc | KeyCode::Char('c') => Action::Dismiss,
            KeyCode::Char('?') => Action::ToggleHelp,
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Char(d @ '1'..='9') => Action::Channel(d as usize - '0' as usize),
            _ => Action::Ignored,
        };
    }
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => match layout {
            LayoutMode::Wide => Action::NextChannel,
            _ => Action::NextRow,
        },
        KeyCode::Char('k') | KeyCode::Up => match layout {
            LayoutMode::Wide => Action::PrevChannel,
            _ => Action::PrevRow,
        },
        KeyCode::Char('g') | KeyCode::Home => Action::Top,
        KeyCode::Char('G') | KeyCode::End => Action::Bottom,
        KeyCode::PageUp => Action::PageUp,
        KeyCode::PageDown => Action::PageDown,
        KeyCode::Char('i') => Action::ComposeNew,
        KeyCode::Enter => Action::ComposeReply,
        KeyCode::Char('c') => Action::TogglePicker,
        KeyCode::Char('f') => Action::FilterNext,
        KeyCode::Char('r') => Action::React,
        KeyCode::Char('e') => Action::EditRow,
        KeyCode::Char('d') => Action::DeleteRow,
        KeyCode::Char('?') => Action::ToggleHelp,
        KeyCode::Esc => Action::Dismiss,
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

    fn wide(code: KeyCode) -> Action {
        map_navigation(
            key(code, KeyModifiers::NONE),
            LayoutMode::Wide,
            false,
            false,
        )
    }

    fn narrow(code: KeyCode) -> Action {
        map_navigation(
            key(code, KeyModifiers::NONE),
            LayoutMode::Narrow,
            false,
            false,
        )
    }

    #[test]
    fn navigation_moves_channels_with_j_k_and_digits() {
        assert_eq!(wide(KeyCode::Char('j')), Action::NextChannel);
        assert_eq!(wide(KeyCode::Down), Action::NextChannel);
        assert_eq!(wide(KeyCode::Char('k')), Action::PrevChannel);
        assert_eq!(wide(KeyCode::Up), Action::PrevChannel);
        assert_eq!(wide(KeyCode::Char('3')), Action::Channel(3));
    }

    #[test]
    fn a_one_column_layout_moves_rows_with_j_k_and_opens_the_picker_with_c() {
        assert_eq!(narrow(KeyCode::Char('j')), Action::NextRow);
        assert_eq!(narrow(KeyCode::Down), Action::NextRow);
        assert_eq!(narrow(KeyCode::Char('k')), Action::PrevRow);
        assert_eq!(narrow(KeyCode::Up), Action::PrevRow);
        assert_eq!(narrow(KeyCode::Char('c')), Action::TogglePicker);
        // The digits still jump straight to a channel.
        assert_eq!(narrow(KeyCode::Char('3')), Action::Channel(3));
    }

    #[test]
    fn an_open_picker_takes_the_selection_keys_and_isolates_the_rest() {
        let picker = |code: KeyCode| {
            map_navigation(
                key(code, KeyModifiers::NONE),
                LayoutMode::Narrow,
                true,
                false,
            )
        };
        assert_eq!(picker(KeyCode::Char('j')), Action::PickerNext);
        assert_eq!(picker(KeyCode::Up), Action::PickerPrev);
        assert_eq!(picker(KeyCode::Enter), Action::PickerConfirm);
        assert_eq!(picker(KeyCode::Esc), Action::Dismiss);
        assert_eq!(picker(KeyCode::Char('c')), Action::Dismiss);
        assert_eq!(picker(KeyCode::Char('q')), Action::Quit);
        // Composing, reacting, and editing do not fire while picking.
        assert_eq!(picker(KeyCode::Char('i')), Action::Ignored);
        assert_eq!(picker(KeyCode::Char('r')), Action::Ignored);
        assert_eq!(picker(KeyCode::Char('e')), Action::Ignored);
        assert_eq!(picker(KeyCode::Char('g')), Action::Ignored);
    }

    #[test]
    fn a_too_small_terminal_only_answers_q_and_ctrl_c() {
        let small = |code: KeyCode, modifiers| {
            map_navigation(key(code, modifiers), LayoutMode::TooSmall, false, false)
        };
        assert_eq!(small(KeyCode::Char('q'), KeyModifiers::NONE), Action::Quit);
        assert_eq!(
            small(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Action::Quit
        );
        assert_eq!(
            small(KeyCode::Char('i'), KeyModifiers::NONE),
            Action::Ignored
        );
        assert_eq!(
            small(KeyCode::Char('f'), KeyModifiers::NONE),
            Action::Ignored
        );
        assert_eq!(
            small(KeyCode::Char('c'), KeyModifiers::NONE),
            Action::Ignored
        );
    }

    #[test]
    fn navigation_scrolls_and_targets_rows() {
        assert_eq!(wide(KeyCode::Char('g')), Action::Top);
        assert_eq!(wide(KeyCode::Home), Action::Top);
        assert_eq!(
            map_navigation(
                key(KeyCode::Char('G'), KeyModifiers::SHIFT),
                LayoutMode::Wide,
                false,
                false
            ),
            Action::Bottom
        );
        assert_eq!(wide(KeyCode::End), Action::Bottom);
        assert_eq!(wide(KeyCode::PageUp), Action::PageUp);
        assert_eq!(wide(KeyCode::PageDown), Action::PageDown);
        assert_eq!(wide(KeyCode::Char('r')), Action::React);
        assert_eq!(wide(KeyCode::Char('e')), Action::EditRow);
        assert_eq!(wide(KeyCode::Char('d')), Action::DeleteRow);
        assert_eq!(wide(KeyCode::Enter), Action::ComposeReply);
        assert_eq!(wide(KeyCode::Esc), Action::Dismiss);
    }

    #[test]
    fn an_open_help_text_scrolls_and_keeps_its_exit() {
        let help = |code: KeyCode| {
            map_navigation(
                key(code, KeyModifiers::NONE),
                LayoutMode::Minimal,
                false,
                true,
            )
        };
        assert_eq!(help(KeyCode::Char('j')), Action::HelpScroll(1));
        assert_eq!(help(KeyCode::Down), Action::HelpScroll(1));
        assert_eq!(help(KeyCode::Char('k')), Action::HelpScroll(-1));
        assert_eq!(
            help(KeyCode::PageDown),
            Action::HelpScroll(PAGE_ROWS as isize)
        );
        assert_eq!(help(KeyCode::Esc), Action::Dismiss);
        assert_eq!(help(KeyCode::Char('q')), Action::Quit);
        // Everything the help text does not use is inert: no key reaches a
        // conversation behind it.
        assert_eq!(help(KeyCode::Char('i')), Action::Ignored);
        assert_eq!(help(KeyCode::Enter), Action::Ignored);
    }

    #[test]
    fn the_picker_cycles_the_filter_and_the_sidebar_does_too() {
        let picker = |code: KeyCode| {
            map_navigation(
                key(code, KeyModifiers::NONE),
                LayoutMode::Narrow,
                true,
                false,
            )
        };
        assert_eq!(picker(KeyCode::Char('f')), Action::FilterNext);
        assert_eq!(picker(KeyCode::Tab), Action::FilterNext);
        assert_eq!(wide(KeyCode::Char('f')), Action::FilterNext);
    }

    #[test]
    fn ctrl_c_quits_from_either_mode() {
        assert_eq!(
            map_navigation(
                key(KeyCode::Char('c'), KeyModifiers::CONTROL),
                LayoutMode::Wide,
                false,
                false
            ),
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
