//! `buzzx whoami` and `buzzx logout`: read and clear the local login. Both
//! are offline commands; neither contacts the relay. The contract is
//! docs/configuration.md: whoami names the effective identity, relay, and
//! the source each won from, with no secret anywhere; logout removes the
//! saved private key and auth tag while the relay preference survives.

use std::io::IsTerminal;

use crate::config::{self, ConfigFile, StartupError};

/// What `logout` decided to do, before any I/O. Separated so the decision
/// is testable without a terminal or a config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoutPlan {
    /// A key override (flag or environment) is in effect. Logout cannot
    /// remove it, so it refuses rather than report a logged-out state that
    /// is not real.
    RefuseOverride,
    /// No local login material exists; logout is already complete.
    AlreadyOut,
    /// No terminal and no explicit `--yes`; the file is not touched.
    NeedsYes,
    /// A terminal can ask; `--yes` was not given.
    Ask,
    /// Explicit consent; clear the file.
    Clear,
}

/// The override check comes first: a logout that succeeded while an
/// external key still drives the session would be a lie the user cannot
/// see. A missing local login is success, not an error, so the plan tests
/// it before the confirmation modes.
pub fn plan_logout(override_key: bool, has_local_login: bool, yes: bool, tty: bool) -> LogoutPlan {
    if override_key {
        LogoutPlan::RefuseOverride
    } else if !has_local_login {
        LogoutPlan::AlreadyOut
    } else if yes {
        LogoutPlan::Clear
    } else if tty {
        LogoutPlan::Ask
    } else {
        LogoutPlan::NeedsYes
    }
}

/// Does the file still hold anything logout removes?
pub fn has_login_material(file: &ConfigFile) -> bool {
    file.private_key.is_some() || file.auth_tag.is_some()
}

/// The whoami report: identity, relay, and the source each won from. No
/// secret appears in it.
pub fn whoami_report(
    short: &str,
    relay: &str,
    key: config::KeySource,
    relay_source: config::RelaySource,
) -> String {
    format!(
        "identity: {short} (from {})\nrelay:    {relay} (from {})",
        key.label(),
        relay_source.label()
    )
}

/// `buzzx whoami`: local resolution only, the same precedence every
/// subcommand uses. Returns the process exit code.
pub fn run_whoami(
    flag_key: Option<&str>,
    flag_relay: Option<&str>,
    flag_auth_tag: Option<&str>,
) -> i32 {
    match config::resolve(flag_key, flag_relay, flag_auth_tag) {
        Ok(resolved) => {
            println!(
                "{}",
                whoami_report(
                    &config::npub_short(&resolved.keys),
                    &resolved.http_url,
                    resolved.sources.key,
                    resolved.sources.relay,
                )
            );
            0
        }
        Err(e) => {
            eprintln!("buzzx: {e}");
            e.code
        }
    }
}

/// Everything `buzzx logout` needs from the command line and the
/// environment.
#[derive(Debug, Default)]
pub struct LogoutCli {
    pub yes: bool,
    /// A `--private-key` flag on this invocation.
    pub flag_key: Option<String>,
    /// `BUZZ_PRIVATE_KEY` set in the environment.
    pub env_key: bool,
}

/// `buzzx logout`. Returns the process exit code.
pub fn run_logout(cli: LogoutCli) -> i32 {
    match logout(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("buzzx: {e}");
            e.code
        }
    }
}

fn logout(cli: LogoutCli) -> Result<i32, StartupError> {
    let path = config::config_path();
    let file = if path.is_file() {
        Some(config::read_config_file(&path)?)
    } else {
        None
    };
    let plan = plan_logout(
        cli.flag_key.is_some() || cli.env_key,
        file.as_ref().is_some_and(has_login_material),
        cli.yes,
        std::io::stdin().is_terminal(),
    );

    match plan {
        LogoutPlan::RefuseOverride => Err(StartupError::usage(
            "an identity override from --private-key or BUZZ_PRIVATE_KEY is in effect; \
             remove it before logging out",
        )),
        LogoutPlan::AlreadyOut => {
            println!("no local login material; already logged out");
            Ok(0)
        }
        LogoutPlan::NeedsYes => Err(StartupError::usage(
            "logout needs --yes to run without a terminal",
        )),
        LogoutPlan::Ask | LogoutPlan::Clear => {
            let file = file.expect("Ask and Clear imply a local login");
            // The saved key may no longer parse; the login must still be
            // removable, so an unreadable identity is shown as such.
            let short = match file.private_key.as_deref() {
                Some(text) => crate::login::validate_key(text)
                    .map(|k| config::npub_short(&k))
                    .unwrap_or_else(|_| "<unreadable>".to_owned()),
                None => "<none>".to_owned(),
            };
            let relay = file.relay_url.as_deref().unwrap_or("<default>");
            println!("login to remove: {short} on {relay}");
            if plan == LogoutPlan::Ask
                && !crate::login::parse_yes(&crate::login::read_line_visible(
                    "log out and remove the saved key and auth tag? [y/N] ",
                )?)
            {
                println!("logout cancelled; {} is unchanged", path.display());
                return Ok(config::EXIT_USAGE);
            }
            config::clear_login_at(&path, file.relay_url.as_deref())?;
            println!(
                "logged out: removed the saved credentials from {}",
                path.display()
            );
            match file.relay_url.as_deref() {
                Some(relay) => println!("relay preference kept: {relay}"),
                None => println!("no relay preference was saved"),
            }
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logout_refuses_an_override_key_before_anything_else() {
        assert_eq!(
            plan_logout(true, true, true, true),
            LogoutPlan::RefuseOverride
        );
        assert_eq!(
            plan_logout(true, false, false, false),
            LogoutPlan::RefuseOverride
        );
    }

    #[test]
    fn logout_without_local_material_is_already_out() {
        assert_eq!(
            plan_logout(false, false, true, false),
            LogoutPlan::AlreadyOut
        );
        assert_eq!(
            plan_logout(false, false, false, true),
            LogoutPlan::AlreadyOut
        );
    }

    #[test]
    fn logout_without_a_terminal_needs_yes() {
        assert_eq!(plan_logout(false, true, false, false), LogoutPlan::NeedsYes);
    }

    #[test]
    fn logout_confirms_on_a_terminal_and_clears_on_yes() {
        assert_eq!(plan_logout(false, true, false, true), LogoutPlan::Ask);
        assert_eq!(plan_logout(false, true, true, false), LogoutPlan::Clear);
        assert_eq!(plan_logout(false, true, true, true), LogoutPlan::Clear);
    }

    #[test]
    fn login_material_is_a_key_or_an_auth_tag() {
        let tag_only = ConfigFile {
            relay_url: None,
            private_key: None,
            auth_tag: Some("x".into()),
        };
        let relay_only = ConfigFile {
            relay_url: Some("http://relay".into()),
            private_key: None,
            auth_tag: None,
        };
        assert!(has_login_material(&tag_only));
        assert!(!has_login_material(&relay_only));
    }

    #[test]
    fn whoami_report_names_both_sources() {
        let report = whoami_report(
            "npub1ab...wxyz",
            "https://relay.example",
            config::KeySource::File,
            config::RelaySource::Env,
        );
        assert!(report.contains("npub1ab...wxyz"), "{report}");
        assert!(report.contains("https://relay.example"), "{report}");
        assert!(report.contains("(from file)"), "{report}");
        assert!(report.contains("(from env)"), "{report}");
    }
}
