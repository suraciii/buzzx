//! Slash commands in the composer: recognition, argument checks, and the
//! command metadata the palette and the suggestion list share.
//!
//! A draft is a command candidate only when its first character is `/`, it
//! is one line, and it does not start with `//`. Everything else is plain
//! text, so a `/` in prose, a URL, or code is never intercepted. `//` sends
//! the text with one leading slash; an unknown name falls back to plain
//! text; a known command with bad arguments blocks the send so the input
//! can be fixed. The parser is pure: surface and permission checks belong
//! to the caller, and arguments stay raw text with no quoting or options.

/// A command the composer or the palette can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    Help,
    Search,
    Switch,
    Thread,
    Agents,
    Status,
    /// Palette only. Quitting duplicates `q`/`Esc`, so the composer never
    /// recognizes it and `/quit` stays plain text.
    Quit,
}

/// The most suggestions the composer shows at once; more would crowd out
/// the timeline on the smallest layouts. It is applied after the caller
/// drops the commands the current surface refuses, so the cap never hides
/// the command the reader is looking for.
pub const MAX_SUGGESTIONS: usize = 5;

impl Command {
    /// Every slash command, in registry order. `Quit` is excluded on
    /// purpose: the parser and the suggestion list iterate this set.
    pub const ALL: [Command; 6] = [
        Command::Help,
        Command::Search,
        Command::Switch,
        Command::Thread,
        Command::Agents,
        Command::Status,
    ];

    /// The palette's action directory, in display order. The palette runs
    /// actions the timeline already has a key for, so it lists `Quit` even
    /// though the composer does not.
    pub const PALETTE: [Command; 7] = [
        Command::Switch,
        Command::Search,
        Command::Agents,
        Command::Thread,
        Command::Help,
        Command::Status,
        Command::Quit,
    ];

    /// The canonical lowercase name, without the leading slash.
    pub fn name(self) -> &'static str {
        match self {
            Command::Help => "help",
            Command::Search => "search",
            Command::Switch => "switch",
            Command::Thread => "thread",
            Command::Agents => "agents",
            Command::Status => "status",
            Command::Quit => "quit",
        }
    }

    /// The invocation shape, shown where a rejected argument is explained
    /// and in help.
    pub fn usage(self) -> &'static str {
        match self {
            Command::Help => "/help [command]",
            Command::Search => "/search [query]",
            Command::Switch => "/switch [name]",
            Command::Thread => "/thread",
            Command::Agents => "/agents",
            Command::Status => "/status",
            Command::Quit => "q",
        }
    }

    /// One line of intent for the suggestion list and help.
    pub fn summary(self) -> &'static str {
        match self {
            Command::Help => "Show help, focused on one command when named",
            Command::Search => "Search messages",
            Command::Switch => "Switch conversation",
            Command::Thread => "Open the focused event's thread",
            Command::Agents => "Open the Agents list",
            Command::Status => "Show connection and target status",
            Command::Quit => "Quit",
        }
    }

    /// The palette row's display text.
    pub fn label(self) -> &'static str {
        match self {
            Command::Help => "Help",
            Command::Search => "Search messages",
            Command::Switch => "Switch conversation",
            Command::Thread => "Open thread",
            Command::Agents => "My agents",
            Command::Status => "Status",
            Command::Quit => "Quit",
        }
    }

    /// The existing key that runs the same action, shown beside the palette
    /// label. `Status` has no key of its own yet, so its row shows none.
    pub fn key(self) -> &'static str {
        match self {
            Command::Help => "?",
            Command::Search => "/",
            Command::Switch => "c",
            Command::Thread => "t",
            Command::Agents => "a",
            Command::Status => "",
            Command::Quit => "q",
        }
    }

    /// The surfaces this command runs on, named the way the composer names
    /// its own surface. A surface that is not listed refuses the command
    /// with a reason instead of re-routing it somewhere the reader did not
    /// ask for; the suggestion list and the command help read the same list.
    pub fn surfaces(self) -> &'static [&'static str] {
        match self {
            Command::Help | Command::Status | Command::Quit => {
                &["channel", "context", "thread", "reader"]
            }
            Command::Search => &["channel", "context", "thread"],
            Command::Thread => &["channel", "context"],
            Command::Switch | Command::Agents => &["channel"],
        }
    }

    /// The command a full name token resolves to. Matching is exact and
    /// case-sensitive: a prefix like `sea` is a suggestion, never an
    /// invocation, and `Quit` is unreachable from the composer.
    fn from_name(token: &str) -> Option<Command> {
        Command::ALL
            .iter()
            .copied()
            .find(|command| command.name() == token)
    }
}

