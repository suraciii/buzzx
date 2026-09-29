//! `KeyEvent` to `Action`, pure. The key set is the contract in
//! docs/tui-use.md and docs/tui.md. Navigation mode handles the timeline and
//! the conversation switcher; composer mode handles text input. Every layout
//! moves the timeline with `j` and `k`: the conversation list is an overlay,
//! not a column, so the layout no longer chooses what a step means.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::layout::LayoutMode;

/// How many rows one PgUp or PgDn moves.
pub const PAGE_ROWS: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// The next or previous timeline row.
    NextRow,
    PrevRow,
    /// Jump to the channel with this 1-based index.
    Channel(usize),
    /// `c`: open the conversation switcher, or close it when it is open.
    ToggleSwitcher,
    SwitcherNext,
    SwitcherPrev,
    SwitcherConfirm,
    SwitcherInput(char),
    SwitcherBackspace,
    /// `C`: open the saved-community selector, or close it when it is open.
    ToggleCommunityPicker,
    CommunityNext,
    CommunityPrev,
    CommunityConfirm,
    /// `Ctrl+P`: open the command palette, or close it when it is open.
    TogglePalette,
    PaletteNext,
    PalettePrev,
    /// Enter: run the selected command.
    PaletteConfirm,
    PickerNext,
    PickerPrev,
    PickerConfirm,
    PickerRetry,
    /// Ask for a fresh channel roster to answer an unresolved creation.
    CreateChannelRefresh,
    LifecycleRefresh,
    MemberSelect,
    MemberAdd,
    MemberRemove,
    MemberRole,
    MemberLeave,
    MemberRoleNext,
    MemberRoleSelect(u8),
    MemberRolePrev,
    /// `f`: the next Inbox filter.
    FilterNext,
    /// `a`: open the Agents overlay, or close it when it is open.
    ToggleAgents,
    /// `j` and `k` inside the Agents overlay.
    AgentsNext,
    AgentsPrev,
    /// Enter inside the Agents overlay: open the selected Agent, or the
    /// selected working conversation.
    AgentsConfirm,
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
    /// `t`: open the focused message's thread.
    OpenThread,
    /// Open the full-screen message search.
    OpenSearch,
    SearchNext,
    SearchPrev,
    SearchSubmit,
    /// `/`: edit the query text again.
    SearchEdit,
    /// `f`: open the filter form over the results.
    SearchFilter,
    SearchInput(char),
    SearchBackspace,
    ContextNext,
    ContextPrev,
    ContextTop,
    ContextBottom,
    ContextPageUp,
    ContextPageDown,
    ContextOpenThread,
    ContextOpenReader,
    ContextReply,
    ContextLoadOlder,
    ContextLoadNewer,
    ContextLeave,
    /// `v`: read the focused confirmed message in a full-screen reader.
    OpenReader,
    /// One displayed reader line.
    ReaderNextLine,
    ReaderPrevLine,
    /// One reader viewport minus one line.
    ReaderPageUp,
    ReaderPageDown,
    ReaderTop,
    ReaderBottom,
    /// Enter from the reader starts a reply to its bound message.
    ReaderReply,
    /// Esc or `v` returns to the reader's origin.
    ReaderClose,
    /// Esc inside a thread: return to the channel it was opened from.
    ThreadLeave,
    /// Enter inside a thread: compose a reply to the focused row.
    ThreadReplyFocused,
    /// `i` or Tab inside a thread: compose a reply to the root.
    ThreadReplyRoot,
    /// `t` inside a thread: retry a failed read. It never retries a write.
    ThreadRetry,
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

/// Which overlay is on screen. The overlays are exclusive: help draws over
/// everything, and each open overlay isolates the navigation keys it does not
/// use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Switcher,
    SwitcherSearch,
    /// The command palette: an action directory over the timeline.
    Palette,
    /// The saved-community selector: the profiles the identity may switch to.
    CommunityPicker,
    CreateChannel,
    EditChannel,
    Archive,
    Members,
    MemberPicker,
    Help,
    Agents,
}

/// What the navigation keys act on. Full-screen search/context/reader views
/// replace the timeline instead of overlaying it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Channel,
    Thread,
    Context,
    Search,
    Reader,
}

/// Which sub-surface of full-screen search a key press lands in. It decides
/// whether printable characters are query text or filter-form and switcher
/// commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Navigating results.
    Results,
    /// Query text entry: every printable character is query text.
    QueryEdit,
    /// The filter form over scope, author and time.
    Form,
    /// The author picker over known profiles or an exact public key.
    AuthorPick,
    /// The conversation picker for a one-conversation scope.
    ScopePick,
}

