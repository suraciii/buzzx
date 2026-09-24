//! `buzzx login`: identity onboarding. The contract is docs/configuration.md:
//! the key comes from a flag, a 0600 file, stdin, the environment, or an
//! interactive wizard; the identity is verified against the relay before the
//! config file is replaced atomically. A secret never reaches stdout, stderr,
//! logs, or an error message.

use std::io::{self, BufRead, IsTerminal, Read, Write as _};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use buzz_ws_client::{NostrWsConnection, WsClientError};
use nostr::Keys;

use crate::config::{self, ConfigFile, StartupError};

/// How long the hidden reader waits for further input before deciding the
/// paste is over and the leftover bytes can be discarded.
const DRAIN_IDLE_MS: std::time::Duration = std::time::Duration::from_millis(50);

/// Where the login key comes from, in precedence order. The three explicit
/// forms are mutually exclusive; the environment and the wizard follow.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyInput {
    Flag(String),
    File(PathBuf),
    Stdin,
    Env,
    Wizard,
}

/// What `run` decided to do about input, before doing any I/O. Separated so
/// the decision is testable without a terminal.
#[derive(Debug, Clone, PartialEq)]
pub enum InputPlan {
    Use(KeyInput),
    /// No key anywhere and no terminal to ask on.
    RefuseNoInput,
}

pub fn plan_input(
    flag: Option<&str>,
    file: Option<&Path>,
    stdin: bool,
    env_set: bool,
    tty: bool,
) -> InputPlan {
    if stdin {
        return InputPlan::Use(KeyInput::Stdin);
    }
    if let Some(path) = file {
        return InputPlan::Use(KeyInput::File(path.to_path_buf()));
    }
    if let Some(key) = flag {
        return InputPlan::Use(KeyInput::Flag(key.to_owned()));
    }
    if env_set {
        return InputPlan::Use(KeyInput::Env);
    }
    if tty {
        return InputPlan::Use(KeyInput::Wizard);
    }
    InputPlan::RefuseNoInput
}

/// Everything `buzzx login` needs from the command line and the environment.
#[derive(Debug, Default)]
pub struct LoginCli {
    pub flag_key: Option<String>,
    pub flag_relay: Option<String>,
    pub flag_auth_tag: Option<String>,
    pub key_file: Option<PathBuf>,
    pub key_stdin: bool,
}

/// Parse and validate key material. The error message names the failure,
/// never the key.
pub fn validate_key(raw: &str) -> Result<Keys, StartupError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(StartupError::auth("private key is empty"));
    }
    Keys::parse(trimmed).map_err(|_| {
        StartupError::auth("invalid private key: expected 64 hex characters or nsec1...")
    })
}

/// Read the key from a file the current user alone may read. The permission
/// check comes first: a group-readable key file is refused before its bytes
/// are touched, and before any network call.
pub fn read_key_file(path: &Path) -> Result<String, StartupError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(_) => {
            return Err(StartupError::usage(format!(
                "cannot read key file {}: no such file",
                path.display()
            )));
        }
    };
    if !meta.is_file() {
        return Err(StartupError::usage(format!(
            "key file {} is not a regular file",
            path.display()
        )));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(StartupError::auth(format!(
            "key file {} is readable by other users; run chmod 600 on it",
            path.display()
        )));
    }
    std::fs::read_to_string(path)
        .map_err(|e| StartupError::usage(format!("cannot read key file {}: {e}", path.display())))
}

/// Read the key from stdin: hidden when stdin is a terminal, one line
/// otherwise. Reading stops at the first line; a pasted remainder is drained
/// so it cannot spill into the shell after the process exits.
pub fn read_key_stdin() -> Result<String, StartupError> {
    if io::stdin().is_terminal() {
        read_line_hidden()
    } else {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            let line = line.map_err(|e| StartupError::usage(format!("cannot read stdin: {e}")))?;
            if !line.trim().is_empty() {
                return Ok(line);
            }
        }
        Err(StartupError::auth(
            "private key is empty: stdin had no input",
        ))
    }
}