/// What a composer draft turns out to be once checked against the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parse<'a> {
    /// Not a command at all: no leading `/`, or the draft spans lines.
    Plain,
    /// `//` escape: send this text, which keeps one leading slash.
    Escaped(&'a str),
    /// A leading `/` whose name token is no full command. Sent as plain
    /// text; the UI may hint but never blocks.
    Unknown,
    /// A known command whose arguments fail its shape. The send is blocked
    /// and the draft kept so the input can be fixed.
    Invalid(Command),
    /// A recognized command and its raw argument text, possibly empty.
    Known(Command, &'a str),
}

/// Recognize a composer draft. Only the first character decides whether a
/// draft is a command candidate, and a multi-line draft never is one.
pub fn parse(input: &str) -> Parse<'_> {
    let Some(rest) = slash_candidate(input) else {
        return Parse::Plain;
    };
    if rest.starts_with('/') {
        return Parse::Escaped(rest);
    }
    let (token, args) = split_invocation(rest);
    let Some(command) = Command::from_name(token) else {
        return Parse::Unknown;
    };
    match command {
        Command::Thread | Command::Agents | Command::Status if !args.is_empty() => {
            Parse::Invalid(command)
        }
        Command::Help if !args.is_empty() => {
            // `/help` takes at most one argument and it must name a known
            // command; anything else would focus a help entry that does not
            // exist.
            if args.contains(char::is_whitespace) || Command::from_name(args).is_none() {
                Parse::Invalid(Command::Help)
            } else {
                Parse::Known(Command::Help, args)
            }
        }
        _ => Parse::Known(command, args),
    }
}

/// The commands a draft could still become, best match first: exact name,
/// then name prefix, then substring, with ties in registry order. A draft
/// that already holds a full command token suggests only that command, so
/// its usage stays on screen while arguments are typed. Every match is
/// returned: the caller drops the commands its surface refuses and applies
/// [`MAX_SUGGESTIONS`] last, so the list never offers what `Enter` would
/// reject.
pub fn suggestions(input: &str) -> Vec<Command> {
    let Some(rest) = slash_candidate(input) else {
        return Vec::new();
    };
    if rest.starts_with('/') {
        return Vec::new();
    }
    let (token, _) = split_invocation(rest);
    if token.len() < rest.len() {
        return Command::from_name(token).into_iter().collect();
    }
    let rank = |command: Command| {
        let name = command.name();
        if token.is_empty() || name == token {
            Some(0)
        } else if name.starts_with(token) {
            Some(1)
        } else if name.contains(token) {
            Some(2)
        } else {
            None
        }
    };
    // The iteration is already in registry order and the sort is stable, so
    // equal ranks keep registry order.
    let mut ranked: Vec<(u8, Command)> = Command::ALL
        .into_iter()
        .filter_map(|command| rank(command).map(|rank| (rank, command)))
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, command)| command).collect()
}

/// The draft without its leading `/`, when the draft is a single-line slash
/// candidate. The caller still has to distinguish the `//` escape.
fn slash_candidate(input: &str) -> Option<&str> {
    if input.contains('\n') {
        return None;
    }
    input.strip_prefix('/')
}