/// Map a key press to an action in navigation mode. `layout` only separates
/// the too-small message from the interface; `overlay` routes keys to the
/// overlay that is open; `surface` selects the channel timeline or an open
/// thread; `search_mode` selects which search sub-surface owns the keys.
pub fn map_navigation(
    key: KeyEvent,
    layout: LayoutMode,
    overlay: Overlay,
    surface: Surface,
    search_mode: SearchMode,
) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        // `Ctrl+F` and `Ctrl+P` are the timeline's own: they open over the
        // channel timeline itself and nowhere else. An open overlay keeps its
        // own keys - help, the switcher and the Agents list are dismissed on
        // their own terms, and the switcher's query must survive - the
        // size message answers only `q`, and inside a thread, search, context
        // or reader the destination is protected until the user returns.
        // `Ctrl+P` a second time closes the palette it opened.
        let timeline_only = surface == Surface::Channel
            && layout != LayoutMode::TooSmall
            && matches!(overlay, Overlay::None | Overlay::Palette);
        return match key.code {
            KeyCode::Char('c') => Action::Quit,
            // The create form's own refresh: an unresolved creation is settled
            // by the roster answer the user asks for here.
            KeyCode::Char('r') if overlay == Overlay::CreateChannel => Action::CreateChannelRefresh,
            KeyCode::Char('r')
                if matches!(
                    overlay,
                    Overlay::EditChannel
                        | Overlay::Archive
                        | Overlay::Members
                        | Overlay::MemberPicker
                ) =>
            {
                Action::LifecycleRefresh
            }
            // The same refresh from the timeline: a form may be closed while
            // its write is still unresolved.
            KeyCode::Char('r') if overlay == Overlay::None && surface == Surface::Channel => {
                Action::CreateChannelRefresh
            }
            KeyCode::Char('f') if timeline_only => Action::OpenSearch,
            KeyCode::Char('p') if timeline_only => Action::TogglePalette,
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
    if overlay == Overlay::Help {
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
    if overlay == Overlay::CreateChannel {
        return match key.code {
            KeyCode::Esc => Action::Dismiss,
            KeyCode::Enter => Action::ComposerSend,
            KeyCode::Tab | KeyCode::Down => Action::ComposerCursorDown,
            KeyCode::BackTab | KeyCode::Up => Action::ComposerCursorUp,
            KeyCode::Left => Action::ComposerCursorLeft,
            KeyCode::Right => Action::ComposerCursorRight,
            KeyCode::Backspace => Action::ComposerBackspace,
            KeyCode::Char(c) if key.modifiers.is_empty() => Action::ComposerInput(c),
            _ => Action::Ignored,
        };
    }
    if overlay == Overlay::EditChannel {
        return match key.code {
            KeyCode::Esc => Action::Dismiss,
            KeyCode::Enter => Action::ComposerSend,
            KeyCode::Tab | KeyCode::Down => Action::ComposerCursorDown,
            KeyCode::BackTab | KeyCode::Up => Action::ComposerCursorUp,
            KeyCode::Left => Action::ComposerCursorLeft,
            KeyCode::Right => Action::ComposerCursorRight,
            KeyCode::Backspace => Action::ComposerBackspace,
            KeyCode::Char(c) if key.modifiers.is_empty() => Action::ComposerInput(c),
            _ => Action::Ignored,
        };
    }
    if overlay == Overlay::Archive {
        return match key.code {
            KeyCode::Enter | KeyCode::Char('y') => Action::MemberSelect,
            KeyCode::Esc | KeyCode::Char('n') => Action::Dismiss,
            _ => Action::Ignored,
        };
    }
    if overlay == Overlay::Members {
        return match key.code {
            KeyCode::Esc => Action::Dismiss,
            KeyCode::Up | KeyCode::Char('k') => Action::PickerPrev,
            KeyCode::Down | KeyCode::Char('j') => Action::PickerNext,
            KeyCode::Enter => Action::MemberSelect,
            KeyCode::Char('a') => Action::MemberAdd,
            KeyCode::Char('d') => Action::MemberRemove,
            KeyCode::Char('r') => Action::MemberRole,
            KeyCode::Char('l') => Action::MemberLeave,
            KeyCode::Char(c @ '1'..='5') => Action::MemberRoleSelect(c as u8 - b'1'),
            KeyCode::Left | KeyCode::BackTab => Action::MemberRolePrev,
            KeyCode::Right | KeyCode::Tab => Action::MemberRoleNext,
            _ => Action::Ignored,
        };
    }
    if overlay == Overlay::MemberPicker {
        return match key.code {
            KeyCode::Esc => Action::Dismiss,
            KeyCode::Up => Action::PickerPrev,
            KeyCode::Down => Action::PickerNext,
            KeyCode::Char(' ') => Action::MemberSelect,
            KeyCode::Enter => Action::MemberAdd,
            KeyCode::Left | KeyCode::BackTab => Action::MemberRolePrev,
            KeyCode::Right | KeyCode::Tab => Action::MemberRoleNext,
            KeyCode::Backspace => Action::ComposerBackspace,
            KeyCode::Char(c) if key.modifiers.is_empty() => Action::ComposerInput(c),
            _ => Action::Ignored,
        };
    }
    if overlay == Overlay::Palette {
        return match key.code {
            KeyCode::Char('j') | KeyCode::Down => Action::PaletteNext,
            KeyCode::Char('k') | KeyCode::Up => Action::PalettePrev,
            KeyCode::Enter => Action::PaletteConfirm,
            KeyCode::Esc => Action::Dismiss,
            KeyCode::Char('?') => Action::ToggleHelp,
            KeyCode::Char('q') => Action::Quit,
            _ => Action::Ignored,
        };
    }
    if overlay == Overlay::Agents {
        return match key.code {
            KeyCode::Char('j') | KeyCode::Down => Action::AgentsNext,
            KeyCode::Char('k') | KeyCode::Up => Action::AgentsPrev,
            KeyCode::Enter => Action::AgentsConfirm,
            KeyCode::Esc => Action::Dismiss,
            KeyCode::Char('a') => Action::ToggleAgents,
            KeyCode::Char('?') => Action::ToggleHelp,
            KeyCode::Char('q') => Action::Quit,
            _ => Action::Ignored,
        };
    }
    if surface == Surface::Search {
        if search_mode == SearchMode::Results {
            return match key.code {
                KeyCode::Char('j') | KeyCode::Down => Action::SearchNext,
                KeyCode::Char('k') | KeyCode::Up => Action::SearchPrev,
                KeyCode::Enter => Action::SearchSubmit,
                KeyCode::Char('/') => Action::SearchEdit,
                KeyCode::Char('s') => Action::SearchInput('s'),
                KeyCode::Char('t') => Action::SearchInput('t'),
                KeyCode::Char('f') => Action::SearchFilter,
                KeyCode::Char('?') => Action::ToggleHelp,
                KeyCode::Esc => Action::Dismiss,
                KeyCode::Char('q') => Action::Quit,
                _ => Action::Ignored,
            };
        }
        if search_mode == SearchMode::QueryEdit {
            // Query text entry: every printable character is query text.
            return match key.code {
                KeyCode::Char(c) if key.modifiers.is_empty() => Action::SearchInput(c),
                KeyCode::Backspace => Action::SearchBackspace,
                KeyCode::Enter => Action::SearchSubmit,
                KeyCode::Esc => Action::Dismiss,
                _ => Action::Ignored,
            };
        }
        // The filter form and both switchers: j/k and the arrows move, other
        // printable characters adjust the focused control or type text.
        return match key.code {
            KeyCode::Char('j') | KeyCode::Down => Action::SearchNext,
            KeyCode::Char('k') | KeyCode::Up => Action::SearchPrev,
            KeyCode::Char(c) if key.modifiers.is_empty() => Action::SearchInput(c),
            KeyCode::Backspace => Action::SearchBackspace,
            KeyCode::Enter => Action::SearchSubmit,
            KeyCode::Esc => Action::Dismiss,
            _ => Action::Ignored,
        };
    }
    if surface == Surface::Context {
        return match key.code {
            KeyCode::Char('j') | KeyCode::Down => Action::ContextNext,
            KeyCode::Char('k') | KeyCode::Up => Action::ContextPrev,
            KeyCode::Char('g') | KeyCode::Home => Action::ContextTop,
            KeyCode::Char('G') | KeyCode::End => Action::ContextBottom,
            KeyCode::PageUp => Action::ContextPageUp,
            KeyCode::PageDown => Action::ContextPageDown,
            KeyCode::Char('t') => Action::ContextOpenThread,
            KeyCode::Char('v') => Action::ContextOpenReader,
            KeyCode::Enter => Action::ContextReply,
            KeyCode::Char('[') => Action::ContextLoadOlder,
            KeyCode::Char(']') => Action::ContextLoadNewer,
            KeyCode::Esc => Action::ContextLeave,
            KeyCode::Char('?') => Action::ToggleHelp,
            KeyCode::Char('q') => Action::Quit,
            _ => Action::Ignored,
        };
    }
    if surface == Surface::Reader {
        return match key.code {
            KeyCode::Char('j') | KeyCode::Down => Action::ReaderNextLine,
            KeyCode::Char('k') | KeyCode::Up => Action::ReaderPrevLine,
            KeyCode::Char('g') | KeyCode::Home => Action::ReaderTop,
            KeyCode::Char('G') | KeyCode::End => Action::ReaderBottom,
            KeyCode::PageUp => Action::ReaderPageUp,
            KeyCode::PageDown => Action::ReaderPageDown,
            KeyCode::Enter => Action::ReaderReply,
            KeyCode::Esc | KeyCode::Char('v') => Action::ReaderClose,
            KeyCode::Char('?') => Action::ToggleHelp,
            KeyCode::Char('q') => Action::Quit,
            _ => Action::Ignored,
        };
    }
    if overlay == Overlay::CommunityPicker {
        return match key.code {
            KeyCode::Char('j') | KeyCode::Down => Action::CommunityNext,
            KeyCode::Char('k') | KeyCode::Up => Action::CommunityPrev,
            KeyCode::Enter => Action::CommunityConfirm,
            KeyCode::Esc | KeyCode::Char('C') => Action::Dismiss,
            KeyCode::Char('?') => Action::ToggleHelp,
            KeyCode::Char('q') => Action::Quit,
            _ => Action::Ignored,
        };
    }
    if overlay == Overlay::SwitcherSearch {
        return match key.code {
            KeyCode::Esc => Action::Dismiss,
            KeyCode::Enter => Action::SwitcherConfirm,
            KeyCode::Backspace => Action::SwitcherBackspace,
            KeyCode::Down => Action::SwitcherNext,
            KeyCode::Up => Action::SwitcherPrev,
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SwitcherInput(c)
            }
            _ => Action::Ignored,
        };
    }
    if overlay == Overlay::Switcher {
        return match key.code {
            KeyCode::Char('j') | KeyCode::Down => Action::SwitcherNext,
            KeyCode::Char('k') | KeyCode::Up => Action::SwitcherPrev,
            KeyCode::Char('f') | KeyCode::Tab => Action::FilterNext,
            KeyCode::Char('/') => Action::SwitcherInput('/'),
            KeyCode::Enter => Action::SwitcherConfirm,
            KeyCode::Esc | KeyCode::Char('c') => Action::Dismiss,
            KeyCode::Char('?') => Action::ToggleHelp,
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Backspace => Action::SwitcherBackspace,
            KeyCode::Char(d @ '1'..='9') => Action::Channel(d as usize - '0' as usize),
            // Any other printable character is the local name filter: the
            // pointer says `type to filter`, so the first letter starts the
            // query instead of being ignored.
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SwitcherInput(c)
            }
            _ => Action::Ignored,
        };
    }
    if surface == Surface::Thread {
        // One full-screen thread: `j`/`k` move message focus at every width,
        // the timeline navigation keeps its meaning, and the conversation
        // keys are not here - they must not switch the destination.
        return match key.code {
            KeyCode::Char('v') => Action::OpenReader,
            KeyCode::Char('j') | KeyCode::Down => Action::NextRow,
            KeyCode::Char('k') | KeyCode::Up => Action::PrevRow,
            KeyCode::Char('[') | KeyCode::PageUp => Action::ContextLoadOlder,
            KeyCode::Char(']') | KeyCode::PageDown => Action::ContextLoadNewer,
            KeyCode::Char('g') | KeyCode::Home => Action::Top,
            KeyCode::Char('G') | KeyCode::End => Action::Bottom,
            KeyCode::Char('/') => Action::OpenSearch,
            KeyCode::Enter => Action::ThreadReplyFocused,
            KeyCode::Char('i') | KeyCode::Tab => Action::ThreadReplyRoot,
            KeyCode::Char('t') => Action::ThreadRetry,
            KeyCode::Char('r') => Action::React,
            KeyCode::Char('e') => Action::EditRow,
            KeyCode::Char('d') => Action::DeleteRow,
            KeyCode::Char('?') => Action::ToggleHelp,
            KeyCode::Esc => Action::ThreadLeave,
            KeyCode::Char('q') => Action::Quit,
            _ => Action::Ignored,
        };
    }
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => Action::NextRow,
        KeyCode::Char('k') | KeyCode::Up => Action::PrevRow,
        KeyCode::Char('g') | KeyCode::Home => Action::Top,
        KeyCode::Char('G') | KeyCode::End => Action::Bottom,
        KeyCode::Char('[') => Action::ContextLoadOlder,
        KeyCode::Char(']') => Action::ContextLoadNewer,
        KeyCode::PageUp => Action::PageUp,
        KeyCode::PageDown => Action::PageDown,
        KeyCode::Char('/') => Action::OpenSearch,
        KeyCode::Char('c') => Action::ToggleSwitcher,
        KeyCode::Char('C') => Action::ToggleCommunityPicker,
        KeyCode::Char('v') => Action::OpenReader,
        KeyCode::Char('t') => Action::OpenThread,
        KeyCode::Char('i') => Action::ComposeNew,
        KeyCode::Enter => Action::ComposeReply,
        KeyCode::Char('a') => Action::ToggleAgents,
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