/// Read one line with the terminal echo off, then discard whatever else the
/// paste carried until the input goes idle.
pub fn read_line_hidden() -> Result<String, StartupError> {
    crossterm::terminal::enable_raw_mode()
        .map_err(|e| StartupError::other(format!("cannot enter raw mode: {e}")))?;
    let mut buffer: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    let outcome: Result<(), StartupError> = loop {
        match io::stdin().read(&mut byte) {
            Ok(0) => break Ok(()),
            Ok(_) => match byte[0] {
                b'\n' | b'\r' => break Ok(()),
                // Raw mode hands Ctrl-C to the program instead of the
                // terminal; treat it as the user cancelling.
                3 => {
                    break Err(StartupError::usage("cancelled"));
                }
                127 | 8 => {
                    buffer.pop();
                }
                _ => buffer.push(byte[0]),
            },
            Err(e) => break Err(StartupError::usage(format!("cannot read input: {e}"))),
        }
    };
    // Drain the rest of a paste so it does not run in the shell afterwards.
    while outcome.is_ok() && crossterm::event::poll(DRAIN_IDLE_MS).unwrap_or(false) {
        let _ = crossterm::event::read();
    }
    crossterm::terminal::disable_raw_mode()
        .map_err(|e| StartupError::other(format!("cannot leave raw mode: {e}")))?;
    let _ = writeln!(io::stdout());
    outcome.map(|_| String::from_utf8_lossy(&buffer).trim().to_string())
}

fn read_line_visible(prompt: &str) -> Result<String, StartupError> {
    print!("{prompt}");
    let _ = io::stdout().flush();
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|e| StartupError::usage(format!("cannot read input: {e}")))?;
    Ok(line.trim().to_owned())
}

/// The wizard's method menu.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Method {
    Scan,
    Key,
    File,
}

pub fn parse_choice(raw: &str) -> Option<Method> {
    match raw.trim() {
        "1" => Some(Method::Scan),
        "2" => Some(Method::Key),
        "3" => Some(Method::File),
        _ => None,
    }
}

/// A `y` answer overwrites; anything else, including an empty line, keeps the
/// current login. The default is the safe one.
pub fn parse_yes(raw: &str) -> bool {
    matches!(raw.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Verify the identity the way a session will use it: one NIP-42 WebSocket
/// connection. Unreachable is code 2; a refusal of the identity is code 3.
pub fn verify_online(
    http_url: &str,
    keys: &Keys,
    auth_tag: Option<&str>,
) -> Result<(), StartupError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| StartupError::other(format!("cannot start runtime: {e}")))?;
    runtime.block_on(async {
        let (_, ws_url) = config::split_relay_url(http_url).expect("a resolved URL always splits");
        let tag = auth_tag
            .map(buzz_sdk::nip_oa::parse_auth_tag)
            .transpose()
            .map_err(|e| StartupError::auth(format!("auth tag is malformed: {e}")))?;
        match NostrWsConnection::connect_authenticated(&ws_url, keys, tag.as_ref()).await {
            Ok(_) => Ok(()),
            Err(WsClientError::AuthFailed(reason)) => Err(StartupError::auth(format!(
                "relay refused the identity {}: {reason}",
                config::npub_short(keys)
            ))),
            Err(e) => Err(StartupError {
                code: config::EXIT_NETWORK,
                message: format!("cannot reach relay {ws_url}: {e}"),
            }),
        }
    })
}

/// `buzzx login`. Returns the process exit code.
pub fn run(cli: LoginCli) -> i32 {
    match login(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("buzzx: {e}");
            e.code
        }
    }
}