/// The name token and the raw arguments of a slash line, split at the first
/// whitespace run. Arguments keep their inner and trailing spacing; only
/// the separating whitespace is dropped.
fn split_invocation(rest: &str) -> (&str, &str) {
    match rest.find(char::is_whitespace) {
        Some(at) => (&rest[..at], rest[at..].trim_start()),
        None => (rest, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_never_a_command() {
        assert_eq!(parse(""), Parse::Plain);
        assert_eq!(parse("hello"), Parse::Plain);
        assert_eq!(parse("see https://example.test/a/b"), Parse::Plain);
        assert_eq!(parse("Use /search when reviewing this run."), Parse::Plain);
        assert_eq!(parse("code: /usr/bin/env bash"), Parse::Plain);
    }

    #[test]
    fn multiline_drafts_are_plain() {
        assert_eq!(parse("/search one\ntwo"), Parse::Plain);
        assert_eq!(parse("one\n/search two"), Parse::Plain);
        assert_eq!(parse("//escape\n/second"), Parse::Plain);
    }

    #[test]
    fn double_slash_escapes_to_plain_text() {
        assert_eq!(
            parse("//search old decision"),
            Parse::Escaped("/search old decision")
        );
        assert_eq!(parse("//"), Parse::Escaped("/"));
        assert_eq!(parse("///x"), Parse::Escaped("//x"));
    }

    #[test]
    fn unknown_or_partial_names_send_as_text() {
        assert_eq!(parse("/"), Parse::Unknown);
        assert_eq!(parse("/sea"), Parse::Unknown);
        assert_eq!(parse("/foo"), Parse::Unknown);
        assert_eq!(parse("/foo bar"), Parse::Unknown);
        assert_eq!(parse("/Search"), Parse::Unknown);
        assert_eq!(parse("/搜索"), Parse::Unknown);
        assert_eq!(parse("/quit"), Parse::Unknown);
    }

    #[test]
    fn free_text_arguments_stay_raw() {
        assert_eq!(parse("/search"), Parse::Known(Command::Search, ""));
        assert_eq!(
            parse("/search old decision"),
            Parse::Known(Command::Search, "old decision")
        );
        assert_eq!(
            parse("/search  spaced   out "),
            Parse::Known(Command::Search, "spaced   out ")
        );
        assert_eq!(parse("/switch"), Parse::Known(Command::Switch, ""));
        assert_eq!(parse("/switch rel"), Parse::Known(Command::Switch, "rel"));
        assert_eq!(
            parse("/search 中文 查询"),
            Parse::Known(Command::Search, "中文 查询")
        );
        assert_eq!(
            parse("/search emoji 🔍 done"),
            Parse::Known(Command::Search, "emoji 🔍 done")
        );
    }

    #[test]
    fn long_arguments_are_kept_whole() {
        let long = "x".repeat(4096);
        let input = format!("/search {long}");
        assert_eq!(parse(&input), Parse::Known(Command::Search, long.as_str()));
    }

    #[test]
    fn no_argument_commands_reject_arguments() {
        for command in [Command::Thread, Command::Agents, Command::Status] {
            let name = command.name();
            assert_eq!(parse(&format!("/{name}")), Parse::Known(command, ""));
            assert_eq!(parse(&format!("/{name}   ")), Parse::Known(command, ""));
            assert_eq!(
                parse(&format!("/{name} extra")),
                Parse::Invalid(command),
                "{name} takes no arguments"
            );
        }
    }

    #[test]
    fn help_takes_zero_or_one_known_command() {
        assert_eq!(parse("/help"), Parse::Known(Command::Help, ""));
        assert_eq!(parse("/help search"), Parse::Known(Command::Help, "search"));
        assert_eq!(parse("/help foo"), Parse::Invalid(Command::Help));
        assert_eq!(parse("/help search extra"), Parse::Invalid(Command::Help));
        // `quit` is palette-only, so help cannot focus it.
        assert_eq!(parse("/help quit"), Parse::Invalid(Command::Help));
    }

    #[test]
    fn suggestions_need_a_single_line_slash_draft() {
        assert!(suggestions("").is_empty());
        assert!(suggestions("hello").is_empty());
        assert!(suggestions("see https://example.test/a/b").is_empty());
        assert!(suggestions("//search").is_empty());
        assert!(suggestions("/search one\ntwo").is_empty());
    }

    #[test]
    fn suggestions_rank_exact_prefix_substring() {
        assert_eq!(suggestions("/search"), vec![Command::Search]);
        assert_eq!(suggestions("/sea"), vec![Command::Search]);
        assert_eq!(
            suggestions("/s"),
            vec![
                Command::Search,
                Command::Switch,
                Command::Status,
                Command::Agents,
            ]
        );
        assert_eq!(
            suggestions("/t"),
            vec![
                Command::Thread,
                Command::Switch,
                Command::Agents,
                Command::Status
            ]
        );
        assert_eq!(suggestions("/foo"), Vec::<Command>::new());
    }

    #[test]
    fn bare_slash_lists_the_whole_registry_in_order() {
        assert_eq!(suggestions("/"), &Command::ALL[..]);
        assert!(suggestions("/").len() > MAX_SUGGESTIONS);
    }

    #[test]
    fn suggestions_follow_a_full_command_token() {
        assert_eq!(suggestions("/search"), vec![Command::Search]);
        assert_eq!(suggestions("/search "), vec![Command::Search]);
        assert_eq!(suggestions("/search old"), vec![Command::Search]);
        assert!(suggestions("/foo bar").is_empty());
    }

    #[test]
    fn registry_metadata_is_complete() {
        assert_eq!(Command::ALL.len(), 6);
        for command in Command::ALL {
            assert!(!command.name().is_empty());
            assert!(command.usage().starts_with('/'));
            assert!(!command.summary().is_empty());
        }
        let mut names: Vec<&str> = Command::ALL.iter().map(|command| command.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            Command::ALL.len(),
            "command names must be unique"
        );

        assert!(!Command::ALL.contains(&Command::Quit));
        assert!(Command::PALETTE.contains(&Command::Quit));
        for command in Command::PALETTE {
            assert!(!command.label().is_empty());
            assert!(!command.summary().is_empty());
        }

        for command in Command::PALETTE {
            assert!(
                !command.surfaces().is_empty(),
                "{} names no surface",
                command.name()
            );
            assert!(
                command.surfaces().contains(&"channel"),
                "{} must run on the composer's home surface",
                command.name()
            );
        }
    }
}
