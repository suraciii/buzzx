//! Terminal setup, the event loop, and teardown. The loop shape is fixed in
//! design/architecture.md: drain events, draw, poll a key, apply, repeat.

mod app;
mod config;
mod content;
mod http;
mod keys;
mod login;
mod session;
mod sub;
mod ui;

use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use crossterm::event::{Event as TermEvent, KeyEventKind};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use uuid::Uuid;

use app::App;
use config::Resolved;

/// The poll timeout. Faster wastes frames; slower makes the session feel
/// dead while a key is held.
const TICK: Duration = Duration::from_millis(100);

#[derive(Parser)]
#[command(name = "buzzx", about = "Terminal client for Buzz channel chat")]
struct Cli {
    /// Relay base URL. Overrides BUZZ_RELAY_URL and the config file.
    #[arg(long, global = true)]
    relay: Option<String>,
    /// Identity key, hex or nsec. Overrides BUZZ_PRIVATE_KEY and the config
    /// file.
    #[arg(long, global = true)]
    private_key: Option<String>,
    /// NIP-OA auth tag JSON. Overrides BUZZ_AUTH_TAG and the config file.
    #[arg(long, global = true)]
    auth_tag: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Open the interactive terminal chat session.
    Tui,
    /// Stream one channel's live events as JSON lines.
    Watch {
        /// Channel UUID to watch.
        channel: String,
    },
    /// Log in: verify an identity against the relay and save it as the
    /// config file. Interactive when no key source is given.
    Login {
        /// Read the key from this file. The file must be readable by the
        /// current user alone (0600).
        #[arg(long)]
        private_key_file: Option<PathBuf>,
        /// Read the key from stdin. Hidden when stdin is a terminal.
        #[arg(long)]
        private_key_stdin: bool,
    },
    /// Write the identity and relay to the config file.
    Init {
        /// Relay base URL.
        #[arg(long)]
        relay: String,
        /// Identity key, hex or nsec.
        #[arg(long)]
        private_key: String,
        /// NIP-OA auth tag JSON.
        #[arg(long)]
        auth_tag: Option<String>,
    },
}

fn main() {
    // Before any TLS use: a build that unifies ring and aws-lc-rs panics
    // inside rustls without an explicit provider.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cli = Cli::parse();
    let code: i32 = match cli.command {
        Command::Login {
            private_key_file,
            private_key_stdin,
        } => login::run(login::LoginCli {
            flag_key: cli.private_key.clone(),
            flag_relay: cli.relay.clone(),
            flag_auth_tag: cli.auth_tag.clone(),
            key_file: private_key_file,
            key_stdin: private_key_stdin,
        }),
        Command::Init {
            relay,
            private_key,
            auth_tag,
        } => match config::init(&relay, &private_key, auth_tag.as_deref()) {
            Ok(path) => {
                println!("wrote {}", path.display());
                0
            }
            Err(e) => {
                eprintln!("buzzx: {e}");
                e.code
            }
        },
        Command::Watch { channel } => match Uuid::parse_str(&channel) {
            Err(_) => {
                eprintln!("buzzx: channel must be a UUID");
                config::EXIT_USAGE
            }
            Ok(channel) => {
                match config::resolve(
                    cli.private_key.as_deref(),
                    cli.relay.as_deref(),
                    cli.auth_tag.as_deref(),
                ) {
                    Ok(resolved) => {
                        let runtime = tokio::runtime::Builder::new_multi_thread()
                            .enable_all()
                            .build()
                            .expect("tokio runtime");
                        runtime.block_on(session::run_watch(&resolved, channel))
                    }
                    Err(e) => {
                        eprintln!("buzzx: {e}");
                        e.code
                    }
                }
            }
        },
        Command::Tui => run_tui(&cli),
    };
    std::process::exit(code);
}

fn run_tui(cli: &Cli) -> i32 {
    if std::env::var("TERM").as_deref() == Ok("dumb") {
        eprintln!("buzzx: TERM=dumb cannot run the TUI");
        return config::EXIT_USAGE;
    }
    let resolved = match config::resolve(
        cli.private_key.as_deref(),
        cli.relay.as_deref(),
        cli.auth_tag.as_deref(),
    ) {
        Ok(resolved) => resolved,
        Err(error) => {
            eprintln!("buzzx: {error}");
            return error.code;
        }
    };
    match run_tui_session(resolved) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("buzzx: {error}");
            config::EXIT_OTHER
        }
    }
}

/// Enters raw mode and the alternate screen, and restores both on every
/// exit path, including panics: a terminal left in raw mode is a bug.
struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(io::stdout(), crossterm::terminal::EnterAlternateScreen)?;
        Ok(Self { active: true })
    }

    fn restore(&mut self) {
        if self.active {
            self.active = false;
            let _ = crossterm::terminal::disable_raw_mode();
            let _ = crossterm::execute!(io::stdout(), crossterm::terminal::LeaveAlternateScreen);
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn run_tui_session(resolved: Resolved) -> Result<i32, String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;

    let mut guard = TerminalGuard::enter().map_err(|e| format!("terminal setup failed: {e}"))?;
    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal =
        Terminal::new(backend).map_err(|e| format!("terminal backend failed: {e}"))?;

    let result: Result<i32, String> = runtime.block_on(async move {
        let (events_tx, mut events_rx) = tokio::sync::mpsc::channel(256);
        let session = session::spawn(&resolved, events_tx);
        let session = session;

        let mut app = App::new(&resolved.keys, &resolved.http_url);
        let mut started = Some(session.started);

        loop {
            // 1. Drain every queued event before drawing, so a burst renders
            //    once.
            while let Ok(event) = events_rx.try_recv() {
                app.apply(event, now_secs());
            }
            // The first connection decides whether the session can run at
            // all: unreachable is 2, an auth refusal is 3.
            if let Some(receiver) = started.as_mut() {
                match receiver.try_recv() {
                    Ok(Ok(())) => started = None,
                    Ok(Err((code, reason))) => {
                        app.quit = true;
                        app.exit_code = code;
                        app.status = reason;
                    }
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => started = None,
                }
            }
            for command in app.take_outbox() {
                let _ = session.commands.send(command).await;
            }

            // 2. Draw.
            terminal
                .draw(|frame| ui::draw(frame, &app, now_secs()))
                .map_err(|e| e.to_string())?;

            // 3. Poll for a key with the tick timeout, then apply it. The
            //    blocking poll only delays this frame; the session tasks run
            //    on the runtime's worker threads.
            let key = if crossterm::event::poll(TICK).unwrap_or(false) {
                match crossterm::event::read() {
                    Ok(TermEvent::Key(key)) if key.kind == KeyEventKind::Press => Some(key),
                    _ => None,
                }
            } else {
                None
            };
            if let Some(key) = key {
                let action = match app.mode {
                    app::Mode::Navigation => keys::map_navigation(key),
                    app::Mode::Composer => keys::map_composer(key),
                };
                app.handle(action, now_secs());
            }

            if app.quit {
                let _ = session
                    .commands
                    .send(session::SessionCommand::Shutdown)
                    .await;
                return Ok(app.exit_code);
            }
        }
    });

    guard.restore();
    let code = result?;
    Ok(code)
}