fn login(cli: LoginCli) -> Result<i32, StartupError> {
    if cli.key_stdin && (cli.key_file.is_some() || cli.flag_key.is_some()) {
        return Err(StartupError::usage(
            "--private-key-stdin cannot be combined with --private-key or --private-key-file",
        ));
    }

    let path = config::config_path();
    let existing = if path.is_file() {
        Some(config::read_config_file(&path)?)
    } else {
        None
    };

    let tty = io::stdin().is_terminal();
    let plan = plan_input(
        cli.flag_key.as_deref(),
        cli.key_file.as_deref(),
        cli.key_stdin,
        std::env::var("BUZZ_PRIVATE_KEY").is_ok(),
        tty,
    );
    let (key_text, from_wizard) = match plan {
        InputPlan::RefuseNoInput => {
            return Err(StartupError::usage(
                "no identity: pass --private-key, --private-key-file, or --private-key-stdin, \
                 set BUZZ_PRIVATE_KEY, or run interactively to use the wizard",
            ));
        }
        InputPlan::Use(KeyInput::Flag(key)) => {
            eprintln!(
                "note: a key on the command line stays in shell history and process listings; \
                 prefer --private-key-file or --private-key-stdin"
            );
            (key, false)
        }
        InputPlan::Use(KeyInput::Stdin) => (read_key_stdin()?, false),
        InputPlan::Use(KeyInput::File(path)) => (read_key_file(&path)?, false),
        InputPlan::Use(KeyInput::Wizard) => wizard()?,
        InputPlan::Use(KeyInput::Env) => {
            let key = std::env::var("BUZZ_PRIVATE_KEY").unwrap_or_default();
            (key, false)
        }
    };

    let keys = validate_key(&key_text)?;
    let short = config::npub_short(&keys);

    // Relay precedence for login matches the runtime contract: flag, env,
    // existing file, default. The wizard may still change it.
    let mut relay = cli
        .flag_relay
        .clone()
        .or_else(|| std::env::var("BUZZ_RELAY_URL").ok())
        .or_else(|| existing.as_ref().and_then(|f| f.relay_url.clone()))
        .unwrap_or_else(|| "http://localhost:3000".to_owned());
    if from_wizard {
        let answer = read_line_visible(&format!("? Relay [{relay}] "))?;
        if !answer.is_empty() {
            relay = answer;
        }
    }
    let (http_url, _) = config::split_relay_url(&relay)?;

    // A login that keeps an auth tag must verify against the new identity;
    // refusing here is fail-fast, the same contract resolve() applies.
    let auth_tag = cli
        .flag_auth_tag
        .clone()
        .or_else(|| std::env::var("BUZZ_AUTH_TAG").ok())
        .or_else(|| existing.as_ref().and_then(|f| f.auth_tag.clone()));
    if let Some(tag) = &auth_tag {
        buzz_sdk::nip_oa::verify_auth_tag(tag, &keys.public_key()).map_err(|e| {
            StartupError::auth(format!("auth tag does not verify for {short}: {e}"))
        })?;
    }

    if let Some(old) = &existing
        && !confirm_overwrite(old, &http_url, &short, tty)?
    {
        println!("login cancelled; {} is unchanged", path.display());
        return Ok(config::EXIT_USAGE);
    }

    verify_online(&http_url, &keys, auth_tag.as_deref())?;

    config::replace_at(&path, &http_url, key_text.trim(), auth_tag.as_deref())?;
    let mode = std::fs::metadata(&path)
        .map(|m| m.permissions())
        .map(|p| format!("{:o}", p.mode() & 0o777))
        .unwrap_or_else(|_| "600".to_owned());
    println!("identity verified: {short}");
    println!("saved to {} ({mode})", path.display());
    println!("next: buzzx tui");
    Ok(0)
}