/// Map a key press in composer mode. Character input must not carry Ctrl or
/// Alt: those combinations belong to commands, not text. `picker_open` reserves
/// navigation keys only while the mention list owns the composer.
pub fn map_composer(key: KeyEvent, picker_open: bool) -> Action {
    // Shift+Tab arrives as BackTab carrying SHIFT: the picker reads it as the
    // bare BackTab it is, so the key that moves backwards reaches the list.
    let key = if key.code == KeyCode::BackTab && key.modifiers == KeyModifiers::SHIFT {
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE)
    } else {
        key
    };
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') => Action::Quit,
            KeyCode::Char('f') => Action::OpenSearch,
            KeyCode::Char('r') if picker_open => Action::PickerRetry,
            _ => Action::Ignored,
        };
    }
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    if picker_open && key.modifiers.is_empty() {
        match key.code {
            KeyCode::Enter => return Action::PickerConfirm,
            KeyCode::Esc => return Action::Dismiss,
            // Only keys that cannot be part of a name move the cursor: a name
            // may contain `j` or `k`, so those stay text.
            KeyCode::Up | KeyCode::BackTab => return Action::PickerPrev,
            KeyCode::Down | KeyCode::Tab => return Action::PickerNext,
            KeyCode::Char('?') => return Action::ToggleHelp,
            _ => {}
        }
    }
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
            Overlay::None,
            Surface::Channel,
            SearchMode::Results,
        )
    }

    fn narrow(code: KeyCode) -> Action {
        map_navigation(
            key(code, KeyModifiers::NONE),
            LayoutMode::Narrow,
            Overlay::None,
            Surface::Channel,
            SearchMode::Results,
        )
    }

    /// A key press in the Agents overlay.
    fn agents(code: KeyCode) -> Action {
        map_navigation(
            key(code, KeyModifiers::NONE),
            LayoutMode::Wide,
            Overlay::Agents,
            Surface::Channel,
            SearchMode::Results,
        )
    }

    /// A key press while one thread is on screen.
    fn thread(code: KeyCode) -> Action {
        map_navigation(
            key(code, KeyModifiers::NONE),
            LayoutMode::Wide,
            Overlay::None,
            Surface::Thread,
            SearchMode::Results,
        )
    }

    fn reader(code: KeyCode) -> Action {
        map_navigation(
            key(code, KeyModifiers::NONE),
            LayoutMode::Wide,
            Overlay::None,
            Surface::Reader,
            SearchMode::Results,
        )
    }

    #[test]
    fn the_reader_isolates_scrolling_reply_and_return_keys() {
        assert_eq!(reader(KeyCode::Char('j')), Action::ReaderNextLine);
        assert_eq!(reader(KeyCode::Down), Action::ReaderNextLine);
        assert_eq!(reader(KeyCode::Char('k')), Action::ReaderPrevLine);
        assert_eq!(reader(KeyCode::PageDown), Action::ReaderPageDown);
        assert_eq!(reader(KeyCode::Char('g')), Action::ReaderTop);
        assert_eq!(reader(KeyCode::Char('G')), Action::ReaderBottom);
        assert_eq!(reader(KeyCode::Enter), Action::ReaderReply);
        assert_eq!(reader(KeyCode::Esc), Action::ReaderClose);
        assert_eq!(reader(KeyCode::Char('v')), Action::ReaderClose);
        assert_eq!(reader(KeyCode::Char('i')), Action::Ignored);
    }

    #[test]
    fn search_results_open_the_form_and_the_form_takes_characters() {
        let results = |code| {
            map_navigation(
                key(code, KeyModifiers::NONE),
                LayoutMode::Wide,
                Overlay::None,
                Surface::Search,
                SearchMode::Results,
            )
        };
        assert_eq!(results(KeyCode::Char('/')), Action::SearchEdit);
        assert_eq!(results(KeyCode::Char('f')), Action::SearchFilter);
        assert_eq!(results(KeyCode::Enter), Action::SearchSubmit);
        assert_eq!(results(KeyCode::Esc), Action::Dismiss);
        // Legacy scope/time toggles remain available without applying a query
        // until the user submits.
        assert_eq!(results(KeyCode::Char('s')), Action::SearchInput('s'));

        let form = |code| {
            map_navigation(
                key(code, KeyModifiers::NONE),
                LayoutMode::Wide,
                Overlay::None,
                Surface::Search,
                SearchMode::Form,
            )
        };
        assert_eq!(form(KeyCode::Char('s')), Action::SearchInput('s'));
        assert_eq!(form(KeyCode::Char('t')), Action::SearchInput('t'));
        assert_eq!(form(KeyCode::Char('j')), Action::SearchNext);
        assert_eq!(form(KeyCode::Down), Action::SearchNext);
        assert_eq!(form(KeyCode::Enter), Action::SearchSubmit);
        assert_eq!(form(KeyCode::Esc), Action::Dismiss);
    }

    #[test]
    fn the_thread_surface_has_its_own_keys_and_no_conversation_keys() {
        assert_eq!(thread(KeyCode::Char('j')), Action::NextRow);
        assert_eq!(thread(KeyCode::Down), Action::NextRow);
        assert_eq!(thread(KeyCode::Char('k')), Action::PrevRow);
        assert_eq!(thread(KeyCode::Char('g')), Action::Top);
        assert_eq!(thread(KeyCode::Char('G')), Action::Bottom);
        assert_eq!(thread(KeyCode::PageUp), Action::ContextLoadOlder);
        assert_eq!(thread(KeyCode::PageDown), Action::ContextLoadNewer);
        assert_eq!(thread(KeyCode::Char('/')), Action::OpenSearch);
        assert_eq!(thread(KeyCode::Enter), Action::ThreadReplyFocused);
        assert_eq!(thread(KeyCode::Char('i')), Action::ThreadReplyRoot);
        assert_eq!(thread(KeyCode::Tab), Action::ThreadReplyRoot);
        assert_eq!(thread(KeyCode::Char('t')), Action::ThreadRetry);
        assert_eq!(thread(KeyCode::Char('r')), Action::React);
        assert_eq!(thread(KeyCode::Char('e')), Action::EditRow);
        assert_eq!(thread(KeyCode::Char('d')), Action::DeleteRow);
        assert_eq!(thread(KeyCode::Char('?')), Action::ToggleHelp);
        assert_eq!(thread(KeyCode::Esc), Action::ThreadLeave);
        assert_eq!(thread(KeyCode::Char('q')), Action::Quit);
        // The conversation keys are not here: none of them may switch the
        // destination behind the thread.
        for code in [
            KeyCode::Char('c'),
            KeyCode::Char('f'),
            KeyCode::Char('a'),
            KeyCode::Char('3'),
        ] {
            assert_eq!(
                thread(code),
                Action::Ignored,
                "{code:?} is not a thread key"
            );
        }
    }

    #[test]
    fn help_still_works_inside_a_thread() {
        assert_eq!(
            map_navigation(
                key(KeyCode::Char('j'), KeyModifiers::NONE),
                LayoutMode::Wide,
                Overlay::Help,
                Surface::Thread,
                SearchMode::Results,
            ),
            Action::HelpScroll(1)
        );
        assert_eq!(
            map_navigation(
                key(KeyCode::Esc, KeyModifiers::NONE),
                LayoutMode::Wide,
                Overlay::Help,
                Surface::Thread,
                SearchMode::Results,
            ),
            Action::Dismiss
        );
    }

    #[test]
    fn t_opens_a_thread_in_the_channel_and_retries_inside_one() {
        assert_eq!(wide(KeyCode::Char('t')), Action::OpenThread);
        assert_eq!(thread(KeyCode::Char('t')), Action::ThreadRetry);
    }

    #[test]
    fn navigation_moves_rows_with_j_k_and_digits() {
        assert_eq!(wide(KeyCode::Char('j')), Action::NextRow);
        assert_eq!(wide(KeyCode::Down), Action::NextRow);
        assert_eq!(wide(KeyCode::Char('k')), Action::PrevRow);
        assert_eq!(wide(KeyCode::Up), Action::PrevRow);
        assert_eq!(wide(KeyCode::Char('3')), Action::Channel(3));
    }

    #[test]
    fn a_one_column_layout_moves_rows_with_j_k_and_opens_the_switcher_with_c() {
        assert_eq!(narrow(KeyCode::Char('j')), Action::NextRow);
        assert_eq!(narrow(KeyCode::Down), Action::NextRow);
        assert_eq!(narrow(KeyCode::Char('k')), Action::PrevRow);
        assert_eq!(narrow(KeyCode::Up), Action::PrevRow);
        assert_eq!(narrow(KeyCode::Char('c')), Action::ToggleSwitcher);
        // The digits still jump straight to a channel.
        assert_eq!(narrow(KeyCode::Char('3')), Action::Channel(3));
    }

    #[test]
    fn every_layout_opens_the_same_switcher_with_c_and_moves_rows_with_j_k() {
        for layout in [LayoutMode::Wide, LayoutMode::Narrow, LayoutMode::Minimal] {
            let press = |code: KeyCode| {
                map_navigation(
                    key(code, KeyModifiers::NONE),
                    layout,
                    Overlay::None,
                    Surface::Channel,
                    SearchMode::Results,
                )
            };
            assert_eq!(
                press(KeyCode::Char('c')),
                Action::ToggleSwitcher,
                "{layout:?}"
            );
            assert_eq!(press(KeyCode::Char('j')), Action::NextRow, "{layout:?}");
            assert_eq!(press(KeyCode::Char('k')), Action::PrevRow, "{layout:?}");
        }
    }

    #[test]
    fn an_open_switcher_takes_the_selection_keys_and_isolates_the_rest() {
        let switcher = |code: KeyCode| {
            map_navigation(
                key(code, KeyModifiers::NONE),
                LayoutMode::Wide,
                Overlay::Switcher,
                Surface::Channel,
                SearchMode::Results,
            )
        };
        assert_eq!(switcher(KeyCode::Char('j')), Action::SwitcherNext);
        assert_eq!(switcher(KeyCode::Up), Action::SwitcherPrev);
        assert_eq!(switcher(KeyCode::Enter), Action::SwitcherConfirm);
        assert_eq!(switcher(KeyCode::Esc), Action::Dismiss);
        assert_eq!(switcher(KeyCode::Char('c')), Action::Dismiss);
        assert_eq!(switcher(KeyCode::Char('q')), Action::Quit);
        assert_eq!(switcher(KeyCode::Char('f')), Action::FilterNext);
        assert_eq!(switcher(KeyCode::Tab), Action::FilterNext);
        assert_eq!(switcher(KeyCode::Char('?')), Action::ToggleHelp);
        // Anything else printable is the local name filter; the held list
        // answers typing instead of acting on the conversation behind it.
        assert_eq!(switcher(KeyCode::Char('i')), Action::SwitcherInput('i'));
        assert_eq!(switcher(KeyCode::Char('r')), Action::SwitcherInput('r'));
        assert_eq!(switcher(KeyCode::Backspace), Action::SwitcherBackspace);
    }

    #[test]
    fn a_too_small_terminal_only_answers_q_and_ctrl_c() {
        let small = |code: KeyCode, modifiers| {
            map_navigation(
                key(code, modifiers),
                LayoutMode::TooSmall,
                Overlay::None,
                Surface::Channel,
                SearchMode::Results,
            )
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
                Overlay::None,
                Surface::Channel,
                SearchMode::Results,
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
                Overlay::Help,
                Surface::Channel,
                SearchMode::Results,
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
    fn a_opens_the_agents_view_in_either_layout() {
        assert_eq!(wide(KeyCode::Char('a')), Action::ToggleAgents);
        assert_eq!(narrow(KeyCode::Char('a')), Action::ToggleAgents);
    }

    #[test]
    fn an_open_agents_view_takes_the_selection_keys_and_isolates_the_rest() {
        assert_eq!(agents(KeyCode::Char('j')), Action::AgentsNext);
        assert_eq!(agents(KeyCode::Down), Action::AgentsNext);
        assert_eq!(agents(KeyCode::Char('k')), Action::AgentsPrev);
        assert_eq!(agents(KeyCode::Up), Action::AgentsPrev);
        assert_eq!(agents(KeyCode::Enter), Action::AgentsConfirm);
        assert_eq!(agents(KeyCode::Esc), Action::Dismiss);
        assert_eq!(agents(KeyCode::Char('a')), Action::ToggleAgents);
        assert_eq!(agents(KeyCode::Char('q')), Action::Quit);
        // A conversation sits behind the overlay: none of its keys fire.
        assert_eq!(agents(KeyCode::Char('i')), Action::Ignored);
        assert_eq!(agents(KeyCode::Char('r')), Action::Ignored);
        assert_eq!(agents(KeyCode::Char('3')), Action::Ignored);
    }

    #[test]
    fn the_switcher_cycles_the_filter_and_the_timeline_does_too() {
        let switcher = |code: KeyCode| {
            map_navigation(
                key(code, KeyModifiers::NONE),
                LayoutMode::Wide,
                Overlay::Switcher,
                Surface::Channel,
                SearchMode::Results,
            )
        };
        assert_eq!(switcher(KeyCode::Char('f')), Action::FilterNext);
        assert_eq!(switcher(KeyCode::Tab), Action::FilterNext);
        assert_eq!(wide(KeyCode::Char('f')), Action::FilterNext);
    }

    #[test]
    fn ctrl_f_opens_search_over_the_channel_timeline_only() {
        assert_eq!(
            map_navigation(
                key(KeyCode::Char('f'), KeyModifiers::CONTROL),
                LayoutMode::Wide,
                Overlay::None,
                Surface::Channel,
                SearchMode::Results,
            ),
            Action::OpenSearch
        );
        // The palette's own shortcut reaches search too: it is the command
        // the list offers, and opening search closes the palette.
        assert_eq!(
            map_navigation(
                key(KeyCode::Char('f'), KeyModifiers::CONTROL),
                LayoutMode::Wide,
                Overlay::Palette,
                Surface::Channel,
                SearchMode::Results,
            ),
            Action::OpenSearch
        );
        // Every other overlay keeps its keys.
        for overlay in [
            Overlay::Switcher,
            Overlay::SwitcherSearch,
            Overlay::CommunityPicker,
            Overlay::Help,
            Overlay::Agents,
        ] {
            assert_eq!(
                map_navigation(
                    key(KeyCode::Char('f'), KeyModifiers::CONTROL),
                    LayoutMode::Wide,
                    overlay,
                    Surface::Channel,
                    SearchMode::Results,
                ),
                Action::Ignored,
                "{overlay:?}"
            );
        }
        assert_eq!(
            map_navigation(
                key(KeyCode::Char('f'), KeyModifiers::CONTROL),
                LayoutMode::TooSmall,
                Overlay::None,
                Surface::Channel,
                SearchMode::Results,
            ),
            Action::Ignored
        );
        // `/` remains the search entry on the thread surface, where the
        // destination is the thread itself.
        assert_eq!(
            map_navigation(
                key(KeyCode::Char('/'), KeyModifiers::NONE),
                LayoutMode::Wide,
                Overlay::None,
                Surface::Thread,
                SearchMode::Results,
            ),
            Action::OpenSearch
        );
    }

    #[test]
    fn ctrl_p_opens_the_palette_over_the_channel_timeline_only() {
        for layout in [LayoutMode::Wide, LayoutMode::Narrow] {
            assert_eq!(
                map_navigation(
                    key(KeyCode::Char('p'), KeyModifiers::CONTROL),
                    layout,
                    Overlay::None,
                    Surface::Channel,
                    SearchMode::Results,
                ),
                Action::TogglePalette,
                "{layout:?}"
            );
        }
        // A second press closes the palette it opened, like its own name says.
        assert_eq!(
            map_navigation(
                key(KeyCode::Char('p'), KeyModifiers::CONTROL),
                LayoutMode::Wide,
                Overlay::Palette,
                Surface::Channel,
                SearchMode::Results,
            ),
            Action::TogglePalette
        );
        // An overlay owns its keys: the palette must not open over the
        // switcher (its query would be destroyed), help or the Agents list.
        for overlay in [
            Overlay::Switcher,
            Overlay::SwitcherSearch,
            Overlay::Help,
            Overlay::Agents,
        ] {
            assert_eq!(
                map_navigation(
                    key(KeyCode::Char('p'), KeyModifiers::CONTROL),
                    LayoutMode::Wide,
                    overlay,
                    Surface::Channel,
                    SearchMode::Results,
                ),
                Action::Ignored,
                "{overlay:?}"
            );
        }
        // The size message answers `q` alone, whatever else is pressed with
        // Ctrl.
        assert_eq!(
            map_navigation(
                key(KeyCode::Char('p'), KeyModifiers::CONTROL),
                LayoutMode::TooSmall,
                Overlay::None,
                Surface::Channel,
                SearchMode::Results,
            ),
            Action::Ignored
        );
        // A surface that carries a destination of its own keeps it.
        for surface in [
            Surface::Thread,
            Surface::Search,
            Surface::Context,
            Surface::Reader,
        ] {
            assert_eq!(
                map_navigation(
                    key(KeyCode::Char('p'), KeyModifiers::CONTROL),
                    LayoutMode::Wide,
                    Overlay::None,
                    surface,
                    SearchMode::Results,
                ),
                Action::Ignored,
                "{surface:?}"
            );
        }
        // The composer keeps its draft: Ctrl+P is not a composer key.
        assert_eq!(
            map_composer(key(KeyCode::Char('p'), KeyModifiers::CONTROL), false),
            Action::Ignored
        );
    }

    #[test]
    fn an_open_palette_takes_the_selection_keys_and_isolates_the_rest() {
        let palette = |code: KeyCode| {
            map_navigation(
                key(code, KeyModifiers::NONE),
                LayoutMode::Wide,
                Overlay::Palette,
                Surface::Channel,
                SearchMode::Results,
            )
        };
        assert_eq!(palette(KeyCode::Char('j')), Action::PaletteNext);
        assert_eq!(palette(KeyCode::Down), Action::PaletteNext);
        assert_eq!(palette(KeyCode::Char('k')), Action::PalettePrev);
        assert_eq!(palette(KeyCode::Up), Action::PalettePrev);
        assert_eq!(palette(KeyCode::Enter), Action::PaletteConfirm);
        assert_eq!(palette(KeyCode::Esc), Action::Dismiss);
        assert_eq!(palette(KeyCode::Char('?')), Action::ToggleHelp);
        assert_eq!(palette(KeyCode::Char('q')), Action::Quit);
        // Typing is not a palette key: the command list is fixed.
        assert_eq!(palette(KeyCode::Char('s')), Action::Ignored);
        assert_eq!(palette(KeyCode::Char('c')), Action::Ignored);
    }

    #[test]
    fn ctrl_c_quits_from_either_mode() {
        assert_eq!(
            map_navigation(
                key(KeyCode::Char('c'), KeyModifiers::CONTROL),
                LayoutMode::Wide,
                Overlay::None,
                Surface::Channel,
                SearchMode::Results,
            ),
            Action::Quit
        );
        assert_eq!(
            map_composer(key(KeyCode::Char('c'), KeyModifiers::CONTROL), false),
            Action::Quit
        );
    }

    #[test]
    fn composer_text_needs_no_modifiers() {
        assert_eq!(
            map_composer(key(KeyCode::Char('x'), KeyModifiers::NONE), false),
            Action::ComposerInput('x')
        );
        assert_eq!(
            map_composer(key(KeyCode::Char('v'), KeyModifiers::NONE), false),
            Action::ComposerInput('v')
        );
        // Alt+x is not text; terminals use it for commands.
        assert_eq!(
            map_composer(key(KeyCode::Char('x'), KeyModifiers::ALT), false),
            Action::Ignored
        );
    }

    #[test]
    fn alt_enter_inserts_a_newline_and_enter_sends() {
        assert_eq!(
            map_composer(key(KeyCode::Enter, KeyModifiers::ALT), false),
            Action::ComposerNewline
        );
        assert_eq!(
            map_composer(key(KeyCode::Enter, KeyModifiers::NONE), false),
            Action::ComposerSend
        );
    }

    #[test]
    fn composer_editing_keys_map_to_cursor_actions() {
        assert_eq!(
            map_composer(key(KeyCode::Backspace, KeyModifiers::NONE), false),
            Action::ComposerBackspace
        );
        assert_eq!(
            map_composer(key(KeyCode::Left, KeyModifiers::NONE), false),
            Action::ComposerCursorLeft
        );
        assert_eq!(
            map_composer(key(KeyCode::Right, KeyModifiers::NONE), false),
            Action::ComposerCursorRight
        );
        assert_eq!(
            map_composer(key(KeyCode::Home, KeyModifiers::NONE), false),
            Action::ComposerHome
        );
        assert_eq!(
            map_composer(key(KeyCode::End, KeyModifiers::NONE), false),
            Action::ComposerEnd
        );
    }

    #[test]
    fn an_open_picker_owns_the_composers_navigation_keys() {
        let open = |code, modifiers| map_composer(key(code, modifiers), true);
        assert_eq!(
            open(KeyCode::Enter, KeyModifiers::NONE),
            Action::PickerConfirm
        );
        assert_eq!(open(KeyCode::Esc, KeyModifiers::NONE), Action::Dismiss);
        assert_eq!(open(KeyCode::Up, KeyModifiers::NONE), Action::PickerPrev);
        assert_eq!(open(KeyCode::Down, KeyModifiers::NONE), Action::PickerNext);
        assert_eq!(open(KeyCode::Tab, KeyModifiers::NONE), Action::PickerNext);
        assert_eq!(
            open(KeyCode::BackTab, KeyModifiers::NONE),
            Action::PickerPrev
        );
        // A terminal reports Shift+Tab as BackTab with SHIFT, not as a bare
        // BackTab: the key that moves backwards must still reach the list.
        assert_eq!(
            open(KeyCode::BackTab, KeyModifiers::SHIFT),
            Action::PickerPrev
        );
        assert_eq!(
            open(KeyCode::Char('r'), KeyModifiers::CONTROL),
            Action::PickerRetry
        );
        // Text keeps its keys, including the two a list would use as movement:
        // a name may contain `j` or `k`.
        assert_eq!(
            open(KeyCode::Char('j'), KeyModifiers::NONE),
            Action::ComposerInput('j')
        );
        assert_eq!(
            open(KeyCode::Char('k'), KeyModifiers::NONE),
            Action::ComposerInput('k')
        );
        assert_eq!(
            open(KeyCode::Char('J'), KeyModifiers::NONE),
            Action::ComposerInput('J')
        );
        assert_eq!(
            open(KeyCode::Enter, KeyModifiers::ALT),
            Action::ComposerNewline
        );
    }

    #[test]
    fn the_create_form_maps_its_own_keys() {
        let form = |code| {
            map_navigation(
                key(code, KeyModifiers::NONE),
                LayoutMode::Wide,
                Overlay::CreateChannel,
                Surface::Channel,
                SearchMode::Results,
            )
        };
        assert_eq!(form(KeyCode::Tab), Action::ComposerCursorDown);
        assert_eq!(form(KeyCode::BackTab), Action::ComposerCursorUp);
        assert_eq!(form(KeyCode::Left), Action::ComposerCursorLeft);
        assert_eq!(form(KeyCode::Right), Action::ComposerCursorRight);
        assert_eq!(form(KeyCode::Char(' ')), Action::ComposerInput(' '));
        assert_eq!(form(KeyCode::Char('q')), Action::ComposerInput('q'));
        assert_eq!(form(KeyCode::Char('项')), Action::ComposerInput('项'));
        assert_eq!(form(KeyCode::Enter), Action::ComposerSend);
        assert_eq!(form(KeyCode::Esc), Action::Dismiss);
        // Ctrl+R asks for the roster answer that settles an unresolved
        // creation, and works from the form or from the timeline the form
        // left behind when it closed.
        assert_eq!(
            map_navigation(
                key(KeyCode::Char('r'), KeyModifiers::CONTROL),
                LayoutMode::Wide,
                Overlay::CreateChannel,
                Surface::Channel,
                SearchMode::Results,
            ),
            Action::CreateChannelRefresh
        );
        assert_eq!(
            map_navigation(
                key(KeyCode::Char('r'), KeyModifiers::CONTROL),
                LayoutMode::Wide,
                Overlay::None,
                Surface::Channel,
                SearchMode::Results,
            ),
            Action::CreateChannelRefresh
        );
        // Other overlays keep their own keys.
        assert_eq!(
            map_navigation(
                key(KeyCode::Char('r'), KeyModifiers::CONTROL),
                LayoutMode::Wide,
                Overlay::Palette,
                Surface::Channel,
                SearchMode::Results,
            ),
            Action::Ignored
        );
    }
}