/// An existing config is never replaced silently. On a terminal the user sees
/// both identities and answers; without one, there is no way to confirm, so
/// the login is refused.
fn confirm_overwrite(
    old: &ConfigFile,
    new_relay: &str,
    new_short: &str,
    tty: bool,
) -> Result<bool, StartupError> {
    // The old file may hold a key that no longer parses; the login must still
    // be replaceable, so an unreadable identity is shown as such, not fatal.
    let old_key = match old.private_key.as_deref() {
        Some(text) => validate_key(text)
            .map(|k| config::npub_short(&k))
            .unwrap_or_else(|_| "<unreadable>".to_owned()),
        None => "<none>".to_owned(),
    };
    println!(
        "current login: {old_key} on {}",
        old.relay_url.as_deref().unwrap_or("<default>")
    );
    println!("new login:     {new_short} on {new_relay}");
    if !tty {
        return Err(StartupError::usage(
            "a config file already exists; run buzzx login on a terminal to confirm the overwrite",
        ));
    }
    let answer = read_line_visible("overwrite the current login? [y/N] ")?;
    Ok(parse_yes(&answer))
}

/// The interactive first-run path: pick a method, then supply the key.
/// Scan is listed because it is the intended future default, and answered
/// honestly: no mobile remote-signing protocol is confirmed, so it cannot
/// run yet. No private transport is improvised in its place.
fn wizard() -> Result<(String, bool), StartupError> {
    loop {
        println!("? Login method");
        println!("  1. Scan a QR code from Buzz Mobile");
        println!("  2. Enter a private key");
        println!("  3. Load a private key from a file");
        let choice = read_line_visible("  method [1-3]: ")?;
        match parse_choice(&choice) {
            Some(Method::Scan) => {
                println!(
                    "  scan login is not available yet: Buzz Mobile has no confirmed \
                     remote-signing protocol; use method 2 or 3"
                );
            }
            Some(Method::Key) => {
                println!("  the input is hidden; nothing is echoed");
                let key = read_line_hidden()?;
                if key.is_empty() {
                    println!("  no input; nothing entered");
                    continue;
                }
                return Ok((key, true));
            }
            Some(Method::File) => {
                let path = read_line_visible("  key file path: ")?;
                if path.is_empty() {
                    println!("  no path given");
                    continue;
                }
                let key = read_key_file(Path::new(&path))?;
                return Ok((key, true));
            }
            None => println!("  choose 1, 2, or 3"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::nips::nip19::ToBech32;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn fixed_key() -> String {
        let secret = nostr::SecretKey::from_slice(&[7u8; 32]).expect("a fixed test key");
        Keys::new(secret).secret_key().to_secret_hex()
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("buzzx-login-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn validate_accepts_hex_and_nsec_and_trims() {
        let keys = Keys::generate();
        assert_eq!(validate_key(&fixed_key()).unwrap().public_key(), {
            let secret = nostr::SecretKey::from_slice(&[7u8; 32]).expect("a fixed test key");
            Keys::new(secret).public_key()
        });
        let nsec = keys.secret_key().to_bech32().expect("nsec form");
        assert_eq!(
            validate_key(&format!("  {nsec}\n")).unwrap().public_key(),
            keys.public_key()
        );
    }

    #[test]
    fn validate_refuses_empty_and_garbage_without_echoing_them() {
        for bad in ["", "   \n\t", "not-a-key", "nsec1vanitygarbage"] {
            let err = validate_key(bad).unwrap_err();
            assert_eq!(err.code, crate::config::EXIT_AUTH, "{bad:?}: {err}");
            let visible = bad.trim();
            if visible.is_empty() {
                assert_eq!(err.message, "private key is empty");
            } else {
                assert!(!err.message.contains(visible), "leaks input: {err}");
            }
        }
    }

    #[test]
    fn key_file_requires_owner_only_permissions_before_reading() {
        let dir = temp_dir("file");
        let path = dir.join("work.nsec");
        fs::write(&path, fixed_key()).expect("write");

        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
        assert_eq!(read_key_file(&path).unwrap(), fixed_key());

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("chmod");
        let err = read_key_file(&path).unwrap_err();
        assert_eq!(err.code, crate::config::EXIT_AUTH);
        assert!(err.message.contains("chmod 600"), "{err}");

        let err = read_key_file(&dir.join("missing.nsec")).unwrap_err();
        assert_eq!(err.code, crate::config::EXIT_USAGE);
        assert!(!err.message.contains(&fixed_key()));

        let err = read_key_file(&dir).unwrap_err();
        assert_eq!(
            err.code,
            crate::config::EXIT_USAGE,
            "a directory is not a key file"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn input_plan_prefers_explicit_sources_then_env_then_wizard() {
        use InputPlan::*;
        use KeyInput::*;

        assert_eq!(
            plan_input(Some("k"), None, false, true, true),
            Use(Flag("k".into()))
        );
        assert_eq!(
            plan_input(None, Some(Path::new("/k.nsec")), false, true, true),
            Use(File(PathBuf::from("/k.nsec")))
        );
        assert_eq!(plan_input(None, None, true, true, true), Use(Stdin));
        assert_eq!(plan_input(None, None, false, true, false), Use(Env));
        assert_eq!(plan_input(None, None, false, false, true), Use(Wizard));
        assert_eq!(plan_input(None, None, false, false, false), RefuseNoInput);
    }

    #[test]
    fn yes_means_overwrite_and_everything_else_keeps_the_login() {
        assert!(parse_yes("y"));
        assert!(parse_yes(" Yes "));
        assert!(!parse_yes(""));
        assert!(!parse_yes("n"));
        assert!(!parse_yes("no"));
        assert!(!parse_yes("why"));
    }

    #[test]
    fn menu_choice_maps_numbers_to_methods() {
        assert_eq!(parse_choice("1"), Some(Method::Scan));
        assert_eq!(parse_choice(" 2"), Some(Method::Key));
        assert_eq!(parse_choice("3\n"), Some(Method::File));
        assert_eq!(parse_choice("4"), None);
        assert_eq!(parse_choice("scan"), None);
    }

    #[test]
    fn npub_short_keeps_head_and_tail_only() {
        let keys = Keys::parse(&fixed_key()).expect("key");
        let short = crate::config::npub_short(&keys);
        let npub = keys.public_key().to_bech32().expect("npub");
        assert!(short.starts_with("npub1"), "{short}");
        assert!(npub.starts_with(&short[..8]), "{short} vs {npub}");
        assert!(
            npub.ends_with(&short[short.len() - 4..]),
            "{short} vs {npub}"
        );
        assert_eq!(short.len(), 15, "{short}");
        assert!(!short.contains(&fixed_key()));
    }

    #[test]
    fn replace_writes_tight_permissions_and_replaces_existing() {
        let dir = temp_dir("replace");
        let path = dir.join("config.toml");
        let other = Keys::generate().secret_key().to_secret_hex();

        let written =
            crate::config::replace_at(&path, "https://relay.example/", &fixed_key(), None)
                .expect("first write");
        assert_eq!(written, path);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        crate::config::replace_at(&path, "http://other.example", &other, None).expect("replace");
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("http://other.example"), "{body}");
        assert!(body.contains(&other), "{body}");
        assert!(!body.contains(&fixed_key()), "{body}");
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers.len(), 1, "no temp file survives: {leftovers:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_failure_leaves_the_old_file_intact() {
        let dir = temp_dir("fail");
        let path = dir.join("config.toml");
        fs::write(&path, "relay_url = \"http://old\"\n").expect("old config");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).expect("lock dir");

        // A root process ignores directory permissions; the failure path
        // cannot be forced there, so the contract is only checked where the
        // permission holds.
        let probe = dir.join("probe");
        let can_force = fs::write(&probe, b"x").is_ok();
        let _ = fs::remove_file(&probe);

        if can_force {
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("unlock");
            let _ = fs::remove_dir_all(&dir);
            return;
        }

        let other = Keys::generate().secret_key().to_secret_hex();
        let err = crate::config::replace_at(&path, "http://new", &other, None)
            .expect_err("write blocked");
        assert_eq!(err.code, crate::config::EXIT_OTHER, "{err}");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "relay_url = \"http://old\"\n",
            "the old file is untouched"
        );
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("unlock");
        let _ = fs::remove_dir_all(&dir);
    }
}
